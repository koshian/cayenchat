//! Services accounts of the users we share a channel with (opt-in per
//! server as "User accounts"): IRCv3 `extended-join`, `account-notify` and,
//! for the users already present when we join, one WHOX query per channel.
//!
//! State is IRC-specific and lives in the connection's worker, so a
//! reconnect starts empty. A user is remembered only while they share a
//! channel with us and only if something is known about them (an account,
//! or a real name from `extended-join`/WHOX). Accounts belong to the server
//! this connection is to and are never compared across networks.
//!
//! - `JOIN #chan account :realname` (extended-join; `*` = not logged in)
//!   records both.
//! - `ACCOUNT name` / `ACCOUNT *` (account-notify) login, logout, change.
//! - After our own JOIN, when the server announces the `WHOX` ISUPPORT
//!   token, `WHO #chan %tnar,<token>` asks for the account (`0` = none) and
//!   real name of everybody in the channel. One channel at a time, an
//!   unanswered one is dropped after [`WHO_TIMEOUT`]; no polling. Without
//!   WHOX nothing is asked (the specification's plain fallback needs the
//!   `a` token, which only WHOX has), so users who were there before us
//!   learn their account from `ACCOUNT` or a later join.
//! - PART, KICK and QUIT forget users who no longer share a channel; NICK
//!   moves the entry; our own PART or KICK removes the channel from all.
//!
//! Every change is reported as [`Event::UserAccount`] or
//! [`Event::UserAccountForgotten`], so the application can mirror it.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Duration,
};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Response};
use tokio::time::Instant;

use crate::{Event, display_nickname, text::nickname_key};

/// Users remembered per connection; more are ignored.
const MAX_USERS: usize = 4096;
/// Channels waiting for their WHOX query.
const MAX_QUEUED_WHO: usize = 64;
/// A WHOX query without an end-of-WHO is given up after this long.
const WHO_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest real name kept, in bytes.
const MAX_REALNAME_BYTES: usize = 256;
/// Longest account name kept, in bytes.
const MAX_ACCOUNT_BYTES: usize = 128;

#[derive(Debug, Default)]
struct Known {
    nickname: String,
    account: Option<String>,
    realname: Option<String>,
    /// Casemapped names of the channels shared with us.
    channels: HashSet<String>,
}

#[derive(Debug)]
struct Outstanding {
    channel: String,
    token: String,
    sent: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct Accounts {
    users: HashMap<String, Known>,
    whox: bool,
    queue: VecDeque<String>,
    outstanding: Option<Outstanding>,
    token: u16,
}

fn account_value(value: &str) -> Option<String> {
    (!value.is_empty() && value != "*" && value.len() <= MAX_ACCOUNT_BYTES)
        .then(|| value.to_owned())
}

fn realname_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| {
        let mut end = value.len().min(MAX_REALNAME_BYTES);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value[..end].to_owned()
    })
}

impl Accounts {
    /// Reads the `WHOX` token from RPL_ISUPPORT (and its removal).
    pub(crate) fn isupport(&mut self, message: &IrcMessage) {
        let IrcCommand::Response(Response::RPL_ISUPPORT, args) = &message.command else {
            return;
        };
        for token in args.iter().skip(1).flat_map(|arg| arg.split_whitespace()) {
            if token.eq_ignore_ascii_case("WHOX") {
                self.whox = true;
            } else if token.eq_ignore_ascii_case("-WHOX") {
                self.whox = false;
            }
        }
    }

    fn report(&self, key: &str) -> Event {
        let known = &self.users[key];
        Event::UserAccount {
            nickname: known.nickname.clone(),
            account: known.account.clone(),
            realname: known.realname.clone(),
        }
    }

    /// Records what is known about `nickname` in `channels`. Reports only a
    /// change. Nothing is kept for a user we do not share a channel with.
    fn learn(
        &mut self,
        nickname: &str,
        account: Option<Option<String>>,
        realname: Option<String>,
        channels: impl IntoIterator<Item = String>,
        events: &mut Vec<Event>,
    ) {
        let key = nickname_key(nickname);
        let channels: Vec<String> = channels.into_iter().collect();
        if !self.users.contains_key(&key) {
            if channels.is_empty() || self.users.len() >= MAX_USERS {
                return;
            }
            // An unknown user with nothing to record is not worth a slot.
            if account.as_ref().is_none_or(Option::is_none) && realname.is_none() {
                return;
            }
        }
        let known = self.users.entry(key.clone()).or_default();
        let before = (known.account.clone(), known.realname.clone());
        let fresh = known.nickname.is_empty();
        known.nickname = nickname.to_owned();
        if let Some(account) = account {
            known.account = account;
        }
        if realname.is_some() {
            known.realname = realname;
        }
        known.channels.extend(channels);
        if fresh || before != (known.account.clone(), known.realname.clone()) {
            events.push(self.report(&key));
        }
    }

    fn forget(&mut self, key: &str, events: &mut Vec<Event>) {
        if let Some(known) = self.users.remove(key) {
            events.push(Event::UserAccountForgotten {
                nickname: known.nickname,
            });
        }
    }

    fn leave(&mut self, key: &str, channel: &str, events: &mut Vec<Event>) {
        let Some(known) = self.users.get_mut(key) else {
            return;
        };
        known.channels.remove(channel);
        if known.channels.is_empty() {
            self.forget(key, events);
        }
    }

    /// Channels of `roster` (name → members with their prefixes) holding
    /// `nickname`.
    fn shared_channels(nickname: &str, roster: &HashMap<String, Vec<String>>) -> Vec<String> {
        roster
            .iter()
            .filter(|(_, members)| {
                members
                    .iter()
                    .any(|member| crate::text::same_nickname(display_nickname(member), nickname))
            })
            .map(|(channel, _)| nickname_key(channel))
            .collect()
    }

    /// Follows JOIN, PART, KICK, QUIT, NICK and ACCOUNT. `roster` is the
    /// last published membership of each joined channel.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        roster: &HashMap<String, Vec<String>>,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        let source = message.source_nickname();
        match &message.command {
            // `JOIN #chan account :realname`: irc-proto fills the key and
            // real-name slots with the last two parameters.
            IrcCommand::JOIN(channel, account, realname) => {
                let Some(nickname) = source else {
                    return events;
                };
                let (account, realname) = match (account, realname) {
                    (Some(account), Some(realname)) => {
                        (Some(account_value(account)), realname_value(realname))
                    }
                    _ => (None, None),
                };
                if account.is_some() || realname.is_some() {
                    self.learn(
                        nickname,
                        account,
                        realname,
                        [nickname_key(channel)],
                        &mut events,
                    );
                } else if let Some(known) = self.users.get_mut(&nickname_key(nickname)) {
                    known.channels.insert(nickname_key(channel));
                }
            }
            IrcCommand::PART(channel, _) => {
                let Some(nickname) = source else {
                    return events;
                };
                if crate::text::same_nickname(nickname, current_nick) {
                    self.forget_channel(channel, &mut events);
                } else {
                    self.leave(&nickname_key(nickname), &nickname_key(channel), &mut events);
                }
            }
            IrcCommand::KICK(channel, nickname, _) => {
                if crate::text::same_nickname(nickname, current_nick) {
                    self.forget_channel(channel, &mut events);
                } else {
                    self.leave(&nickname_key(nickname), &nickname_key(channel), &mut events);
                }
            }
            IrcCommand::QUIT(_) => {
                if let Some(nickname) = source {
                    self.forget(&nickname_key(nickname), &mut events);
                }
            }
            IrcCommand::NICK(new_nick) => {
                if let Some(old) = source {
                    let old_key = nickname_key(old);
                    if let Some(mut known) = self.users.remove(&old_key) {
                        events.push(Event::UserAccountForgotten {
                            nickname: known.nickname.clone(),
                        });
                        known.nickname = new_nick.clone();
                        let key = nickname_key(new_nick);
                        self.users.insert(key.clone(), known);
                        events.push(self.report(&key));
                    }
                }
            }
            IrcCommand::ACCOUNT(account) => {
                if let Some(nickname) = source {
                    let channels = Self::shared_channels(nickname, roster);
                    let account = account_value(account);
                    // A logout of a user we know nothing else about leaves
                    // nothing to keep (`learn` skips it); a known user
                    // reports the change.
                    self.learn(nickname, Some(account), None, channels, &mut events);
                }
            }
            _ => {}
        }
        events
    }

    fn forget_channel(&mut self, channel: &str, events: &mut Vec<Event>) {
        let key = nickname_key(channel);
        self.queue.retain(|queued| nickname_key(queued) != key);
        if self
            .outstanding
            .as_ref()
            .is_some_and(|o| nickname_key(&o.channel) == key)
        {
            self.outstanding = None;
        }
        let keys: Vec<String> = self.users.keys().cloned().collect();
        for user in keys {
            self.leave(&user, &key, events);
        }
    }

    /// A channel's member list was published: users who are no longer in
    /// it lose the channel, users who are in it and known gain it.
    pub(crate) fn names(&mut self, channel: &str, users: &[String]) -> Vec<Event> {
        let mut events = Vec::new();
        let key = nickname_key(channel);
        let present: HashSet<String> = users
            .iter()
            .map(|user| nickname_key(display_nickname(user)))
            .collect();
        let keys: Vec<String> = self.users.keys().cloned().collect();
        for user in keys {
            if present.contains(&user) {
                if let Some(known) = self.users.get_mut(&user) {
                    known.channels.insert(key.clone());
                }
            } else if self.users[&user].channels.contains(&key) {
                self.leave(&user, &key, &mut events);
            }
        }
        events
    }

    /// We joined `channel`: its members' accounts are asked for.
    pub(crate) fn joined(&mut self, channel: &str) {
        if !self.whox || self.queue.len() >= MAX_QUEUED_WHO {
            return;
        }
        let key = nickname_key(channel);
        let busy = self
            .outstanding
            .as_ref()
            .is_some_and(|o| nickname_key(&o.channel) == key);
        if !busy && !self.queue.iter().any(|queued| nickname_key(queued) == key) {
            self.queue.push_back(channel.to_owned());
        }
    }

    /// The next WHOX query, when none is waiting for its answer.
    pub(crate) fn next_who(&mut self, now: Instant) -> Option<IrcCommand> {
        if self
            .outstanding
            .as_ref()
            .is_some_and(|o| now.duration_since(o.sent) >= WHO_TIMEOUT)
        {
            self.outstanding = None;
        }
        if self.outstanding.is_some() {
            return None;
        }
        let channel = self.queue.pop_front()?;
        self.token = self.token % 999 + 1;
        let token = self.token.to_string();
        let command = IrcCommand::Raw(
            "WHO".into(),
            vec![channel.clone(), format!("%tnar,{token}")],
        );
        self.outstanding = Some(Outstanding {
            channel,
            token,
            sent: now,
        });
        Some(command)
    }

    /// `Some` when the message is part of our WHOX reply (354 with our
    /// token, or the end-of-WHO 315 of the channel asked); it is then
    /// consumed.
    pub(crate) fn observe_who(&mut self, message: &IrcMessage) -> Option<Vec<Event>> {
        let (code, args) = crate::metadata::numeric(message)?;
        let outstanding = self.outstanding.as_ref()?;
        let mut events = Vec::new();
        match code {
            // `354 me <token> <nick> <account|0> :<realname>`
            354 if args.get(1) == Some(&outstanding.token) => {
                let (Some(nickname), Some(account)) = (args.get(2), args.get(3)) else {
                    return Some(events);
                };
                let channel = nickname_key(&outstanding.channel);
                let account = (account != "0").then(|| account_value(account)).flatten();
                let realname = args.get(4).and_then(|real| realname_value(real));
                self.learn(nickname, Some(account), realname, [channel], &mut events);
                Some(events)
            }
            315 if args
                .get(1)
                .is_some_and(|name| crate::text::same_nickname(name, &outstanding.channel)) =>
            {
                self.outstanding = None;
                Some(events)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    fn roster(channels: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        channels
            .iter()
            .map(|(name, members)| {
                (
                    (*name).to_owned(),
                    members.iter().map(|m| (*m).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn shown(events: Vec<Event>) -> Vec<String> {
        events
            .into_iter()
            .map(|event| match event {
                Event::UserAccount {
                    nickname,
                    account,
                    realname,
                } => format!(
                    "{nickname} account={} real={}",
                    account.as_deref().unwrap_or("-"),
                    realname.as_deref().unwrap_or("-")
                ),
                Event::UserAccountForgotten { nickname } => format!("{nickname} gone"),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    fn feed(
        accounts: &mut Accounts,
        text: &str,
        roster: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        shown(accounts.observe(&line(text), "me", roster))
    }

    #[test]
    fn extended_join_records_the_account_and_the_real_name() {
        let mut accounts = Accounts::default();
        let none = HashMap::new();
        assert_eq!(
            feed(
                &mut accounts,
                ":alice!u@h JOIN #a alice-acct :Alice Liddell",
                &none
            ),
            ["alice account=alice-acct real=Alice Liddell"]
        );
        // `*` is not an account, but the real name is still kept.
        assert_eq!(
            feed(&mut accounts, ":bob!u@h JOIN #a * :Bob B", &none),
            ["bob account=- real=Bob B"]
        );
        // A plain JOIN of a known user only adds the channel.
        assert!(feed(&mut accounts, ":alice!u@h JOIN #b", &none).is_empty());
        assert!(accounts.users["alice"].channels.contains("#b"));
        // A plain JOIN of an unknown user records nothing.
        assert!(feed(&mut accounts, ":eve!u@h JOIN #a", &none).is_empty());
        assert!(!accounts.users.contains_key("eve"));
    }

    #[test]
    fn account_notify_follows_login_change_and_logout() {
        let mut accounts = Accounts::default();
        let members = roster(&[("#a", &["@alice", "bob"])]);
        assert_eq!(
            feed(&mut accounts, ":bob!u@h ACCOUNT bob1", &members),
            ["bob account=bob1 real=-"]
        );
        assert_eq!(
            feed(&mut accounts, ":bob!u@h ACCOUNT bob2", &members),
            ["bob account=bob2 real=-"]
        );
        assert!(feed(&mut accounts, ":bob!u@h ACCOUNT bob2", &members).is_empty());
        // Logout keeps the user (their real name may be known) with no account.
        accounts.users.get_mut("bob").unwrap().realname = Some("Bob".into());
        assert_eq!(
            feed(&mut accounts, ":bob!u@h ACCOUNT *", &members),
            ["bob account=- real=Bob"]
        );
        // Somebody we share no channel with is not remembered.
        assert!(feed(&mut accounts, ":zed!u@h ACCOUNT zed", &members).is_empty());
        assert!(!accounts.users.contains_key("zed"));
    }

    #[test]
    fn users_are_forgotten_when_they_no_longer_share_a_channel() {
        let mut accounts = Accounts::default();
        let none = HashMap::new();
        feed(&mut accounts, ":alice!u@h JOIN #a acct :A", &none);
        feed(&mut accounts, ":alice!u@h JOIN #b", &none);
        assert!(feed(&mut accounts, ":alice!u@h PART #a", &none).is_empty());
        assert_eq!(
            feed(&mut accounts, ":alice!u@h PART #B", &none),
            ["alice gone"]
        );
        feed(&mut accounts, ":alice!u@h JOIN #a acct :A", &none);
        assert_eq!(
            feed(&mut accounts, ":alice!u@h QUIT :bye", &none),
            ["alice gone"]
        );
        feed(&mut accounts, ":carol!u@h JOIN #a c :C", &none);
        assert_eq!(
            feed(&mut accounts, ":op!u@h KICK #a carol :out", &none),
            ["carol gone"]
        );
        // Leaving a channel ourselves drops whoever is left only there.
        feed(&mut accounts, ":dave!u@h JOIN #a d :D", &none);
        feed(&mut accounts, ":erin!u@h JOIN #a e :E", &none);
        feed(&mut accounts, ":erin!u@h JOIN #b", &none);
        let gone = feed(&mut accounts, ":me!u@h PART #a", &none);
        assert_eq!(gone, ["dave gone"]);
        assert!(accounts.users["erin"].channels.contains("#b"));
    }

    #[test]
    fn a_nick_change_keeps_the_identity_and_uses_irc_casemapping() {
        let mut accounts = Accounts::default();
        let none = HashMap::new();
        feed(&mut accounts, ":Al[x]!u@h JOIN #a acct :A", &none);
        assert_eq!(
            feed(&mut accounts, ":al{X}!u@h NICK Alice", &none),
            ["Al[x] gone", "Alice account=acct real=A"]
        );
        assert!(!accounts.users.contains_key("al{x}"));
        assert!(accounts.users.contains_key("alice"));
        // A user known under another case is one user.
        feed(
            &mut accounts,
            ":ALICE!u@h ACCOUNT other",
            &roster(&[("#a", &["Alice"])]),
        );
        assert_eq!(accounts.users.len(), 1);
        assert_eq!(accounts.users["alice"].account.as_deref(), Some("other"));
    }

    #[test]
    fn a_published_member_list_drops_users_who_left_unseen() {
        let mut accounts = Accounts::default();
        let none = HashMap::new();
        feed(&mut accounts, ":alice!u@h JOIN #a acct :A", &none);
        feed(&mut accounts, ":bob!u@h JOIN #a b :B", &none);
        let events = accounts.names("#A", &["@bob".to_owned()]);
        assert_eq!(shown(events), ["alice gone"]);
        assert!(accounts.names("#a", &["bob".to_owned()]).is_empty());
    }

    #[test]
    fn whox_asks_once_per_joined_channel_and_fills_the_state() {
        let mut accounts = Accounts::default();
        let now = Instant::now();
        // Without the WHOX token nothing is asked.
        accounts.joined("#a");
        assert!(accounts.next_who(now).is_none());
        accounts.isupport(&line(":srv 005 me WHOX CHATHISTORY=50 :are supported"));
        accounts.joined("#a");
        accounts.joined("#A");
        accounts.joined("#b");
        let asked = accounts.next_who(now).unwrap();
        assert_eq!(
            IrcMessage::from(asked).to_string().trim_end(),
            "WHO #a %tnar,1"
        );
        // One at a time.
        assert!(accounts.next_who(now).is_none());
        let mut reply = |text: &str| accounts.observe_who(&line(text));
        // Another client's WHOX reply (other token) is not ours.
        assert!(reply(":srv 354 me 77 zed zacct :Zed").is_none());
        let a = reply(":srv 354 me 1 alice alice-acct :Alice Liddell").unwrap();
        assert_eq!(shown(a), ["alice account=alice-acct real=Alice Liddell"]);
        let b = reply(":srv 354 me 1 bob 0 :Bob").unwrap();
        assert_eq!(shown(b), ["bob account=- real=Bob"]);
        assert!(
            reply(":srv 315 me #A :End of /WHO list")
                .unwrap()
                .is_empty()
        );
        let next = accounts.next_who(now).unwrap();
        assert_eq!(
            IrcMessage::from(next).to_string().trim_end(),
            "WHO #b %tnar,2"
        );
    }

    #[test]
    fn an_unanswered_query_is_dropped_and_a_channel_left_is_forgotten() {
        let mut accounts = Accounts::default();
        let now = Instant::now();
        accounts.isupport(&line(":srv 005 me WHOX :are supported"));
        accounts.joined("#a");
        accounts.joined("#b");
        accounts.next_who(now).unwrap();
        assert!(accounts.next_who(now + Duration::from_secs(5)).is_none());
        assert!(accounts.next_who(now + WHO_TIMEOUT).is_some(), "#b follows");
        // Leaving while queued or asked cancels it.
        accounts.joined("#c");
        accounts.observe(&line(":me!u@h PART #c"), "me", &HashMap::new());
        assert!(accounts.queue.is_empty());
    }

    #[test]
    fn memory_is_bounded() {
        let mut accounts = Accounts::default();
        let none = HashMap::new();
        for n in 0..MAX_USERS + 50 {
            accounts.observe(
                &line(&format!(":u{n}!u@h JOIN #a acct{n} :real")),
                "me",
                &none,
            );
        }
        assert_eq!(accounts.users.len(), MAX_USERS);
        accounts.isupport(&line(":srv 005 me WHOX :are supported"));
        for n in 0..MAX_QUEUED_WHO + 10 {
            accounts.joined(&format!("#c{n}"));
        }
        assert_eq!(accounts.queue.len(), MAX_QUEUED_WHO);
        let long = "x".repeat(1000);
        let mut events = Vec::new();
        accounts.learn(
            "big",
            None,
            realname_value(&long),
            ["#a".to_owned()],
            &mut events,
        );
        assert_eq!(
            accounts
                .users
                .get("big")
                .map(|k| k.realname.as_ref().map(String::len)),
            None,
            "full: not added"
        );
        assert!(realname_value(&long).unwrap().len() <= MAX_REALNAME_BYTES);
        assert!(account_value(&"a".repeat(200)).is_none());
    }
}
