//! Avatars exchanged between clients with KVIrc's CTCP AVATAR, for servers
//! without IRCv3 metadata (experimental, opt-in per server).
//!
//! Protocol followed (KVIrc 5.2 documentation `doc_ctcp_avatar.html` and
//! its source, `KviIrcServerParser_ctcp.cpp`, `KviIrcServerParser_numericHandlers.cpp`,
//! `KviIrcConnection.cpp` and `libkviavatar.cpp` on master, checked
//! 2026-09-28):
//!
//! - A client that has an avatar starts its realname with ETX (0x03), an
//!   ASCII digit `0`–`7` whose bit 2 (the fours place) is set, and SI
//!   (0x0F). KVIrc sends `\x034\x0f` in its `USER` command; the realname's
//!   text follows unchanged.
//! - `PRIVMSG <nick> :\x01AVATAR\x01` asks for it. KVIrc sends the query
//!   when a WHO reply shows the mark.
//! - The answer is `NOTICE <nick> :\x01AVATAR <file> <gender>\x01`, where
//!   `<gender>` is `M`, `F` or `?`; `avatar.notify` sends
//!   `\x01AVATAR <file>[ <size>]\x01` to a nickname or a channel without
//!   being asked. `<file>` is a URL or the name of a local file offered by
//!   DCC GET. Without an avatar the answer is `\x01AVATAR \x01` or
//!   `\x01AVATAR\x01`.
//!
//! Only URLs are used here: an `http://` or `https://` value becomes the
//! user's avatar reference (the media layer decides whether it may be
//! fetched); a file name, DCC, anything else or an empty value means the
//! user has no avatar we can show. No DCC is ever sent or accepted, and no
//! file transfer offer is made. The second field is ignored.
//!
//! Discovery is bounded. NAMES has no realname, and `extended-join` is not
//! negotiated, so marks are read from WHOIS (311) and WHO (352) replies,
//! and a user who speaks (a live message) in a channel we share, and whose
//! realname we have not seen, is looked up with one `WHO <nick>`. There is
//! no channel-wide WHO and no query to users without the mark. Lookups and
//! queries are deduplicated per user until the user leaves (or the
//! connection ends), queued up to a bound, sent one at a time at most every
//! two seconds with a few outstanding, and given up after a timeout.
//! Replies to our own lookups (and the server's errors for them) are kept
//! out of the server log; CTCP AVATAR traffic never makes chat rows, unread
//! marks, highlights, notifications or previews.
//!
//! Our avatar: when sharing is on, a private CTCP AVATAR query is answered
//! with the shared URL, rate-limited per user and in total. Channel queries
//! are not answered, and nothing is answered while nothing is shared. The
//! realname mark is decided at registration by the caller
//! ([`realname`]); the shared URL can change while connected.
//!
//! Server metadata wins: [`PeerAvatars`] keeps both references per user and
//! reports the metadata avatar when there is one, the peer one otherwise,
//! and passes metadata events through that merge. Avatars belong to the
//! user's presence: they move on NICK and end when the user quits or leaves
//! the last channel we share, and everything lives in the connection's
//! worker, so a reconnect starts empty.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Duration,
};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix};
use tokio::time::Instant;

use crate::{
    Event, display_nickname,
    metadata::{Handled, MAX_AVATAR_USERS, MAX_PUBLISHED_AVATAR_BYTES, avatar_value, numeric},
    text::{nickname_key, same_nickname},
    valid_channel,
};

const CTCP_TAG: &str = "AVATAR";
/// The realname mark's bit for "has an avatar".
const AVATAR_BIT: u8 = 4;
/// Our lookups and queries are spaced out by this much.
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// An unanswered lookup or query is given up after this; the user is not
/// asked again until they leave and come back.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// A server's `263 RPL_TRYAGAIN` for WHO pauses lookups this long.
const TRY_AGAIN_PAUSE: Duration = Duration::from_secs(30);
const MAX_QUEUED_PROBES: usize = 32;
const MAX_PROBES_IN_FLIGHT: usize = 4;
/// Users asked about per connection until they leave.
const MAX_PROBED_USERS: usize = MAX_AVATAR_USERS;
/// Answers to CTCP AVATAR queries: at most this many per window in total,
/// and one per user per [`REPLY_INTERVAL_PER_USER`].
const MAX_REPLIES_PER_WINDOW: usize = 5;
const REPLY_WINDOW: Duration = Duration::from_secs(10);
const REPLY_INTERVAL_PER_USER: Duration = Duration::from_secs(60);
const MAX_REPLY_RECIPIENTS: usize = 256;

/// The realname sent at registration, with KVIrc's avatar mark when we
/// share an avatar.
pub(crate) fn realname(base: &str, advertise: bool) -> String {
    if advertise {
        format!("\u{3}{AVATAR_BIT}\u{f}{base}")
    } else {
        base.to_owned()
    }
}

/// A realname without KVIrc's leading mark (ETX, a digit, SI), as the user
/// configured it.
pub(crate) fn without_mark(realname: &str) -> &str {
    match realname.as_bytes() {
        [0x03, b'0'..=b'7', 0x0f, ..] => &realname[3..],
        _ => realname,
    }
}

/// Whether a realname starts with KVIrc's mark for an avatar.
pub(crate) fn advertises_avatar(realname: &str) -> bool {
    match realname.as_bytes() {
        [0x03, digit @ b'0'..=b'7', 0x0f, ..] => (digit - b'0') & AVATAR_BIT != 0,
        _ => false,
    }
}

/// The parameters of a CTCP AVATAR message (after the tag), or `None` for
/// any other text. The closing 0x01 is optional, as in other clients.
fn avatar_ctcp(text: &str) -> Option<&str> {
    let body = text.strip_prefix('\u{1}')?;
    let body = body.strip_suffix('\u{1}').unwrap_or(body);
    let (tag, rest) = body.split_once(' ').unwrap_or((body, ""));
    tag.eq_ignore_ascii_case(CTCP_TAG).then_some(rest)
}

fn has_scheme(value: &str, scheme: &str) -> bool {
    value.len() > scheme.len()
        && value.is_char_boundary(scheme.len())
        && value[..scheme.len()].eq_ignore_ascii_case(scheme)
}

/// The avatar URL in the parameters of a CTCP AVATAR answer or
/// announcement, or `None` when the user has no avatar we can use: empty,
/// a local file (offered by DCC), not HTTP(S), or not a usable value.
pub(crate) fn reply_url(params: &str, utf8: bool) -> Option<String> {
    let params = params.trim_start_matches(' ');
    // KVIrc quotes a parameter only when it is empty (`""`).
    let token = match params.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map_or(quoted, |(inside, _)| inside),
        None => params.split(' ').next().unwrap_or_default(),
    };
    // Escapes (`\040` and so on) only appear in file names.
    if token.contains('\\') || !(has_scheme(token, "http://") || has_scheme(token, "https://")) {
        return None;
    }
    avatar_value(token, utf8)
}

/// Checks a URL we are about to share: the rules for received values, one
/// IRC line, and HTTP(S). Whether it is acceptable is the caller's policy.
pub fn shareable(url: &str, utf8: bool) -> Result<(), String> {
    crate::publishable_avatar(url, utf8)?;
    if url.len() > MAX_PUBLISHED_AVATAR_BYTES || reply_url(url, utf8).as_deref() != Some(url) {
        return Err("Only an http or https avatar URL can be shared.".into());
    }
    Ok(())
}

/// Where each user's avatar comes from. The shown one is the metadata
/// avatar if there is one.
#[derive(Debug)]
struct Sources {
    nickname: String,
    metadata: Option<String>,
    peer: Option<String>,
}

impl Sources {
    fn shown(&self) -> Option<String> {
        self.metadata.clone().or_else(|| self.peer.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeKind {
    /// `WHO <nick>`, for the realname.
    Who,
    /// `PRIVMSG <nick> :\x01AVATAR\x01`.
    Query,
}

#[derive(Debug)]
struct Probe {
    kind: ProbeKind,
    nickname: String,
    key: String,
    /// When an outstanding probe is given up.
    deadline: Instant,
    /// The user left or changed name while it was out: its answer is
    /// swallowed but not used.
    stale: bool,
}

/// How far a user present now has been asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Looked up, or their realname was seen without the mark.
    Seen,
    /// Queried for their avatar (the mark was seen).
    Queried,
}

/// Per-connection state of CTCP AVATAR exchange; it exists only when the
/// server's option is on.
#[derive(Debug)]
pub(crate) struct PeerAvatars {
    utf8: bool,
    /// The URL answered to queries, if we share one.
    share: Option<String>,
    avatars: HashMap<String, Sources>,
    stages: HashMap<String, Stage>,
    queue: VecDeque<Probe>,
    in_flight: Vec<Probe>,
    next_probe: Option<Instant>,
    /// Users not asked about because the queue was full, since it last
    /// emptied; only the first is reported.
    skipped: usize,
    replies: VecDeque<Instant>,
    replied: HashMap<String, Instant>,
}

impl PeerAvatars {
    pub(crate) fn new(utf8: bool, share: Option<String>) -> Self {
        Self {
            utf8,
            share: share.filter(|url| shareable(url, utf8).is_ok()),
            avatars: HashMap::new(),
            stages: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: Vec::new(),
            next_probe: None,
            skipped: 0,
            replies: VecDeque::new(),
            replied: HashMap::new(),
        }
    }

    /// Changes the URL answered to queries; `None` stops answering. The
    /// realname mark sent at registration is not changed.
    pub(crate) fn set_share(&mut self, url: Option<String>) {
        self.share = url.filter(|url| shareable(url, self.utf8).is_ok());
    }

    /// Handles CTCP AVATAR traffic and replies to our lookups. `Some` means
    /// the message is consumed: it must not become a chat row or server
    /// line. WHOIS and other people's WHO replies are only read.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        rosters: &HashMap<String, Vec<String>>,
        replayed: bool,
        now: Instant,
    ) -> Option<Handled> {
        let sender = match &message.prefix {
            Some(Prefix::Nickname(nick, _, _)) => Some(nick.as_str()),
            _ => None,
        };
        match &message.command {
            IrcCommand::PRIVMSG(target, text) => {
                avatar_ctcp(text)?;
                let mut handled = Handled::default();
                if let Some(sender) = sender
                    && !replayed
                    && !same_nickname(sender, current_nick)
                    && same_nickname(target, current_nick)
                    && let Some(url) = self.share.clone()
                    && self.may_reply(sender, now)
                {
                    handled.send.push(IrcCommand::NOTICE(
                        sender.to_owned(),
                        format!("\u{1}{CTCP_TAG} {url}\u{1}"),
                    ));
                }
                Some(handled)
            }
            IrcCommand::NOTICE(target, text) => {
                let params = avatar_ctcp(text)?;
                let mut handled = Handled::default();
                let Some(sender) = sender.filter(|sender| {
                    !replayed && valid_sender(sender) && !same_nickname(sender, current_nick)
                }) else {
                    return Some(handled);
                };
                // Only the user present now under that name, as the server
                // names them: sent to us by someone we share a channel
                // with, or to a channel they are in.
                let present = if same_nickname(target, current_nick) {
                    shares(rosters, sender, None)
                } else {
                    valid_channel(target) && in_channel(rosters, target, sender)
                };
                if !present {
                    return Some(handled);
                }
                let key = nickname_key(sender);
                self.in_flight
                    .retain(|probe| probe.kind != ProbeKind::Query || probe.key != key);
                if self.stages.contains_key(&key) || self.stages.len() < MAX_PROBED_USERS {
                    self.stages.insert(key, Stage::Queried);
                }
                let url = reply_url(params, self.utf8);
                handled.events.extend(self.set_peer(sender, url));
                Some(handled)
            }
            _ => self.numeric_reply(message, current_nick, rosters, now),
        }
    }

    fn numeric_reply(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        rosters: &HashMap<String, Vec<String>>,
        now: Instant,
    ) -> Option<Handled> {
        let (code, args) = numeric(message)?;
        let ours = |kind: ProbeKind, nick: &str| {
            let key = nickname_key(nick);
            self.in_flight
                .iter()
                .position(|probe| probe.kind == kind && probe.key == key)
        };
        match code {
            // RPL_WHOREPLY: <me> <channel> <user> <host> <server> <nick>
            // <flags> :<hops> <realname>
            352 if args.len() >= 8 => {
                let nick = &args[5];
                let found = ours(ProbeKind::Who, nick);
                let stale = found.is_some_and(|index| self.in_flight[index].stale);
                if !stale {
                    let trailing = args.last().map(String::as_str).unwrap_or_default();
                    let real = trailing.split_once(' ').map_or("", |(_, real)| real);
                    self.realname_seen(nick, real, current_nick, rosters);
                }
                found.map(|_| Handled::default())
            }
            // RPL_ENDOFWHO: <me> <mask> :End of WHO
            315 => {
                let index = ours(ProbeKind::Who, args.get(1)?)?;
                self.in_flight.remove(index);
                Some(Handled::default())
            }
            // RPL_WHOISUSER: <me> <nick> <user> <host> * :<realname>. The
            // user asked for it; it is only read.
            311 if args.len() > 5 => {
                let real = args.last().map(String::as_str).unwrap_or_default();
                self.realname_seen(&args[1], real, current_nick, rosters);
                None
            }
            // ERR_NOSUCHNICK for a lookup or query of ours: they are gone.
            401 => {
                let nick = args.get(1)?;
                let key = nickname_key(nick);
                let before = self.in_flight.len();
                self.in_flight.retain(|probe| probe.key != key);
                (self.in_flight.len() != before).then(Handled::default)
            }
            // RPL_TRYAGAIN for WHO while ours are out: pause lookups.
            263 if args
                .get(1)
                .is_some_and(|command| command.eq_ignore_ascii_case("WHO"))
                && self
                    .in_flight
                    .iter()
                    .any(|probe| probe.kind == ProbeKind::Who) =>
            {
                self.in_flight.retain(|probe| probe.kind != ProbeKind::Who);
                self.next_probe = Some(now + TRY_AGAIN_PAUSE);
                Some(Handled {
                    notes: vec!["Server asked to retry WHO later; avatar lookups paused.".into()],
                    ..Handled::default()
                })
            }
            _ => None,
        }
    }

    /// A realname from WHO or WHOIS: a marked user present now is queried
    /// once, unless the server's metadata gave an avatar.
    fn realname_seen(
        &mut self,
        nick: &str,
        real: &str,
        current_nick: &str,
        rosters: &HashMap<String, Vec<String>>,
    ) {
        if same_nickname(nick, current_nick) || !valid_sender(nick) || !shares(rosters, nick, None)
        {
            return;
        }
        let key = nickname_key(nick);
        if !advertises_avatar(real) {
            if !self.stages.contains_key(&key) && self.stages.len() < MAX_PROBED_USERS {
                self.stages.insert(key, Stage::Seen);
            }
            return;
        }
        let from_metadata = self
            .avatars
            .get(&key)
            .is_some_and(|sources| sources.metadata.is_some());
        if self.stages.get(&key) != Some(&Stage::Queried) && !from_metadata {
            self.enqueue(ProbeKind::Query, nick, key);
        }
    }

    /// Someone spoke live where we can see them: look up their realname
    /// once, if nothing is known about them yet.
    pub(crate) fn speaker(
        &mut self,
        nick: &str,
        current_nick: &str,
        rosters: &HashMap<String, Vec<String>>,
    ) -> Option<String> {
        if same_nickname(nick, current_nick) || !valid_sender(nick) || !shares(rosters, nick, None)
        {
            return None;
        }
        let key = nickname_key(nick);
        if self.stages.contains_key(&key) || self.avatars.contains_key(&key) {
            return None;
        }
        self.enqueue(ProbeKind::Who, nick, key)
    }

    fn enqueue(&mut self, kind: ProbeKind, nick: &str, key: String) -> Option<String> {
        if !self.stages.contains_key(&key) && self.stages.len() >= MAX_PROBED_USERS {
            return None;
        }
        if self.queue.len() >= MAX_QUEUED_PROBES {
            self.skipped += 1;
            return (self.skipped == 1).then(|| {
                format!("Too many avatar lookups queued; not asking about {nick} for now.")
            });
        }
        self.stages.insert(
            key.clone(),
            match kind {
                ProbeKind::Who => Stage::Seen,
                ProbeKind::Query => Stage::Queried,
            },
        );
        // A lookup still waiting is replaced by the query it would lead to.
        self.queue.retain(|probe| probe.key != key);
        self.queue.push_back(Probe {
            kind,
            nickname: nick.to_owned(),
            key,
            deadline: Instant::now(),
            stale: false,
        });
        None
    }

    /// The next time [`PeerAvatars::tick`] has something to do.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let send = (!self.queue.is_empty() && self.in_flight.len() < MAX_PROBES_IN_FLIGHT)
            .then(|| self.next_probe.unwrap_or_else(Instant::now));
        let expiry = self.in_flight.iter().map(|probe| probe.deadline).min();
        send.into_iter().chain(expiry).min()
    }

    /// Gives up unanswered probes and sends the next one when due.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        current_nick: &str,
        rosters: &HashMap<String, Vec<String>>,
    ) -> Handled {
        let mut handled = Handled::default();
        self.in_flight.retain(|probe| probe.deadline > now);
        if self.next_probe.is_some_and(|due| due > now)
            || self.in_flight.len() >= MAX_PROBES_IN_FLIGHT
        {
            return handled;
        }
        while let Some(mut probe) = self.queue.pop_front() {
            if same_nickname(&probe.nickname, current_nick)
                || !shares(rosters, &probe.nickname, None)
            {
                continue;
            }
            handled.send.push(match probe.kind {
                ProbeKind::Who => IrcCommand::WHO(Some(probe.nickname.clone()), None),
                ProbeKind::Query => {
                    IrcCommand::PRIVMSG(probe.nickname.clone(), format!("\u{1}{CTCP_TAG}\u{1}"))
                }
            });
            probe.deadline = now + PROBE_TIMEOUT;
            self.in_flight.push(probe);
            self.next_probe = Some(now + PROBE_INTERVAL);
            break;
        }
        if self.queue.is_empty() {
            self.skipped = 0;
        }
        handled
    }

    /// Follows who is present, judged from the rosters published before
    /// `message`, like metadata's lifecycle: NICK moves a user's avatar and
    /// what we asked; QUIT, or PART/KICK from the last shared channel, ends
    /// them. Call it before passing metadata's events for the same message
    /// through [`PeerAvatars::merge_metadata`].
    pub(crate) fn lifecycle(
        &mut self,
        message: &IrcMessage,
        rosters: &HashMap<String, Vec<String>>,
        current_nick: &str,
    ) -> Vec<Event> {
        let Some(actor) = message.source_nickname() else {
            return Vec::new();
        };
        let ours = |nick: &str| same_nickname(nick, current_nick);
        let gone: Vec<String> = match &message.command {
            IrcCommand::NICK(new) => return self.rename(actor, new),
            IrcCommand::QUIT(_) if !ours(actor) => vec![actor.to_owned()],
            IrcCommand::PART(channel, _) | IrcCommand::KICK(channel, _, _) => {
                let leaving = match &message.command {
                    IrcCommand::KICK(_, nickname, _) => nickname.as_str(),
                    _ => actor,
                };
                if ours(leaving) {
                    let others: HashSet<String> = rosters
                        .iter()
                        .filter(|(name, _)| *name != channel)
                        .flat_map(|(_, members)| members)
                        .map(|member| nickname_key(display_nickname(member)))
                        .collect();
                    rosters
                        .get(channel)
                        .into_iter()
                        .flatten()
                        .map(|member| display_nickname(member))
                        .filter(|nick| !ours(nick) && !others.contains(&nickname_key(nick)))
                        .map(str::to_owned)
                        .collect()
                } else if shares(rosters, leaving, Some(channel)) {
                    Vec::new()
                } else {
                    vec![leaving.to_owned()]
                }
            }
            _ => Vec::new(),
        };
        gone.iter().filter_map(|nick| self.forget(nick)).collect()
    }

    /// The user left: their avatar ends, and what we were asking about them
    /// is dropped (answers still out are swallowed unused).
    fn forget(&mut self, nick: &str) -> Option<Event> {
        let key = nickname_key(nick);
        self.stages.remove(&key);
        self.queue.retain(|probe| probe.key != key);
        self.in_flight
            .retain(|probe| probe.kind == ProbeKind::Who || probe.key != key);
        for probe in &mut self.in_flight {
            if probe.key == key {
                probe.stale = true;
            }
        }
        let sources = self.avatars.remove(&key)?;
        sources.shown().map(|_| Event::UserAvatar {
            nickname: sources.nickname,
            url: None,
        })
    }

    fn rename(&mut self, from: &str, to: &str) -> Vec<Event> {
        let (from_key, to_key) = (nickname_key(from), nickname_key(to));
        if from_key == to_key {
            if let Some(sources) = self.avatars.get_mut(&from_key) {
                sources.nickname = to.to_owned();
            }
            return Vec::new();
        }
        // Anything under the new name belonged to someone who left it.
        let mut events: Vec<Event> = self.forget(to).into_iter().collect();
        if let Some(stage) = self.stages.remove(&from_key) {
            self.stages.insert(to_key.clone(), stage);
        }
        for probe in &mut self.queue {
            if probe.key == from_key {
                probe.nickname = to.to_owned();
                probe.key = to_key.clone();
            }
        }
        // A query's answer comes from the new name and is taken as an
        // announcement; a lookup's reply describes the old name's holder.
        self.in_flight
            .retain(|probe| probe.kind == ProbeKind::Who || probe.key != from_key);
        for probe in &mut self.in_flight {
            if probe.key == from_key {
                probe.stale = true;
                if self.stages.get(&to_key) == Some(&Stage::Seen) {
                    self.stages.remove(&to_key);
                }
            }
        }
        if let Some(mut sources) = self.avatars.remove(&from_key) {
            sources.nickname = to.to_owned();
            if sources.shown().is_some() {
                events.push(Event::AvatarMoved {
                    from: from.to_owned(),
                    to: to.to_owned(),
                });
            }
            self.avatars.insert(to_key, sources);
        }
        events
    }

    fn set_peer(&mut self, nickname: &str, url: Option<String>) -> Option<Event> {
        self.set(nickname, |sources| sources.peer = url)
    }

    /// Applies one source's change and reports the shown avatar if it
    /// changed. A new user beyond the bound is ignored.
    fn set(&mut self, nickname: &str, change: impl FnOnce(&mut Sources)) -> Option<Event> {
        let key = nickname_key(nickname);
        if !self.avatars.contains_key(&key) && self.avatars.len() >= MAX_AVATAR_USERS {
            return None;
        }
        let sources = self.avatars.entry(key.clone()).or_insert_with(|| Sources {
            nickname: nickname.to_owned(),
            metadata: None,
            peer: None,
        });
        let before = sources.shown();
        change(sources);
        sources.nickname = nickname.to_owned();
        let after = sources.shown();
        if sources.metadata.is_none() && sources.peer.is_none() {
            self.avatars.remove(&key);
        }
        (before != after).then(|| Event::UserAvatar {
            nickname: nickname.to_owned(),
            url: after,
        })
    }

    /// Passes metadata's avatar events through the merge: the metadata
    /// avatar is shown when there is one, the peer one otherwise. Moves
    /// were already made by [`PeerAvatars::lifecycle`].
    pub(crate) fn merge_metadata(&mut self, events: Vec<Event>) -> Vec<Event> {
        let mut merged = Vec::new();
        for event in events {
            match event {
                Event::UserAvatar { nickname, url } => {
                    let key = nickname_key(&nickname);
                    if url.is_some()
                        && !self.avatars.contains_key(&key)
                        && self.avatars.len() >= MAX_AVATAR_USERS
                    {
                        // Not tracked here; metadata still reports it.
                        merged.push(Event::UserAvatar { nickname, url });
                    } else {
                        merged.extend(self.set(&nickname, |sources| sources.metadata = url));
                    }
                }
                Event::AvatarMoved { from, to } => {
                    if !self.avatars.contains_key(&nickname_key(&to)) {
                        merged.push(Event::AvatarMoved { from, to });
                    }
                }
                Event::AvatarsReset => {
                    // The application forgets every avatar of the
                    // connection; peer ones are reported again.
                    merged.push(Event::AvatarsReset);
                    self.avatars.retain(|_, sources| {
                        sources.metadata = None;
                        sources.peer.is_some()
                    });
                    merged.extend(self.avatars.values().map(|sources| Event::UserAvatar {
                        nickname: sources.nickname.clone(),
                        url: sources.peer.clone(),
                    }));
                }
                other => merged.push(other),
            }
        }
        merged
    }

    fn may_reply(&mut self, sender: &str, now: Instant) -> bool {
        while self
            .replies
            .front()
            .is_some_and(|sent| now.duration_since(*sent) >= REPLY_WINDOW)
        {
            self.replies.pop_front();
        }
        self.replied
            .retain(|_, sent| now.duration_since(*sent) < REPLY_INTERVAL_PER_USER);
        let key = nickname_key(sender);
        if self.replies.len() >= MAX_REPLIES_PER_WINDOW
            || self.replied.contains_key(&key)
            || self.replied.len() >= MAX_REPLY_RECIPIENTS
        {
            return false;
        }
        self.replies.push_back(now);
        self.replied.insert(key, now);
        true
    }
}

fn valid_sender(nick: &str) -> bool {
    crate::valid_nickname(nick) && !valid_channel(nick)
}

/// Whether `nick` is in a channel we are in, other than `except`.
fn shares(rosters: &HashMap<String, Vec<String>>, nick: &str, except: Option<&str>) -> bool {
    rosters.iter().any(|(channel, members)| {
        Some(channel.as_str()) != except
            && members
                .iter()
                .any(|member| same_nickname(display_nickname(member), nick))
    })
}

fn in_channel(rosters: &HashMap<String, Vec<String>>, channel: &str, nick: &str) -> bool {
    rosters.iter().any(|(name, members)| {
        name.eq_ignore_ascii_case(channel)
            && members
                .iter()
                .any(|member| same_nickname(display_nickname(member), nick))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "alice";

    fn line(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    /// `#a` holds us, kv (a KVIrc user), bob and carol; `#b` holds us and
    /// carol.
    fn rosters() -> HashMap<String, Vec<String>> {
        HashMap::from([
            (
                "#a".to_owned(),
                vec!["@alice".into(), "kv".into(), "+bob".into(), "carol".into()],
            ),
            ("#b".to_owned(), vec!["alice".into(), "carol".into()]),
        ])
    }

    fn observe(peers: &mut PeerAvatars, text: &str, now: Instant) -> Option<Handled> {
        peers.observe(&line(text), ME, &rosters(), false, now)
    }

    fn wire(commands: &[IrcCommand]) -> Vec<String> {
        commands
            .iter()
            .map(|command| {
                IrcMessage::from(command.clone())
                    .to_string()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn avatars(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                Event::UserAvatar { nickname, url } => {
                    format!("{nickname}={}", url.as_deref().unwrap_or("-"))
                }
                Event::AvatarMoved { from, to } => format!("{from}->{to}"),
                Event::AvatarsReset => "reset".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// Accepts kv's avatar as announced to us.
    fn with_kv_avatar(url: &str) -> PeerAvatars {
        let mut peers = PeerAvatars::new(true, None);
        let handled = observe(
            &mut peers,
            &format!(":kv!u@h NOTICE alice :\u{1}AVATAR {url} M\u{1}"),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(avatars(&handled.events), [format!("kv={url}")]);
        peers
    }

    #[test]
    fn the_mark_is_stripped_to_show_the_configured_realname() {
        assert_eq!(without_mark(&realname("Alice", true)), "Alice");
        assert_eq!(without_mark("Alice"), "Alice");
        assert_eq!(without_mark("\u{3}8\u{f}Alice"), "\u{3}8\u{f}Alice");
    }

    #[test]
    fn realname_mark_follows_kvirc() {
        // KVIrc prepends ETX, the flag digit and SI; the text is kept.
        assert_eq!(realname("CayenChat", true), "\u{3}4\u{f}CayenChat");
        assert_eq!(realname("CayenChat", false), "CayenChat");
        assert!(advertises_avatar(&realname("CayenChat", true)));
        assert!(advertises_avatar("\u{3}4\u{f}"));
        // Gender bits (1 male, 2 female) combined with the avatar bit, and
        // KVIrc's nickname color tag after the mark.
        for digit in ['4', '5', '6', '7'] {
            assert!(
                advertises_avatar(&format!("\u{3}{digit}\u{f}\u{3}12,1\u{f}Name")),
                "{digit}"
            );
        }
        for real in [
            "\u{3}0\u{f}Name",
            "\u{3}1\u{f}Name",
            "\u{3}3\u{f}Name",
            "\u{3}8\u{f}Name",
            "\u{3}4Name",
            "\u{3}44\u{f}Name",
            "4\u{f}Name",
            " \u{3}4\u{f}Name",
            "\u{3}",
            "",
            "Plain name",
        ] {
            assert!(!advertises_avatar(real), "{real:?}");
        }
    }

    #[test]
    fn replies_are_parsed_like_kvirc_sends_them_and_only_urls_are_used() {
        let url = |text: &str| avatar_ctcp(text).and_then(|params| reply_url(params, true));
        for (text, expected) in [
            // The answer to a query carries a gender field.
            (
                "\u{1}AVATAR https://example.com/me.png M\u{1}",
                Some("https://example.com/me.png"),
            ),
            (
                "\u{1}AVATAR https://example.com/me.png ?\u{1}",
                Some("https://example.com/me.png"),
            ),
            // avatar.notify without and with a file size.
            (
                "\u{1}AVATAR http://example.com/a\u{1}",
                Some("http://example.com/a"),
            ),
            (
                "\u{1}avatar HTTPS://Example.com/a.png 1234\u{1}",
                Some("HTTPS://Example.com/a.png"),
            ),
            // No closing 0x01.
            (
                "\u{1}AVATAR https://example.com/b.png",
                Some("https://example.com/b.png"),
            ),
            // No avatar: empty, quoted empty, or only the gender.
            ("\u{1}AVATAR \u{1}", None),
            ("\u{1}AVATAR\u{1}", None),
            ("\u{1}AVATAR \"\" F\u{1}", None),
            // Local files offered by DCC GET, and other schemes.
            ("\u{1}AVATAR avatar.png 20480\u{1}", None),
            ("\u{1}AVATAR my\\040face.png 20480\u{1}", None),
            ("\u{1}AVATAR C:\\avatars\\me.png M\u{1}", None),
            ("\u{1}AVATAR /home/me/me.png M\u{1}", None),
            ("\u{1}AVATAR file:///home/me/me.png M\u{1}", None),
            ("\u{1}AVATAR ftp://example.com/me.png M\u{1}", None),
            ("\u{1}AVATAR https:// M\u{1}", None),
            ("\u{1}AVATAR https://example.com/a\u{7}b\u{1}", None),
        ] {
            assert_eq!(url(text).as_deref(), expected, "{text:?}");
        }
        // Other CTCP messages are not AVATAR.
        for text in [
            "\u{1}AVATARS\u{1}",
            "\u{1}ACTION waves\u{1}",
            "\u{1}DCC SEND avatar.png 2130706433 5000 20480\u{1}",
            "AVATAR https://example.com/a.png",
        ] {
            assert_eq!(avatar_ctcp(text), None, "{text:?}");
        }
        // Legacy encodings: only ASCII values.
        assert_eq!(reply_url("https://例え.jp/a.png", false), None);
        assert!(reply_url("https://例え.jp/a.png", true).is_some());
    }

    #[test]
    fn shared_urls_are_http_only_and_fit_a_line() {
        assert!(shareable("https://example.com/me.png", true).is_ok());
        for bad in [
            "",
            "avatar.png",
            "ftp://example.com/a.png",
            "https://example.com/a b.png",
            "https://example.com/a\r\nQUIT",
            ":https://example.com/a.png",
        ] {
            assert!(shareable(bad, true).is_err(), "{bad:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(400));
        assert!(shareable(&long, true).is_err());
        // An unusable URL is never answered.
        assert_eq!(PeerAvatars::new(true, Some("me.png".into())).share, None);
    }

    #[test]
    fn private_queries_are_answered_only_while_sharing_and_rate_limited() {
        let now = Instant::now();
        let query = ":kv!u@h PRIVMSG alice :\u{1}AVATAR\u{1}";
        // Nothing shared: consumed, not answered.
        let mut peers = PeerAvatars::new(true, None);
        let handled = observe(&mut peers, query, now).unwrap();
        assert!(handled.send.is_empty() && handled.events.is_empty());

        peers.set_share(Some("https://example.com/me.png".into()));
        let handled = observe(&mut peers, query, now).unwrap();
        assert_eq!(
            wire(&handled.send),
            ["NOTICE kv :\u{1}AVATAR https://example.com/me.png\u{1}"]
        );
        // Once per user per minute.
        assert!(observe(&mut peers, query, now).unwrap().send.is_empty());
        assert_eq!(
            observe(&mut peers, query, now + REPLY_INTERVAL_PER_USER)
                .unwrap()
                .send
                .len(),
            1
        );
        // Channel queries, replayed ones and our own echo are not answered.
        for text in [
            ":bob!u@h PRIVMSG #a :\u{1}AVATAR\u{1}",
            ":alice!u@h PRIVMSG alice :\u{1}AVATAR\u{1}",
            ":irc.example.net PRIVMSG alice :\u{1}AVATAR\u{1}",
        ] {
            let handled = observe(&mut peers, text, now).unwrap();
            assert!(handled.send.is_empty(), "{text}");
        }
        let replayed = peers
            .observe(
                &line(":bob!u@h PRIVMSG alice :\u{1}AVATAR\u{1}"),
                ME,
                &rosters(),
                true,
                now,
            )
            .unwrap();
        assert!(replayed.send.is_empty());
        // At most five answers per ten seconds in total.
        let later = now + Duration::from_secs(600);
        let sent: usize = (0..8)
            .map(|index| {
                let text = format!(":user{index}!u@h PRIVMSG alice :\u{1}AVATAR\u{1}");
                observe(&mut peers, &text, later).unwrap().send.len()
            })
            .sum();
        assert_eq!(sent, MAX_REPLIES_PER_WINDOW);
        // Stopping ends answers at once.
        peers.set_share(None);
        let text = ":dave!u@h PRIVMSG alice :\u{1}AVATAR\u{1}";
        assert!(
            observe(&mut peers, text, later + REPLY_WINDOW)
                .unwrap()
                .send
                .is_empty()
        );
        // Other CTCP and chat are not ours.
        assert!(observe(&mut peers, ":kv!u@h PRIVMSG alice :\u{1}VERSION\u{1}", now).is_none());
        assert!(observe(&mut peers, ":kv!u@h PRIVMSG #a :AVATAR please", now).is_none());
    }

    #[test]
    fn announcements_count_only_from_users_present_now() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, None);
        // To a channel they are in, or to us from someone sharing a channel.
        let handled = observe(
            &mut peers,
            ":bob!u@h NOTICE #a :\u{1}AVATAR https://example.com/bob.png\u{1}",
            now,
        )
        .unwrap();
        assert_eq!(
            avatars(&handled.events),
            ["bob=https://example.com/bob.png"]
        );
        // Not in that channel, not sharing one with us, replayed, from us,
        // or from the server: consumed and ignored.
        for text in [
            ":bob!u@h NOTICE #b :\u{1}AVATAR https://example.com/x.png\u{1}",
            ":mallory!u@h NOTICE alice :\u{1}AVATAR https://example.com/x.png\u{1}",
            ":mallory!u@h NOTICE #a :\u{1}AVATAR https://example.com/x.png\u{1}",
            ":alice!u@h NOTICE #a :\u{1}AVATAR https://example.com/x.png\u{1}",
            ":irc.example.net NOTICE alice :\u{1}AVATAR https://example.com/x.png\u{1}",
        ] {
            let handled = observe(&mut peers, text, now).unwrap();
            assert!(
                handled.events.is_empty() && handled.send.is_empty(),
                "{text}"
            );
        }
        let replayed = peers
            .observe(
                &line(":carol!u@h NOTICE alice :\u{1}AVATAR https://example.com/c.png\u{1}"),
                ME,
                &rosters(),
                true,
                now,
            )
            .unwrap();
        assert!(replayed.events.is_empty());
        // A file offer ends a known avatar; nothing is requested by DCC.
        let handled = observe(
            &mut peers,
            ":bob!u@h NOTICE alice :\u{1}AVATAR bob.png 2048\u{1}",
            now,
        )
        .unwrap();
        assert_eq!(avatars(&handled.events), ["bob=-"]);
        assert!(handled.send.is_empty());
        // An empty answer removes it too; nothing to remove is silent.
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        let handled = observe(&mut peers, ":kv!u@h NOTICE alice :\u{1}AVATAR \u{1}", now).unwrap();
        assert_eq!(avatars(&handled.events), ["kv=-"]);
        let handled = observe(&mut peers, ":kv!u@h NOTICE alice :\u{1}AVATAR\u{1}", now).unwrap();
        assert!(handled.events.is_empty());
    }

    #[test]
    fn speakers_are_looked_up_once_and_only_marked_users_are_queried() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, None);
        let rosters = rosters();
        // Not present with us, or ourselves: nothing.
        assert_eq!(peers.speaker("mallory", ME, &rosters), None);
        assert_eq!(peers.speaker("Alice", ME, &rosters), None);
        assert_eq!(peers.next_deadline(), None);
        peers.speaker("kv", ME, &rosters);
        peers.speaker("bob", ME, &rosters);
        peers.speaker("KV", ME, &rosters);
        // One at a time, spaced out.
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO kv"]);
        assert!(peers.tick(now, ME, &rosters).send.is_empty());
        assert_eq!(peers.next_deadline(), Some(now + PROBE_INTERVAL));
        let now = now + PROBE_INTERVAL;
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO bob"]);
        // Our lookups' replies are consumed; the mark leads to one query.
        let kv = ":srv 352 alice #a ~kv host srv kv H :0 \u{3}4\u{f}KVIrc user";
        assert!(observe(&mut peers, kv, now).is_some());
        assert!(observe(&mut peers, ":srv 315 alice kv :End of WHO", now).is_some());
        let bob = ":srv 352 alice #a ~bob host srv bob H :0 Bob";
        assert!(observe(&mut peers, bob, now).is_some());
        assert!(observe(&mut peers, ":srv 315 alice bob :End of WHO", now).is_some());
        let now = now + PROBE_INTERVAL;
        assert_eq!(
            wire(&peers.tick(now, ME, &rosters).send),
            ["PRIVMSG kv \u{1}AVATAR\u{1}"]
        );
        let later = now + PROBE_INTERVAL;
        assert!(
            peers.tick(later, ME, &rosters).send.is_empty(),
            "bob has no mark"
        );
        // Speaking again, or a WHO the user asked for, asks nothing more.
        peers.speaker("kv", ME, &rosters);
        peers.speaker("bob", ME, &rosters);
        assert!(observe(&mut peers, kv, later).is_none(), "not ours: shown");
        assert!(
            peers
                .tick(later + PROBE_INTERVAL, ME, &rosters)
                .send
                .is_empty()
        );
        // The answer, KVIrc style.
        let handled = observe(
            &mut peers,
            ":kv!u@h NOTICE alice :\u{1}AVATAR https://example.com/kv.png M\u{1}",
            later,
        )
        .unwrap();
        assert_eq!(avatars(&handled.events), ["kv=https://example.com/kv.png"]);
        // WHOIS the user opened is read (not consumed): carol is marked.
        let whois = ":srv 311 alice carol ~c host * :\u{3}4\u{f}Carol";
        assert!(observe(&mut peers, whois, later).is_none());
        let now = later + PROBE_INTERVAL * 2;
        assert_eq!(
            wire(&peers.tick(now, ME, &rosters).send),
            ["PRIVMSG carol \u{1}AVATAR\u{1}"]
        );
        // Errors for our own probes are consumed; others are shown.
        assert!(observe(&mut peers, ":srv 401 alice carol :No such nick", now).is_some());
        assert!(observe(&mut peers, ":srv 401 alice zed :No such nick", now).is_none());
    }

    #[test]
    fn discovery_is_bounded() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, None);
        let members: Vec<String> = (0..100).map(|index| format!("u{index}")).collect();
        let rosters = HashMap::from([("#big".to_owned(), members.clone())]);
        let notes: Vec<String> = members
            .iter()
            .filter_map(|nick| peers.speaker(nick, ME, &rosters))
            .collect();
        assert_eq!(peers.queue.len(), MAX_QUEUED_PROBES);
        assert_eq!(notes.len(), 1, "reported once");
        // At most a few outstanding; unanswered ones time out.
        let mut sent = Vec::new();
        let mut at = now;
        for _ in 0..10 {
            sent.extend(wire(&peers.tick(at, ME, &rosters).send));
            at += PROBE_INTERVAL;
        }
        assert_eq!(sent.len(), MAX_PROBES_IN_FLIGHT);
        let expired = now + PROBE_TIMEOUT + PROBE_INTERVAL * 4;
        assert_eq!(peers.tick(expired, ME, &rosters).send.len(), 1);
        // A timed-out user is not asked again.
        peers.speaker("u0", ME, &rosters);
        assert!(!peers.queue.iter().any(|probe| probe.nickname == "u0"));
        // RPL_TRYAGAIN pauses lookups.
        let handled = observe(&mut peers, ":srv 263 alice WHO :Try again", expired);
        assert!(handled.is_some());
        assert!(
            peers
                .tick(expired + PROBE_INTERVAL, ME, &rosters)
                .send
                .is_empty()
        );
        assert_eq!(
            peers
                .tick(expired + TRY_AGAIN_PAUSE, ME, &rosters)
                .send
                .len(),
            1
        );
    }

    #[test]
    fn avatars_follow_one_user_and_are_never_inherited() {
        let rosters = rosters();
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        // NICK moves it.
        let events = peers.lifecycle(&line(":kv!u@h NICK kv2"), &rosters, ME);
        assert_eq!(avatars(&events), ["kv->kv2"]);
        let moved: HashMap<String, Vec<String>> = HashMap::from([(
            "#a".to_owned(),
            vec!["alice".into(), "kv2".into(), "bob".into()],
        )]);
        // QUIT ends it; the next holder of the name gets nothing from it.
        let events = peers.lifecycle(&line(":kv2!u@h QUIT :bye"), &moved, ME);
        assert_eq!(avatars(&events), ["kv2=-"]);
        assert!(peers.avatars.is_empty());
        // PART from the last shared channel ends it; from one of two not.
        let mut peers = PeerAvatars::new(true, None);
        observe(
            &mut peers,
            ":carol!u@h NOTICE #b :\u{1}AVATAR https://example.com/c.png\u{1}",
            Instant::now(),
        );
        assert!(
            peers
                .lifecycle(&line(":carol!u@h PART #a"), &rosters, ME)
                .is_empty()
        );
        let only_b = HashMap::from([(
            "#b".to_owned(),
            vec!["alice".to_owned(), "carol".to_owned()],
        )]);
        let events = peers.lifecycle(&line(":carol!u@h PART #b"), &only_b, ME);
        assert_eq!(avatars(&events), ["carol=-"]);
        // Our own PART ends everyone we shared only through that channel.
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        let events = peers.lifecycle(&line(":alice!u@h PART #a"), &rosters, ME);
        assert_eq!(avatars(&events), ["kv=-"]);
        // Renaming onto a name with an avatar ends that one first.
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        observe(
            &mut peers,
            ":bob!u@h NOTICE #a :\u{1}AVATAR https://example.com/bob.png\u{1}",
            Instant::now(),
        );
        let events = peers.lifecycle(&line(":bob!u@h NICK kv"), &rosters, ME);
        assert_eq!(avatars(&events), ["kv=-", "bob->kv"]);
    }

    #[test]
    fn stale_lookups_are_swallowed_but_not_used() {
        let now = Instant::now();
        let rosters = rosters();
        let mut peers = PeerAvatars::new(true, None);
        peers.speaker("bob", ME, &rosters);
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO bob"]);
        // bob changes name before the reply; someone else now holds "bob".
        assert!(
            peers
                .lifecycle(&line(":bob!u@h NICK bobby"), &rosters, ME)
                .is_empty()
        );
        let reply = ":srv 352 alice #a ~x host srv bob H :0 \u{3}4\u{f}Someone else";
        assert!(observe(&mut peers, reply, now).is_some(), "still ours");
        assert!(observe(&mut peers, ":srv 315 alice bob :End", now).is_some());
        assert!(peers.queue.is_empty(), "no query from a stale reply");
        // Departures drop queued work.
        peers.speaker("carol", ME, &rosters);
        peers.lifecycle(&line(":carol!u@h QUIT :bye"), &rosters, ME);
        assert!(peers.queue.is_empty());
    }

    #[test]
    fn server_metadata_wins_and_peers_fill_in() {
        let rosters = rosters();
        let meta = |nick: &str, url: Option<&str>| Event::UserAvatar {
            nickname: nick.into(),
            url: url.map(str::to_owned),
        };
        let mut peers = PeerAvatars::new(true, None);
        let events = peers.merge_metadata(vec![meta("kv", Some("https://meta.example/kv"))]);
        assert_eq!(avatars(&events), ["kv=https://meta.example/kv"]);
        // A marked user with a metadata avatar is not queried.
        let whois = ":srv 311 alice kv ~kv host * :\u{3}4\u{f}KV";
        observe(&mut peers, whois, Instant::now());
        assert!(peers.queue.is_empty());
        // A peer answer does not replace it, but stands in when it goes.
        let handled = observe(
            &mut peers,
            ":kv!u@h NOTICE alice :\u{1}AVATAR https://peer.example/kv.png\u{1}",
            Instant::now(),
        )
        .unwrap();
        assert!(handled.events.is_empty());
        let events = peers.merge_metadata(vec![meta("kv", None)]);
        assert_eq!(avatars(&events), ["kv=https://peer.example/kv.png"]);
        let events = peers.merge_metadata(vec![meta("kv", Some("https://meta.example/kv2"))]);
        assert_eq!(avatars(&events), ["kv=https://meta.example/kv2"]);
        // One move for both sources; metadata's own move is absorbed.
        let mut events = peers.lifecycle(&line(":kv!u@h NICK kv2"), &rosters, ME);
        events.extend(peers.merge_metadata(vec![Event::AvatarMoved {
            from: "kv".into(),
            to: "kv2".into(),
        }]));
        assert_eq!(avatars(&events), ["kv->kv2"]);
        // Losing metadata: everything is reset, peer avatars come back.
        let events = peers.merge_metadata(vec![Event::AvatarsReset]);
        assert_eq!(
            avatars(&events),
            ["reset", "kv2=https://peer.example/kv.png"]
        );
        // A departure ends both without the peer one standing in.
        let mut events = peers.lifecycle(&line(":kv2!u@h QUIT :bye"), &rosters, ME);
        events.extend(peers.merge_metadata(vec![meta("kv2", None)]));
        assert_eq!(avatars(&events), ["kv2=-"]);
    }
}
