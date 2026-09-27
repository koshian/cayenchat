//! The receive-side subset of the experimental IRCv3 metadata draft
//! (`draft/metadata-2`) that user avatars need.
//!
//! Specification: `extensions/metadata.md` of ircv3-specifications as last
//! changed by commit ef205ce (2026-05-07), and the `avatar` key of the IRCv3
//! registry (ircv3.github.io `_data/metadata_registry.yml`, 9480443,
//! 2026-05-21): "URL with an optional `{size}` substitution".
//!
//! Only the `avatar` key of users is used. The client subscribes to it
//! (`METADATA * SUB avatar`) once registered, then follows `METADATA`
//! messages, `761 RPL_KEYVALUE` and `766 RPL_KEYNOTSET` for it. Every other
//! key, and channel targets, are ignored and nothing else is stored: no
//! metadata map exists. `774 RPL_METADATASYNCLATER` for a joined channel
//! schedules one `METADATA <channel> SYNC` after the server's delay, with a
//! bounded number of channels and attempts. Replies of this subset are kept
//! out of the server log (the diagnostic transcript still has them).
//!
//! The draft does not spell out how a removal is announced. A `METADATA`
//! message without a value (three parameters, as the earlier
//! `metadata-notify` did) or with an empty value removes the avatar here.
//!
//! Values are UTF-8, but the `irc` codec decodes whole lines with the
//! connection's encoding. On legacy encodings only ASCII values are
//! accepted, because any non-ASCII byte decodes to something else there; a
//! URL outside ASCII is dropped, never guessed.
//!
//! Metadata never produces chat rows, unread marks, notifications or
//! previews: the events here only feed the avatar directory.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use irc::proto::{Command as IrcCommand, Message as IrcMessage};
use tokio::time::Instant;

use crate::{
    Event, display_nickname,
    text::{nickname_key, same_nickname},
    valid_channel, valid_nickname,
};

pub(crate) const AVATAR_KEY: &str = "avatar";
/// Longer avatar values are ignored (treated as no avatar). The media layer
/// refuses URLs above the same length.
pub(crate) const MAX_AVATAR_BYTES: usize = 2048;
/// Users with an avatar remembered per connection. A new avatar beyond this
/// is ignored (the user shows none) rather than evicting another, so the
/// lifecycle below can always tell the application about every avatar it
/// holds. The application directory has the same bound.
pub const MAX_AVATAR_USERS: usize = 2048;
/// Channels waiting for a deferred synchronization at once.
const MAX_PENDING_SYNCS: usize = 16;
/// `METADATA <channel> SYNC` requests per channel and connection.
const MAX_SYNC_ATTEMPTS: u8 = 3;
/// Delay used when 774 names none, and the bounds applied to a named one.
const DEFAULT_SYNC_DELAY: Duration = Duration::from_secs(5);
const MIN_SYNC_DELAY: Duration = Duration::from_secs(1);
const MAX_SYNC_DELAY: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct PendingSync {
    channel: String,
    due: Instant,
}

/// The result of one incoming message.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Handled {
    pub events: Vec<Event>,
    pub notes: Vec<String>,
}

/// Per-connection metadata state. It lives in the connection's worker, so a
/// reconnect starts empty; [`MetadataState::reset`] empties it when the
/// capability (or `batch`, which it requires) goes away.
#[derive(Debug)]
pub(crate) struct MetadataState {
    utf8: bool,
    subscribed: bool,
    /// Folded nicknames that currently have an avatar. Only these are
    /// reported when they quit, leave or change nickname.
    known: HashSet<String>,
    syncs: Vec<PendingSync>,
    /// One entry per synchronization requested on this connection, so a
    /// server that keeps deferring cannot make the client retry forever.
    attempted: Vec<String>,
}

impl MetadataState {
    pub(crate) fn new(utf8: bool) -> Self {
        Self {
            utf8,
            subscribed: false,
            known: HashSet::new(),
            syncs: Vec::new(),
            attempted: Vec::new(),
        }
    }

    /// The subscription to send once registered with metadata enabled.
    /// Sent at most once per enablement.
    pub(crate) fn subscribe(&mut self) -> Option<IrcCommand> {
        if self.subscribed {
            return None;
        }
        self.subscribed = true;
        Some(metadata_command(&["*", "SUB", AVATAR_KEY]))
    }

    /// Forgets everything; the capability was withdrawn.
    pub(crate) fn reset(&mut self) {
        self.subscribed = false;
        self.known = HashSet::new();
        self.syncs = Vec::new();
        self.attempted = Vec::new();
    }

    /// The next deferred synchronization, if any.
    pub(crate) fn next_sync(&self) -> Option<Instant> {
        self.syncs.iter().map(|sync| sync.due).min()
    }

    /// Synchronizations that are due. `joined` tells whether the channel is
    /// still joined; others are dropped.
    pub(crate) fn due_syncs(
        &mut self,
        now: Instant,
        joined: impl Fn(&str) -> bool,
    ) -> Vec<IrcCommand> {
        let mut send = Vec::new();
        self.syncs.retain(|sync| {
            if sync.due > now {
                return true;
            }
            if joined(&sync.channel) {
                send.push(metadata_command(&[&sync.channel, "SYNC"]));
            }
            false
        });
        send
    }

    /// Stops a pending synchronization of a channel we left.
    pub(crate) fn forget_channel(&mut self, channel: &str) {
        self.syncs
            .retain(|sync| !sync.channel.eq_ignore_ascii_case(channel));
    }

    /// Handles a metadata reply or notification. `None` means the message
    /// is not part of this subset and is processed as usual.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        now: Instant,
        joined: impl Fn(&str) -> bool,
    ) -> Option<Handled> {
        let mut handled = Handled::default();
        match &message.command {
            IrcCommand::METADATA(..) => {
                let args = metadata_args(&message.command);
                // METADATA <Target> <Key> <Visibility> [<Value>]
                if let [target, key, _visibility, rest @ ..] = args.as_slice()
                    && *key == AVATAR_KEY
                {
                    handled
                        .events
                        .extend(self.avatar(target, rest.first().copied()));
                }
            }
            IrcCommand::Raw(verb, args) if verb.eq_ignore_ascii_case("METADATA") => {
                if let [target, key, _visibility, rest @ ..] = args.as_slice()
                    && key == AVATAR_KEY
                {
                    handled
                        .events
                        .extend(self.avatar(target, rest.first().map(String::as_str)));
                }
            }
            IrcCommand::Raw(verb, args)
                if verb == "FAIL" && args.first().is_some_and(|c| c == "METADATA") =>
            {
                let detail = args[1..].join(" ");
                handled
                    .notes
                    .push(format!("Server metadata reply: FAIL METADATA {detail}"));
            }
            _ => {
                let (code, args) = numeric(message)?;
                match code {
                    // RPL_KEYVALUE <client> <Target> <Key> <Visibility> :<Value>
                    761 => {
                        if let [_, target, key, _visibility, value] = args
                            && key == AVATAR_KEY
                        {
                            handled.events.extend(self.avatar(target, Some(value)));
                        }
                    }
                    // RPL_KEYNOTSET <client> <Target> <Key> :key not set
                    766 => {
                        if let [_, target, key, ..] = args
                            && key == AVATAR_KEY
                        {
                            handled.events.extend(self.avatar(target, None));
                        }
                    }
                    // RPL_METADATASUBOK, RPL_METADATAUNSUBOK, RPL_METADATASUBS
                    770..=772 => handled.notes.push(format!(
                        "Server metadata subscription reply {code}: {}",
                        args.get(1..).unwrap_or_default().join(" ")
                    )),
                    // RPL_METADATASYNCLATER <client> <Target> [<RetryAfter>]
                    774 => {
                        if let Some(note) = self.defer(args, now, joined) {
                            handled.notes.push(note);
                        }
                    }
                    _ => return None,
                }
            }
        }
        Some(handled)
    }

    fn avatar(&mut self, target: &str, value: Option<&str>) -> Option<Event> {
        // Channel avatars are out of scope; `*` and other odd targets too.
        if target == "*" || valid_channel(target) || !valid_nickname(target) {
            return None;
        }
        let key = nickname_key(target);
        let url = value.and_then(|value| avatar_value(value, self.utf8));
        if url.is_some() {
            if !self.known.contains(&key) && self.known.len() >= MAX_AVATAR_USERS {
                return None;
            }
            self.known.insert(key);
        } else if !self.known.remove(&key) {
            // Nothing to remove.
            return None;
        }
        Some(Event::UserAvatar {
            nickname: target.to_owned(),
            url,
        })
    }

    /// Avatars whose owner is no longer certain after `message`, judged from
    /// the rosters published before it (`rosters`: channel to members with
    /// their rank prefixes). A nickname keeps its avatar only while its user
    /// shares a channel with us: after QUIT, or after leaving (PART, KICK)
    /// the last shared channel, the server stops sending updates and the
    /// nickname may pass to someone else, so the avatar is dropped. NICK
    /// moves it. Our own avatar stays while we are connected.
    pub(crate) fn lifecycle(
        &mut self,
        message: &IrcMessage,
        rosters: &HashMap<String, Vec<String>>,
        current_nick: &str,
    ) -> Vec<Event> {
        if self.known.is_empty() {
            return Vec::new();
        }
        let Some(actor) = message.source_nickname() else {
            return Vec::new();
        };
        let ours = |nick: &str| same_nickname(nick, current_nick);
        let elsewhere = |nick: &str, left: &str| {
            rosters.iter().any(|(channel, members)| {
                channel != left
                    && members
                        .iter()
                        .any(|member| same_nickname(display_nickname(member), nick))
            })
        };
        let gone: Vec<String> = match &message.command {
            IrcCommand::NICK(new) => {
                if self.known.remove(&nickname_key(actor)) {
                    self.known.insert(nickname_key(new));
                    return vec![Event::AvatarMoved {
                        from: actor.to_owned(),
                        to: new.clone(),
                    }];
                }
                // A known avatar under the new name belonged to someone
                // else, who has left that name.
                vec![new.clone()]
            }
            IrcCommand::QUIT(_) if !ours(actor) => vec![actor.to_owned()],
            IrcCommand::PART(channel, _) | IrcCommand::KICK(channel, _, _) => {
                let leaving = match &message.command {
                    IrcCommand::KICK(_, nickname, _) => nickname.as_str(),
                    _ => actor,
                };
                if ours(leaving) {
                    // Everyone only we shared through this channel.
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
                } else if elsewhere(leaving, channel) {
                    Vec::new()
                } else {
                    vec![leaving.to_owned()]
                }
            }
            _ => Vec::new(),
        };
        gone.into_iter()
            .filter(|nick| self.known.remove(&nickname_key(nick)))
            .map(|nickname| Event::UserAvatar {
                nickname,
                url: None,
            })
            .collect()
    }

    fn defer(
        &mut self,
        args: &[String],
        now: Instant,
        joined: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let channel = args.get(1)?;
        // Only channels we are in: users' own metadata arrives with the
        // channels they share with us.
        if !valid_channel(channel) || !joined(channel) {
            return None;
        }
        let delay = args
            .get(2)
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_SYNC_DELAY)
            .clamp(MIN_SYNC_DELAY, MAX_SYNC_DELAY);
        let due = now + delay;
        if let Some(sync) = self
            .syncs
            .iter_mut()
            .find(|sync| sync.channel.eq_ignore_ascii_case(channel))
        {
            sync.due = sync.due.max(due);
            return None;
        }
        let attempts = self
            .attempted
            .iter()
            .filter(|done| done.eq_ignore_ascii_case(channel))
            .count() as u8;
        if attempts >= MAX_SYNC_ATTEMPTS {
            return Some(format!(
                "Server deferred avatars for {channel} again; not asking any more on this connection."
            ));
        }
        if self.syncs.len() >= MAX_PENDING_SYNCS {
            return Some(format!(
                "Too many deferred avatar synchronizations; skipping {channel}."
            ));
        }
        // One entry per attempt, bounded by attempts × pending channels.
        if self.attempted.len() < MAX_PENDING_SYNCS * usize::from(MAX_SYNC_ATTEMPTS) {
            self.attempted.push(channel.clone());
        }
        self.syncs.push(PendingSync {
            channel: channel.clone(),
            due,
        });
        Some(format!(
            "Server deferred avatars for {channel}; synchronizing in {} s.",
            delay.as_secs()
        ))
    }

    #[cfg(test)]
    fn pending_syncs(&self) -> Vec<(String, u8)> {
        self.syncs
            .iter()
            .map(|sync| {
                let attempts = self
                    .attempted
                    .iter()
                    .filter(|done| **done == sync.channel)
                    .count() as u8;
                (sync.channel.clone(), attempts)
            })
            .collect()
    }
}

/// A usable avatar value, or `None` (no avatar). The value is not parsed as
/// a URL here; the media layer decides whether it may be fetched.
pub(crate) fn avatar_value(value: &str, utf8: bool) -> Option<String> {
    let usable = !value.is_empty()
        && value.len() <= MAX_AVATAR_BYTES
        // Bytes the line codec could not decode.
        && !value.contains('\u{FFFD}')
        && !value.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        && (utf8 || value.is_ascii());
    usable.then(|| value.to_owned())
}

fn metadata_command(args: &[&str]) -> IrcCommand {
    IrcCommand::Raw(
        "METADATA".into(),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
    )
}

/// irc-proto 1.1.0 parses `METADATA` for the old metadata-3.2 client
/// command: with three parameters and a second one that is not GET, LIST,
/// SET or CLEAR it keeps the target and puts the rest in its list; with four
/// it falls back to `Raw`. Either way the original order is recovered here.
fn metadata_args(command: &IrcCommand) -> Vec<&str> {
    match command {
        IrcCommand::METADATA(target, subcommand, rest) => std::iter::once(target.as_str())
            .chain(subcommand.as_ref().map(|subcommand| subcommand.to_str()))
            .chain(
                rest.iter()
                    .flatten()
                    // With a subcommand the list repeats it first.
                    .skip(usize::from(subcommand.is_some()))
                    .map(String::as_str),
            )
            .collect(),
        _ => Vec::new(),
    }
}

/// A numeric reply as its code and parameters. irc-proto knows some of the
/// metadata numerics under their metadata-3.2 names and none of the newer
/// ones, which stay `Raw`.
fn numeric(message: &IrcMessage) -> Option<(u16, &[String])> {
    match &message.command {
        IrcCommand::Response(response, args) => Some((*response as u16, args)),
        IrcCommand::Raw(command, args) if command.len() == 3 => {
            Some((command.parse::<u16>().ok()?, args))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    fn avatars(handled: Option<Handled>) -> Vec<(String, Option<String>)> {
        handled
            .unwrap()
            .events
            .into_iter()
            .map(|event| match event {
                Event::UserAvatar { nickname, url } => (nickname, url),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    fn joined(channel: &str) -> bool {
        channel == "#chan" || channel.starts_with("#big")
    }

    #[test]
    fn subscribes_once_to_the_avatar_key_only() {
        let mut state = MetadataState::new(true);
        let sub = state.subscribe().unwrap();
        assert_eq!(String::from(&sub), "METADATA * SUB avatar");
        assert!(state.subscribe().is_none());
        state.reset();
        assert!(state.subscribe().is_some(), "again after re-enabling");
    }

    #[test]
    fn follows_avatar_values_updates_and_removals() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        let observe =
            |state: &mut MetadataState, text: &str| state.observe(&line(text), now, joined);
        assert_eq!(
            avatars(observe(
                &mut state,
                "@batch=a :srv METADATA alice avatar * :https://example.com/a/{size}.png"
            )),
            [(
                "alice".into(),
                Some("https://example.com/a/{size}.png".into())
            )]
        );
        // Removal: no value (three parameters) or an empty value.
        assert_eq!(
            avatars(observe(&mut state, ":srv METADATA alice avatar *")),
            [("alice".into(), None)]
        );
        observe(
            &mut state,
            ":srv METADATA bob avatar * :https://example.com/b",
        );
        assert_eq!(
            avatars(observe(&mut state, ":srv METADATA BOB avatar * :")),
            [("BOB".into(), None)],
            "nicknames compare case-insensitively"
        );
        // Removing an avatar nobody had tells nobody.
        assert!(avatars(observe(&mut state, ":srv METADATA dave avatar *")).is_empty());
        // Replies to GET/LIST use numerics.
        assert_eq!(
            avatars(observe(
                &mut state,
                ":srv 761 me carol avatar * :https://example.com/c.png"
            )),
            [("carol".into(), Some("https://example.com/c.png".into()))]
        );
        assert_eq!(
            avatars(observe(&mut state, ":srv 766 me carol avatar :key not set")),
            [("carol".into(), None)]
        );
    }

    #[test]
    fn other_keys_channels_and_invalid_values_store_nothing() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        for text in [
            ":srv METADATA alice display-name * :Alice",
            ":srv METADATA alice url * :https://example.com",
            ":srv METADATA #chan avatar * :https://example.com/room.png",
            ":srv METADATA * avatar * :https://example.com/x.png",
            ":srv 761 me #chan avatar * :https://example.com/room.png",
        ] {
            let handled = state.observe(&line(text), now, joined).unwrap();
            assert!(handled.events.is_empty(), "{text}");
        }
        // Unusable values replace the avatar with none.
        let long = format!("https://example.com/{}", "a".repeat(MAX_AVATAR_BYTES));
        for value in [long.as_str(), "https://example.com/a b.png", "\u{1}x"] {
            let valid = ":srv METADATA alice avatar * :https://example.com/a.png";
            state.observe(&line(valid), now, joined);
            let text = format!(":srv METADATA alice avatar * :{value}");
            assert_eq!(
                avatars(state.observe(&line(&text), now, joined)),
                [("alice".into(), None)],
                "{value}"
            );
        }
        // Not metadata: handled as before.
        assert!(
            state
                .observe(&line(":srv 001 me :Welcome"), now, joined)
                .is_none()
        );
        assert!(
            state
                .observe(&line(":a!u@h PRIVMSG #chan :hi"), now, joined)
                .is_none()
        );
        assert!(
            state
                .observe(&line("FAIL CHATHISTORY INVALID :no"), now, joined)
                .is_none()
        );
    }

    #[test]
    fn legacy_encodings_accept_only_ascii_values() {
        assert_eq!(
            avatar_value("https://example.com/a.png", false).as_deref(),
            Some("https://example.com/a.png")
        );
        assert_eq!(avatar_value("https://例え.jp/a.png", false), None);
        assert_eq!(
            avatar_value("https://例え.jp/a.png", true).as_deref(),
            Some("https://例え.jp/a.png")
        );
        assert_eq!(avatar_value("https://example.com/\u{FFFD}.png", true), None);
    }

    #[test]
    fn subscription_replies_and_failures_become_diagnostics() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        for text in [
            ":srv 770 me avatar",
            ":srv 772 me avatar",
            "FAIL METADATA TOO_MANY_SUBS avatar :Too many subscriptions!",
            "FAIL METADATA KEY_NO_PERMISSION me avatar :denied",
        ] {
            let handled = state.observe(&line(text), now, joined).unwrap();
            assert!(handled.events.is_empty());
            assert_eq!(handled.notes.len(), 1, "{text}");
        }
    }

    #[test]
    fn deferred_synchronization_is_bounded() {
        let mut state = MetadataState::new(true);
        let start = Instant::now();
        let defer = |state: &mut MetadataState, text: &str, now| {
            state.observe(&line(text), now, joined).unwrap()
        };
        defer(&mut state, ":srv 774 me #chan 4", start);
        // Not joined, or not a channel: ignored.
        defer(&mut state, ":srv 774 me #elsewhere 4", start);
        defer(&mut state, ":srv 774 me alice 4", start);
        assert_eq!(state.pending_syncs(), [("#chan".into(), 1)]);
        assert_eq!(state.next_sync(), Some(start + Duration::from_secs(4)));
        assert!(state.due_syncs(start, joined).is_empty());
        let sent = state.due_syncs(start + Duration::from_secs(4), joined);
        assert_eq!(
            sent.iter().map(String::from).collect::<Vec<_>>(),
            ["METADATA #chan SYNC"]
        );
        assert_eq!(state.next_sync(), None);

        // Deferred again twice more, then the client stops asking.
        for attempt in 2..=3 {
            defer(&mut state, ":srv 774 me #chan 1", start);
            assert_eq!(state.pending_syncs(), [("#chan".into(), attempt)]);
            state.due_syncs(start + Duration::from_secs(10), joined);
        }
        let handled = defer(&mut state, ":srv 774 me #chan 1", start);
        assert_eq!(handled.notes.len(), 1);
        assert!(state.pending_syncs().is_empty());

        // Delays are clamped; a missing delay uses the default.
        defer(&mut state, ":srv 774 me #big1 999999", start);
        defer(&mut state, ":srv 774 me #big2", start);
        defer(&mut state, ":srv 774 me #big3 0", start);
        let due: Vec<_> = state.syncs.iter().map(|sync| sync.due - start).collect();
        assert_eq!(due, [MAX_SYNC_DELAY, DEFAULT_SYNC_DELAY, MIN_SYNC_DELAY]);

        // Many channels: bounded.
        for n in 0..40 {
            defer(&mut state, &format!(":srv 774 me #big{n}x 5"), start);
        }
        assert_eq!(state.syncs.len(), MAX_PENDING_SYNCS);

        // Leaving a channel or losing the capability drops its request.
        state.forget_channel("#BIG1");
        assert!(state.syncs.iter().all(|sync| sync.channel != "#big1"));
        let sent = state.due_syncs(start + MAX_SYNC_DELAY, |_| false);
        assert!(sent.is_empty(), "parted channels are not synchronized");
        defer(&mut state, ":srv 774 me #big2 5", start);
        state.reset();
        assert_eq!((state.next_sync(), state.attempted.len()), (None, 0));
    }

    #[test]
    fn avatars_end_with_the_last_shared_channel_and_are_bounded() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        for nick in ["me", "bob", "carol", "dave"] {
            let text = format!(":srv METADATA {nick} avatar * :https://example.com/{nick}");
            state.observe(&line(&text), now, joined);
        }
        let rosters = HashMap::from([
            (
                "#a".to_owned(),
                vec!["@me".into(), "bob".into(), "+carol".into()],
            ),
            (
                "#b".to_owned(),
                vec!["me".into(), "carol".into(), "dave".into()],
            ),
        ]);
        let gone = |state: &mut MetadataState, text: &str| -> Vec<String> {
            state
                .lifecycle(&line(text), &rosters, "me")
                .into_iter()
                .map(|event| match event {
                    Event::UserAvatar {
                        nickname,
                        url: None,
                    } => nickname,
                    other => panic!("{other:?}"),
                })
                .collect()
        };
        // Carol is still in #b; bob shared only #a.
        assert!(gone(&mut state, ":carol!u@h PART #a").is_empty());
        assert_eq!(gone(&mut state, ":op!u@h KICK #a bob :out"), ["bob"]);
        // We leave #b: dave is gone, carol stays through #a, we keep ours.
        assert_eq!(gone(&mut state, ":me!u@h PART #b"), ["dave"]);
        assert_eq!(gone(&mut state, ":carol!u@h QUIT :bye"), ["carol"]);
        assert!(gone(&mut state, ":carol!u@h QUIT :again").is_empty());
        assert!(gone(&mut state, ":me!u@h QUIT :bye").is_empty());

        // Renaming onto a name with a stale avatar drops that avatar.
        state.observe(
            &line(":srv METADATA erin avatar * :https://example.com/e"),
            now,
            joined,
        );
        assert_eq!(gone(&mut state, ":frank!u@h NICK erin"), ["erin"]);

        // At most MAX_AVATAR_USERS users; later ones get none.
        let mut state = MetadataState::new(true);
        for n in 0..MAX_AVATAR_USERS + 10 {
            let text = format!(":srv METADATA u{n} avatar * :https://example.com/{n}");
            state.observe(&line(&text), now, joined);
        }
        assert_eq!(state.known.len(), MAX_AVATAR_USERS);
        let late = format!(":srv METADATA u{MAX_AVATAR_USERS} avatar * :https://example.com/x");
        assert!(avatars(state.observe(&line(&late), now, joined)).is_empty());
        assert_eq!(
            avatars(state.observe(
                &line(":srv METADATA u0 avatar * :https://e.example/y"),
                now,
                joined
            )),
            [("u0".into(), Some("https://e.example/y".into()))],
            "known users still update"
        );
    }
}
