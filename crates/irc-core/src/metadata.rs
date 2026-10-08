//! The subset of the experimental IRCv3 metadata draft (`draft/metadata-2`)
//! that user avatars need: following other users' avatars, looking up users
//! who join after us, and publishing or removing our own.
//!
//! Specification: `extensions/metadata.md` of ircv3-specifications as last
//! changed by commit ef205ce (2026-05-07, unchanged when rechecked on
//! 2026-09-28), and the `avatar` key of the IRCv3 registry (ircv3.github.io
//! `_data/metadata_registry.yml`, 9480443, 2026-05-21): "URL with an
//! optional `{size}` substitution".
//!
//! Only the `avatar` key of users is used. Once registered the client
//! subscribes to it (`METADATA * SUB avatar`) and asks once for its own
//! value (`METADATA * GET avatar`), then follows `METADATA` messages,
//! `761 RPL_KEYVALUE` and `766 RPL_KEYNOTSET` for it. Every other key, and
//! channel targets, are ignored and nothing else is stored: no metadata map
//! exists. `774 RPL_METADATASYNCLATER` for a joined channel schedules one
//! `METADATA <channel> SYNC` after the server's delay, with a bounded number
//! of channels and attempts. Replies of this subset are kept out of the
//! server log (the diagnostic transcript still has them).
//!
//! Users who join a channel after us: the draft sends a channel's metadata
//! to the user who joins, but not the joiner's metadata to the members
//! already there (Ergo 2.19 behaves exactly so). A live JOIN of a user we
//! share no other channel with therefore schedules one
//! `METADATA <nick> GET avatar` after a short pause, unless the server
//! announced their avatar meanwhile. Lookups are deduplicated per user,
//! spaced out, bounded in number and attempts, dropped when the user leaves,
//! and never sent for replayed history, rosters (NAMES) or redraws.
//!
//! Our own avatar: [`MetadataState::request_own`] sends
//! `METADATA * SET avatar :<url>` or, to remove only that key,
//! `METADATA * SET avatar` (never `CLEAR`). One request is outstanding at a
//! time; it ends with the server's `761`/`766` for our nickname (or `*`),
//! with a `FAIL METADATA` naming us or the key, or after a timeout. Nothing
//! is published implicitly, also not after a reconnect.
//!
//! The draft does not spell out how a removal is announced. A `METADATA`
//! message without a value (three parameters, as the earlier
//! `metadata-notify` did) or with an empty value removes the avatar here.
//! A numeric whose first parameter is `*` is a notification rather than an
//! answer to one of our requests: Ergo announces other users' changes that
//! way instead of with `METADATA` messages.
//!
//! Values are UTF-8, but the `irc` codec decodes whole lines with the
//! connection's encoding. On legacy encodings only ASCII values are
//! accepted, because any non-ASCII byte decodes to something else there; a
//! URL outside ASCII is dropped, never guessed, and never published.
//!
//! Metadata never produces chat rows, unread marks, notifications or
//! previews: the events here only feed the avatar directory and the state
//! of our own avatar.

use std::{collections::HashSet, time::Duration};

use irc::proto::{Command as IrcCommand, Message as IrcMessage};
use tokio::time::Instant;

use crate::{
    Event,
    presence::PresenceIndex,
    text::{nickname_key, same_channel, same_nickname},
    valid_channel, valid_nickname,
};

pub(crate) const AVATAR_KEY: &str = "avatar";
/// Longer avatar values are ignored (treated as no avatar). The media layer
/// refuses URLs above the same length.
pub(crate) const MAX_AVATAR_BYTES: usize = 2048;
/// Longest avatar URL we publish, so `METADATA * SET avatar :<url>` fits an
/// IRC line (512 bytes). Servers may accept less (Ergo: 344 bytes) and
/// answer with a `FAIL`, which is shown as a rejection.
pub const MAX_PUBLISHED_AVATAR_BYTES: usize = 400;
/// Users with an avatar remembered per connection. A new avatar beyond this
/// is ignored (the user shows none) rather than evicting another, so the
/// lifecycle below can always tell the application about every avatar it
/// holds. The application directory has the same bound.
pub const MAX_AVATAR_USERS: usize = 2048;
/// Channels waiting for a deferred synchronization at once.
const MAX_PENDING_SYNCS: usize = 16;
/// `METADATA <channel> SYNC` requests per channel and connection.
const MAX_SYNC_ATTEMPTS: u8 = 3;
/// Delay used when 774 or `RATE_LIMITED` names none, and the bounds applied
/// to a named one.
const DEFAULT_SYNC_DELAY: Duration = Duration::from_secs(5);
const MIN_SYNC_DELAY: Duration = Duration::from_secs(1);
const MAX_SYNC_DELAY: Duration = Duration::from_secs(300);
/// How long our own request (and the initial own-value query) waits for
/// the server.
const OWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// A joiner is looked up only after this pause, so a server that announces
/// their avatar by itself is not asked, and a user who leaves at once costs
/// nothing.
const LOOKUP_DELAY: Duration = Duration::from_secs(2);
/// Minimum spacing between lookups: at most two per second.
const LOOKUP_INTERVAL: Duration = Duration::from_millis(500);
/// Users waiting for a lookup, sent or not. Further joiners are not looked
/// up until the table drains.
pub(crate) const MAX_LOOKUPS: usize = 64;
/// Lookups sent and not yet answered.
const MAX_LOOKUPS_IN_FLIGHT: usize = 8;
/// An unanswered lookup is abandoned after this; its answer, if it still
/// comes within the same time again, is dropped.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Requests per lookup, counting the retry after `RATE_LIMITED`.
const MAX_LOOKUP_ATTEMPTS: u8 = 2;

/// Why a request about our own avatar did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvatarRequestFailure {
    /// Not registered yet, or the metadata capability is not negotiated.
    Unavailable,
    /// Another request is still waiting for the server's answer.
    Busy,
    /// `FAIL METADATA <code> … :<description>` from the server.
    Rejected { code: String, description: String },
    /// `FAIL METADATA RATE_LIMITED`: try again after `retry_after` seconds.
    RateLimited { retry_after: Option<u64> },
    /// The server did not answer in time.
    NoReply,
    /// The capability went away before the server answered.
    CapabilityLost,
}

#[derive(Debug)]
struct PendingSync {
    channel: String,
    due: Instant,
}

/// Our outstanding request: a SET from the user (`request`), or the query
/// for our current value after subscribing (`None`).
#[derive(Debug)]
struct OwnRequest {
    request: Option<u64>,
    removing: bool,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LookupState {
    /// Scheduled for `due`.
    Waiting,
    /// Sent; given up at `due`.
    Sent,
    /// Given up (timeout) or the user left; an answer arriving before `due`
    /// is dropped.
    Abandoned,
}

#[derive(Debug)]
struct Lookup {
    nickname: String,
    key: String,
    state: LookupState,
    due: Instant,
    attempts: u8,
}

/// The result of one incoming message or timer tick.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Handled {
    pub events: Vec<Event>,
    pub notes: Vec<String>,
    pub send: Vec<IrcCommand>,
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
    own: Option<OwnRequest>,
    /// At most [`MAX_LOOKUPS`]; scanned linearly.
    lookups: Vec<Lookup>,
    /// No lookup is sent before this (spacing, or a server rate limit).
    next_lookup: Option<Instant>,
    /// Joiners not looked up because the table was full, since it last
    /// emptied; only the first one is reported.
    lookups_skipped: usize,
}

impl MetadataState {
    pub(crate) fn new(utf8: bool) -> Self {
        Self {
            utf8,
            subscribed: false,
            known: HashSet::new(),
            syncs: Vec::new(),
            attempted: Vec::new(),
            own: None,
            lookups: Vec::new(),
            next_lookup: None,
            lookups_skipped: 0,
        }
    }

    /// What to send once registered with metadata enabled: the subscription
    /// and one query for our own current avatar. Empty when already sent
    /// for this enablement.
    pub(crate) fn start(&mut self, now: Instant) -> Vec<IrcCommand> {
        if self.subscribed {
            return Vec::new();
        }
        self.subscribed = true;
        self.own = Some(OwnRequest {
            request: None,
            removing: false,
            deadline: now + OWN_REQUEST_TIMEOUT,
        });
        vec![
            metadata_command(&["*", "SUB", AVATAR_KEY]),
            metadata_command(&["*", "GET", AVATAR_KEY]),
        ]
    }

    /// Forgets everything; the capability was withdrawn. A request still
    /// waiting for the server fails.
    pub(crate) fn reset(&mut self) -> Vec<Event> {
        let failed =
            self.own
                .take()
                .and_then(|own| own.request)
                .map(|request| Event::OwnAvatarFailed {
                    request,
                    failure: AvatarRequestFailure::CapabilityLost,
                });
        *self = Self::new(self.utf8);
        failed.into_iter().collect()
    }

    /// Publishes (`Some`) or removes (`None`) our own avatar. `value` must
    /// already be validated ([`publishable_avatar`]); `available` says
    /// whether we are registered with the capability negotiated.
    pub(crate) fn request_own(
        &mut self,
        request: u64,
        value: Option<&str>,
        available: bool,
        now: Instant,
    ) -> Result<IrcCommand, AvatarRequestFailure> {
        if !available || !self.subscribed {
            return Err(AvatarRequestFailure::Unavailable);
        }
        if self.own.is_some() {
            return Err(AvatarRequestFailure::Busy);
        }
        self.own = Some(OwnRequest {
            request: Some(request),
            removing: value.is_none(),
            deadline: now + OWN_REQUEST_TIMEOUT,
        });
        let mut args = vec!["*", "SET", AVATAR_KEY];
        args.extend(value);
        Ok(metadata_command(&args))
    }

    /// The next time [`MetadataState::tick`] has something to do.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let in_flight = self.in_flight();
        let lookups = self.lookups.iter().filter_map(|lookup| match lookup.state {
            LookupState::Waiting if in_flight >= MAX_LOOKUPS_IN_FLIGHT => None,
            LookupState::Waiting => Some(
                self.next_lookup
                    .map_or(lookup.due, |next| next.max(lookup.due)),
            ),
            LookupState::Sent | LookupState::Abandoned => Some(lookup.due),
        });
        self.syncs
            .iter()
            .map(|sync| sync.due)
            .chain(self.own.as_ref().map(|own| own.deadline))
            .chain(lookups)
            .min()
    }

    /// Timers: deferred synchronizations, request timeouts and lookups that
    /// may be sent now. `joined` tells whether a channel is still joined.
    pub(crate) fn tick(&mut self, now: Instant, joined: impl Fn(&str) -> bool) -> Handled {
        let mut handled = Handled::default();
        self.syncs.retain(|sync| {
            if sync.due > now {
                return true;
            }
            if joined(&sync.channel) {
                handled
                    .send
                    .push(metadata_command(&[&sync.channel, "SYNC"]));
            }
            false
        });
        if self.own.as_ref().is_some_and(|own| own.deadline <= now) {
            match self.own.take().and_then(|own| own.request) {
                Some(request) => handled.events.push(Event::OwnAvatarFailed {
                    request,
                    failure: AvatarRequestFailure::NoReply,
                }),
                None => handled
                    .notes
                    .push("Server did not answer the query for our own avatar.".into()),
            }
        }
        self.lookups.retain_mut(|lookup| match lookup.state {
            LookupState::Sent if lookup.due <= now => {
                lookup.state = LookupState::Abandoned;
                lookup.due = now + LOOKUP_TIMEOUT;
                true
            }
            LookupState::Abandoned => lookup.due > now,
            _ => true,
        });
        if self.next_lookup.is_none_or(|next| next <= now)
            && self.in_flight() < MAX_LOOKUPS_IN_FLIGHT
            && let Some(lookup) = self
                .lookups
                .iter_mut()
                .filter(|lookup| lookup.state == LookupState::Waiting && lookup.due <= now)
                .min_by_key(|lookup| lookup.due)
        {
            lookup.state = LookupState::Sent;
            lookup.due = now + LOOKUP_TIMEOUT;
            lookup.attempts += 1;
            handled
                .send
                .push(metadata_command(&[&lookup.nickname, "GET", AVATAR_KEY]));
            self.next_lookup = Some(now + LOOKUP_INTERVAL);
        }
        if self.lookups.is_empty() {
            self.lookups_skipped = 0;
        }
        handled
    }

    /// Stops a pending synchronization of a channel we left.
    pub(crate) fn forget_channel(&mut self, channel: &str) {
        self.syncs
            .retain(|sync| !same_channel(&sync.channel, channel));
    }

    /// Handles a metadata reply or notification. `None` means the message
    /// is not part of this subset and is processed as usual.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        now: Instant,
        current_nick: &str,
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
                    handled.events.extend(self.report(
                        target,
                        rest.first().copied(),
                        false,
                        current_nick,
                    ));
                }
            }
            IrcCommand::Raw(verb, args) if verb.eq_ignore_ascii_case("METADATA") => {
                if let [target, key, _visibility, rest @ ..] = args.as_slice()
                    && key == AVATAR_KEY
                {
                    handled.events.extend(self.report(
                        target,
                        rest.first().map(String::as_str),
                        false,
                        current_nick,
                    ));
                }
            }
            IrcCommand::Raw(verb, args)
                if verb == "FAIL" && args.first().is_some_and(|c| c == "METADATA") =>
            {
                let detail = args[1..].join(" ");
                handled
                    .notes
                    .push(format!("Server metadata reply: FAIL METADATA {detail}"));
                handled
                    .events
                    .extend(self.failure(&args[1..], now, current_nick));
            }
            _ => {
                let (code, args) = numeric(message)?;
                // A first parameter of `*` marks a notification, not an
                // answer addressed to us.
                let reply = args.first().is_some_and(|client| client != "*");
                match code {
                    // RPL_KEYVALUE <client> <Target> <Key> <Visibility> :<Value>
                    761 => {
                        if let [_, target, key, _visibility, value] = args
                            && key == AVATAR_KEY
                        {
                            handled.events.extend(self.report(
                                target,
                                Some(value),
                                reply,
                                current_nick,
                            ));
                        }
                    }
                    // RPL_KEYNOTSET <client> <Target> <Key> :key not set
                    766 => {
                        if let [_, target, key, ..] = args
                            && key == AVATAR_KEY
                        {
                            handled
                                .events
                                .extend(self.report(target, None, reply, current_nick));
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

    /// A value (or its absence) for `target`. `reply` marks a numeric
    /// addressed to us, which answers our outstanding request.
    fn report(
        &mut self,
        target: &str,
        value: Option<&str>,
        reply: bool,
        current_nick: &str,
    ) -> Vec<Event> {
        let own = same_nickname(target, current_nick) || (reply && target == "*");
        if own {
            let request = if reply {
                self.own.take().and_then(|own| own.request)
            } else {
                None
            };
            let url = value.and_then(|value| avatar_value(value, self.utf8));
            return std::iter::once(Event::OwnAvatar { url, request })
                .chain(self.avatar(current_nick, value))
                .collect();
        }
        if !valid_channel(target) && valid_nickname(target) {
            let key = nickname_key(target);
            if let Some(index) = self.lookups.iter().position(|lookup| lookup.key == key) {
                match self.lookups[index].state {
                    // Announced without asking: nothing to look up.
                    LookupState::Waiting => {
                        self.lookups.remove(index);
                    }
                    LookupState::Sent if reply => {
                        self.lookups.remove(index);
                    }
                    LookupState::Abandoned if reply => {
                        self.lookups.remove(index);
                        return Vec::new();
                    }
                    _ => {}
                }
            }
        }
        self.avatar(target, value).into_iter().collect()
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

    /// `FAIL METADATA <code> [<params>…] :<description>` (`args` start at
    /// the code). It ends our own request when it names us or the key, and
    /// a lookup when it names that user.
    fn failure(&mut self, args: &[String], now: Instant, current_nick: &str) -> Vec<Event> {
        let Some(code) = args.first().map(String::as_str) else {
            return Vec::new();
        };
        let (params, description) = match args.len() {
            0 | 1 => (&[][..], ""),
            len => (&args[1..len - 1], args[len - 1].as_str()),
        };
        let first = params.first().map(String::as_str);
        // Codes whose first parameter is the target; the others name the
        // key (the draft's names and Ergo's).
        let target_first = matches!(
            code,
            "KEY_NO_PERMISSION"
                | "KEY_NOT_SET"
                | "LIMIT_REACHED"
                | "RATE_LIMITED"
                | "INVALID_TARGET"
                | "FORBIDDEN"
        );
        let key_first = matches!(
            code,
            "KEY_INVALID" | "INVALID_KEY" | "VALUE_INVALID" | "INVALID_VALUE"
        );
        // Codes that also name the key, after the target.
        let other_key = matches!(code, "KEY_NO_PERMISSION" | "KEY_NOT_SET" | "RATE_LIMITED")
            && params
                .get(1)
                .is_some_and(|key| key != AVATAR_KEY && key != "*");
        let names_us = if target_first {
            !other_key
                && first.is_none_or(|target| target == "*" || same_nickname(target, current_nick))
        } else {
            key_first && first.is_none_or(|key| key == AVATAR_KEY)
        };
        let retry_after = || {
            params
                .get(2)
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|seconds| *seconds > 0)
        };
        if names_us && let Some(own) = self.own.take() {
            let Some(request) = own.request else {
                return Vec::new();
            };
            if code == "KEY_NOT_SET" && own.removing {
                // Removing a key that was not set: the goal holds.
                return std::iter::once(Event::OwnAvatar {
                    url: None,
                    request: Some(request),
                })
                .chain(self.avatar(current_nick, None))
                .collect();
            }
            let failure = if code == "RATE_LIMITED" {
                AvatarRequestFailure::RateLimited {
                    retry_after: retry_after(),
                }
            } else {
                AvatarRequestFailure::Rejected {
                    code: code.to_owned(),
                    description: description.to_owned(),
                }
            };
            return vec![Event::OwnAvatarFailed { request, failure }];
        }
        if target_first && let Some(target) = first {
            let key = nickname_key(target);
            if let Some(index) = self
                .lookups
                .iter()
                .position(|lookup| lookup.key == key && lookup.state != LookupState::Waiting)
            {
                let lookup = &mut self.lookups[index];
                if code == "RATE_LIMITED" && lookup.attempts < MAX_LOOKUP_ATTEMPTS {
                    let delay = retry_after()
                        .map(Duration::from_secs)
                        .unwrap_or(DEFAULT_SYNC_DELAY)
                        .clamp(MIN_SYNC_DELAY, MAX_SYNC_DELAY);
                    lookup.state = LookupState::Waiting;
                    lookup.due = now + delay;
                    // The limit is the client's, not this user's.
                    self.next_lookup = Some(
                        self.next_lookup
                            .map_or(now + delay, |next| next.max(now + delay)),
                    );
                } else {
                    self.lookups.remove(index);
                }
            }
        }
        Vec::new()
    }

    fn in_flight(&self) -> usize {
        self.lookups
            .iter()
            .filter(|lookup| lookup.state == LookupState::Sent)
            .count()
    }

    /// Follows who is present, judged from the membership published before
    /// `message` (`presence`).
    ///
    /// A nickname keeps its avatar only while its user shares a channel with
    /// us: after QUIT, or after leaving (PART, KICK) the last shared
    /// channel, the server stops sending updates and the nickname may pass
    /// to someone else, so the avatar is dropped, and so is a lookup. NICK
    /// moves both. Our own avatar stays while we are connected. A live JOIN
    /// (not `replayed`) of a user we share no other channel with schedules
    /// a lookup.
    pub(crate) fn lifecycle(
        &mut self,
        message: &IrcMessage,
        presence: &PresenceIndex,
        current_nick: &str,
        replayed: bool,
        now: Instant,
    ) -> Handled {
        let mut handled = Handled::default();
        let Some(actor) = message.source_nickname() else {
            return handled;
        };
        let ours = |nick: &str| same_nickname(nick, current_nick);
        // A lookup for a name someone renamed onto is left alone: it was
        // scheduled for that user just now.
        let renamed = matches!(message.command, IrcCommand::NICK(_));
        let gone: Vec<String> = match &message.command {
            IrcCommand::JOIN(..) => {
                if self.subscribed
                    && !replayed
                    && !ours(actor)
                    && !presence.shares(actor, None)
                    && let Some(note) = self.schedule_lookup(actor, now)
                {
                    handled.notes.push(note);
                }
                Vec::new()
            }
            IrcCommand::NICK(new) => {
                self.rename_lookup(actor, new, now);
                if self.known.remove(&nickname_key(actor)) {
                    self.known.insert(nickname_key(new));
                    handled.events.push(Event::AvatarMoved {
                        from: actor.to_owned(),
                        to: new.clone(),
                    });
                    return handled;
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
                    let mut only = presence.only_in(channel);
                    only.retain(|nick| !ours(nick));
                    only
                } else if presence.shares(leaving, Some(channel)) {
                    Vec::new()
                } else {
                    vec![leaving.to_owned()]
                }
            }
            _ => Vec::new(),
        };
        if !renamed {
            for nick in &gone {
                self.cancel_lookup(&nickname_key(nick), now);
            }
        }
        handled.events.extend(
            gone.into_iter()
                .filter(|nick| self.known.remove(&nickname_key(nick)))
                .map(|nickname| Event::UserAvatar {
                    nickname,
                    url: None,
                }),
        );
        handled
    }

    fn schedule_lookup(&mut self, nickname: &str, now: Instant) -> Option<String> {
        let key = nickname_key(nickname);
        if self.known.contains(&key) {
            return None;
        }
        if let Some(lookup) = self.lookups.iter_mut().find(|lookup| lookup.key == key) {
            // Back before the answer came: the answer now describes them.
            if lookup.state == LookupState::Abandoned {
                lookup.state = LookupState::Sent;
                lookup.due = now + LOOKUP_TIMEOUT;
            }
            return None;
        }
        if self.lookups.len() >= MAX_LOOKUPS {
            self.lookups_skipped += 1;
            return (self.lookups_skipped == 1).then(|| {
                format!("Too many avatar lookups pending; not looking up {nickname} and later joiners for now.")
            });
        }
        self.lookups.push(Lookup {
            nickname: nickname.to_owned(),
            key,
            state: LookupState::Waiting,
            due: now + LOOKUP_DELAY,
            attempts: 0,
        });
        None
    }

    /// The user left: a lookup not sent yet is dropped, a sent one's answer
    /// will be ignored.
    fn cancel_lookup(&mut self, key: &str, now: Instant) {
        if let Some(index) = self.lookups.iter().position(|lookup| lookup.key == key) {
            let lookup = &mut self.lookups[index];
            match lookup.state {
                LookupState::Waiting => {
                    self.lookups.remove(index);
                }
                LookupState::Sent => {
                    lookup.state = LookupState::Abandoned;
                    lookup.due = now + LOOKUP_TIMEOUT;
                }
                LookupState::Abandoned => {}
            }
        }
    }

    /// A user with a pending lookup changed name: look them up under the
    /// new one.
    fn rename_lookup(&mut self, from: &str, to: &str, now: Instant) {
        let key = nickname_key(from);
        let Some(lookup) = self.lookups.iter().find(|lookup| lookup.key == key) else {
            return;
        };
        if lookup.state == LookupState::Abandoned {
            return;
        }
        let due = lookup.due;
        let waiting = lookup.state == LookupState::Waiting;
        self.cancel_lookup(&key, now);
        let to_key = nickname_key(to);
        if !self.lookups.iter().any(|lookup| lookup.key == to_key) {
            self.lookups.push(Lookup {
                nickname: to.to_owned(),
                key: to_key,
                state: LookupState::Waiting,
                due: if waiting { due } else { now },
                attempts: 0,
            });
        }
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
            .find(|sync| same_channel(&sync.channel, channel))
        {
            sync.due = sync.due.max(due);
            return None;
        }
        let attempts = self
            .attempted
            .iter()
            .filter(|done| same_channel(done, channel))
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

/// Checks a value we are about to publish as our avatar: the same rules as
/// received values, a length that fits one IRC line, and no leading `:`.
/// Whether it is an acceptable URL is the caller's policy.
pub fn publishable_avatar(value: &str, utf8: bool) -> Result<(), String> {
    if value.len() > MAX_PUBLISHED_AVATAR_BYTES {
        return Err(format!(
            "Avatar URL is longer than {MAX_PUBLISHED_AVATAR_BYTES} bytes."
        ));
    }
    if value.starts_with(':') || avatar_value(value, utf8).is_none() {
        return Err(if !utf8 && !value.is_ascii() {
            "Only ASCII avatar URLs can be published with this server's encoding.".into()
        } else {
            "Avatar URL must not be empty or contain spaces or control characters.".into()
        });
    }
    Ok(())
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
pub(crate) fn numeric(message: &IrcMessage) -> Option<(u16, &[String])> {
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
    use std::collections::HashMap;

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

    fn wire(commands: &[IrcCommand]) -> Vec<String> {
        commands.iter().map(String::from).collect()
    }

    /// A state that has subscribed and received the answer about our own
    /// avatar (none), as after a normal registration.
    fn ready(now: Instant) -> MetadataState {
        let mut state = MetadataState::new(true);
        state.start(now);
        state.observe(&line(":srv 766 me me avatar :not set"), now, "me", joined);
        state
    }

    fn observe(state: &mut MetadataState, text: &str, now: Instant) -> Vec<Event> {
        state
            .observe(&line(text), now, "me", joined)
            .unwrap()
            .events
    }

    #[test]
    fn subscribes_once_and_asks_for_our_own_avatar() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        assert_eq!(
            wire(&state.start(now)),
            ["METADATA * SUB avatar", "METADATA * GET avatar"]
        );
        assert!(state.start(now).is_empty());
        // The answer reports our state without claiming any request.
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me me avatar * :https://example.com/me",
                now
            ),
            [
                Event::OwnAvatar {
                    url: Some("https://example.com/me".into()),
                    request: None
                },
                Event::UserAvatar {
                    nickname: "me".into(),
                    url: Some("https://example.com/me".into())
                }
            ]
        );
        assert!(state.reset().is_empty());
        assert_eq!(state.start(now).len(), 2, "again after re-enabling");
    }

    #[test]
    fn follows_avatar_values_updates_and_removals() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        let observe =
            |state: &mut MetadataState, text: &str| state.observe(&line(text), now, "me", joined);
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
        // Ergo announces changes as numerics addressed to `*`.
        assert_eq!(
            avatars(observe(
                &mut state,
                ":srv 761 * erin avatar * :https://example.com/e.png"
            )),
            [("erin".into(), Some("https://example.com/e.png".into()))]
        );
        assert_eq!(
            avatars(observe(&mut state, ":srv 766 * erin avatar :Key deleted")),
            [("erin".into(), None)]
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
            ":srv 761 me me display-name * :Me",
        ] {
            let handled = state.observe(&line(text), now, "me", joined).unwrap();
            assert!(handled.events.is_empty(), "{text}");
        }
        // Unusable values replace the avatar with none.
        let long = format!("https://example.com/{}", "a".repeat(MAX_AVATAR_BYTES));
        for value in [long.as_str(), "https://example.com/a b.png", "\u{1}x"] {
            let valid = ":srv METADATA alice avatar * :https://example.com/a.png";
            state.observe(&line(valid), now, "me", joined);
            let text = format!(":srv METADATA alice avatar * :{value}");
            assert_eq!(
                avatars(state.observe(&line(&text), now, "me", joined)),
                [("alice".into(), None)],
                "{value}"
            );
        }
        // Not metadata: handled as before.
        for text in [
            ":srv 001 me :Welcome",
            ":a!u@h PRIVMSG #chan :hi",
            "FAIL CHATHISTORY INVALID :no",
        ] {
            assert!(state.observe(&line(text), now, "me", joined).is_none());
        }
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
        // Publishing follows the same rule.
        assert!(publishable_avatar("https://例え.jp/a.png", true).is_ok());
        assert!(publishable_avatar("https://例え.jp/a.png", false).is_err());
    }

    #[test]
    fn published_values_cannot_inject_commands_or_overflow_the_line() {
        for bad in [
            "",
            "https://example.com/a.png\r\nQUIT",
            "https://example.com/a.png\nPRIVMSG #x :hi",
            "https://example.com/a\0.png",
            "https://example.com/a b.png",
            ":https://example.com/a.png",
            "https://example.com/\u{7f}",
        ] {
            assert!(publishable_avatar(bad, true).is_err(), "{bad:?}");
        }
        let long = format!(
            "https://example.com/{}",
            "a".repeat(MAX_PUBLISHED_AVATAR_BYTES)
        );
        assert!(publishable_avatar(&long, true).is_err());
        let fits = format!(
            "https://example.com/{}",
            "a".repeat(MAX_PUBLISHED_AVATAR_BYTES - 20)
        );
        assert!(publishable_avatar(&fits, true).is_ok());
        // The longest accepted value still fits one IRC line.
        let command = metadata_command(&["*", "SET", AVATAR_KEY, &fits]);
        assert!(String::from(&command).len() + 2 <= 512);
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
            let handled = state.observe(&line(text), now, "me", joined).unwrap();
            assert!(handled.events.is_empty());
            assert_eq!(handled.notes.len(), 1, "{text}");
        }
    }

    #[test]
    fn our_own_avatar_needs_registration_and_one_request_at_a_time() {
        let now = Instant::now();
        let mut state = MetadataState::new(true);
        let url = Some("https://example.com/me.png");
        assert_eq!(
            state.request_own(1, url, true, now),
            Err(AvatarRequestFailure::Unavailable),
            "not subscribed yet"
        );
        state.start(now);
        assert_eq!(
            state.request_own(1, url, false, now),
            Err(AvatarRequestFailure::Unavailable),
            "capability or registration missing"
        );
        assert_eq!(
            state.request_own(1, url, true, now),
            Err(AvatarRequestFailure::Busy),
            "our own query is still out"
        );
        observe(&mut state, ":srv 766 me me avatar :not set", now);
        let set = state.request_own(2, url, true, now).unwrap();
        assert_eq!(
            String::from(&set),
            "METADATA * SET avatar https://example.com/me.png"
        );
        assert_eq!(
            state.request_own(3, None, true, now),
            Err(AvatarRequestFailure::Busy),
            "repeated clicks do not stack up"
        );
    }

    #[test]
    fn publishing_and_removing_follow_the_servers_answers() {
        let now = Instant::now();
        let mut state = ready(now);
        let own = |url: Option<&str>, request: Option<u64>| Event::OwnAvatar {
            url: url.map(str::to_owned),
            request,
        };
        let mine = |url: Option<&str>| Event::UserAvatar {
            nickname: "me".into(),
            url: url.map(str::to_owned),
        };

        // Confirmed by 761 for our nickname; the server may rewrite it.
        state
            .request_own(1, Some("https://example.com/a.png"), true, now)
            .unwrap();
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me me avatar * :https://cdn.example/a.png",
                now
            ),
            [
                own(Some("https://cdn.example/a.png"), Some(1)),
                mine(Some("https://cdn.example/a.png"))
            ]
        );
        // The draft's own example answers with `*` as the target.
        state
            .request_own(2, Some("https://example.com/b.png"), true, now)
            .unwrap();
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me * avatar * :https://example.com/b.png",
                now
            )[0],
            own(Some("https://example.com/b.png"), Some(2))
        );

        // Removal sends SET without a value (never CLEAR) and ends with 766.
        let remove = state.request_own(3, None, true, now).unwrap();
        assert_eq!(String::from(&remove), "METADATA * SET avatar");
        assert_eq!(
            observe(&mut state, ":srv 766 me me avatar :Key deleted", now),
            [own(None, Some(3)), mine(None)]
        );
        // Removing a key that is not set is still a removal.
        state.request_own(4, None, true, now).unwrap();
        assert_eq!(
            observe(
                &mut state,
                "FAIL METADATA KEY_NOT_SET me avatar :key not set",
                now
            ),
            [own(None, Some(4))]
        );
    }

    #[test]
    fn rejections_rate_limits_and_timeouts_end_the_request() {
        let now = Instant::now();
        let mut state = ready(now);
        let failed = |request, failure| Event::OwnAvatarFailed { request, failure };
        let url = Some("https://example.com/a.png");

        // A warning about another key does not answer our request.
        state.request_own(1, url, true, now).unwrap();
        assert!(
            observe(
                &mut state,
                "FAIL METADATA KEY_NO_PERMISSION me secret :denied",
                now
            )
            .is_empty()
        );
        // Ergo's code names and the draft's are both recognized.
        assert_eq!(
            observe(
                &mut state,
                "FAIL METADATA INVALID_VALUE avatar :Value is too long",
                now
            ),
            [failed(
                1,
                AvatarRequestFailure::Rejected {
                    code: "INVALID_VALUE".into(),
                    description: "Value is too long".into()
                }
            )]
        );
        state.request_own(2, url, true, now).unwrap();
        assert_eq!(
            observe(
                &mut state,
                "FAIL METADATA VALUE_INVALID :value is too long or not UTF8",
                now
            )[0],
            failed(
                2,
                AvatarRequestFailure::Rejected {
                    code: "VALUE_INVALID".into(),
                    description: "value is too long or not UTF8".into()
                }
            )
        );
        state.request_own(3, url, true, now).unwrap();
        assert_eq!(
            observe(
                &mut state,
                "FAIL METADATA RATE_LIMITED me avatar 5 :slow down",
                now
            ),
            [failed(
                3,
                AvatarRequestFailure::RateLimited {
                    retry_after: Some(5)
                }
            )]
        );
        state.request_own(4, url, true, now).unwrap();
        assert_eq!(
            observe(
                &mut state,
                "FAIL METADATA RATE_LIMITED * avatar * :slow down",
                now
            ),
            [failed(
                4,
                AvatarRequestFailure::RateLimited { retry_after: None }
            )]
        );
        state.request_own(5, url, true, now).unwrap();
        assert_eq!(
            observe(&mut state, "FAIL METADATA FORBIDDEN * :Only operators", now)[0],
            failed(
                5,
                AvatarRequestFailure::Rejected {
                    code: "FORBIDDEN".into(),
                    description: "Only operators".into()
                }
            )
        );

        // No answer: the request fails once, and a late answer only
        // reports the server's state.
        state.request_own(6, url, true, now).unwrap();
        assert_eq!(state.next_deadline(), Some(now + OWN_REQUEST_TIMEOUT));
        assert!(
            state
                .tick(now + Duration::from_secs(1), joined)
                .events
                .is_empty()
        );
        assert_eq!(
            state.tick(now + OWN_REQUEST_TIMEOUT, joined).events,
            [failed(6, AvatarRequestFailure::NoReply)]
        );
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me me avatar * :https://example.com/a.png",
                now
            )[0],
            Event::OwnAvatar {
                url: Some("https://example.com/a.png".into()),
                request: None
            },
            "stale answer confirms nothing"
        );

        // Losing the capability fails a request in progress.
        state.request_own(7, None, true, now).unwrap();
        assert_eq!(
            state.reset(),
            [failed(7, AvatarRequestFailure::CapabilityLost)]
        );
        assert_eq!(
            state.request_own(8, None, true, now),
            Err(AvatarRequestFailure::Unavailable)
        );
    }

    #[test]
    fn changes_made_elsewhere_do_not_answer_our_request() {
        let now = Instant::now();
        let mut state = ready(now);
        state
            .request_own(1, Some("https://example.com/mine.png"), true, now)
            .unwrap();
        // Another client of ours changed it (a METADATA message, or Ergo's
        // numeric to `*`): reported, but our request is still open.
        for text in [
            ":srv METADATA me avatar * :https://example.com/other.png",
            ":srv 761 * me avatar * :https://example.com/other.png",
        ] {
            assert_eq!(
                observe(&mut state, text, now)[0],
                Event::OwnAvatar {
                    url: Some("https://example.com/other.png".into()),
                    request: None
                }
            );
        }
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me me avatar * :https://example.com/mine.png",
                now
            )[0],
            Event::OwnAvatar {
                url: Some("https://example.com/mine.png".into()),
                request: Some(1)
            }
        );
    }

    #[test]
    fn deferred_synchronization_is_bounded() {
        let mut state = MetadataState::new(true);
        let start = Instant::now();
        let defer = |state: &mut MetadataState, text: &str, now| {
            state.observe(&line(text), now, "me", joined).unwrap()
        };
        defer(&mut state, ":srv 774 me #chan 4", start);
        // Not joined, or not a channel (including Ergo's `*ALL`): ignored.
        defer(&mut state, ":srv 774 me #elsewhere 4", start);
        defer(&mut state, ":srv 774 me alice 4", start);
        defer(&mut state, ":srv 774 * *ALL 0", start);
        assert_eq!(state.pending_syncs(), [("#chan".into(), 1)]);
        assert_eq!(state.next_deadline(), Some(start + Duration::from_secs(4)));
        assert!(state.tick(start, joined).send.is_empty());
        let sent = state.tick(start + Duration::from_secs(4), joined).send;
        assert_eq!(wire(&sent), ["METADATA #chan SYNC"]);
        assert_eq!(state.next_deadline(), None);

        // Deferred again twice more, then the client stops asking.
        for attempt in 2..=3 {
            defer(&mut state, ":srv 774 me #chan 1", start);
            assert_eq!(state.pending_syncs(), [("#chan".into(), attempt)]);
            state.tick(start + Duration::from_secs(10), joined);
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
        let sent = state.tick(start + MAX_SYNC_DELAY, |_| false).send;
        assert!(sent.is_empty(), "parted channels are not synchronized");
        defer(&mut state, ":srv 774 me #big2 5", start);
        state.reset();
        assert_eq!((state.next_deadline(), state.attempted.len()), (None, 0));
    }

    #[test]
    fn avatars_end_with_the_last_shared_channel_and_are_bounded() {
        let mut state = MetadataState::new(true);
        let now = Instant::now();
        for nick in ["me", "bob", "carol", "dave"] {
            let text = format!(":srv METADATA {nick} avatar * :https://example.com/{nick}");
            state.observe(&line(&text), now, "me", joined);
        }
        let rosters = PresenceIndex::from_rosters(&HashMap::from([
            (
                "#a".to_owned(),
                vec!["@me".into(), "bob".into(), "+carol".into()],
            ),
            (
                "#b".to_owned(),
                vec!["me".into(), "carol".into(), "dave".into()],
            ),
        ]));
        let gone = |state: &mut MetadataState, text: &str| -> Vec<String> {
            state
                .lifecycle(&line(text), &rosters, "me", false, now)
                .events
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
            "me",
            joined,
        );
        assert_eq!(gone(&mut state, ":frank!u@h NICK erin"), ["erin"]);

        // At most MAX_AVATAR_USERS users; later ones get none.
        let mut state = MetadataState::new(true);
        for n in 0..MAX_AVATAR_USERS + 10 {
            let text = format!(":srv METADATA u{n} avatar * :https://example.com/{n}");
            state.observe(&line(&text), now, "me", joined);
        }
        assert_eq!(state.known.len(), MAX_AVATAR_USERS);
        let late = format!(":srv METADATA u{MAX_AVATAR_USERS} avatar * :https://example.com/x");
        assert!(avatars(state.observe(&line(&late), now, "me", joined)).is_empty());
        assert_eq!(
            avatars(state.observe(
                &line(":srv METADATA u0 avatar * :https://e.example/y"),
                now,
                "me",
                joined
            )),
            [("u0".into(), Some("https://e.example/y".into()))],
            "known users still update"
        );
    }

    fn rosters() -> PresenceIndex {
        PresenceIndex::from_rosters(&HashMap::from([
            ("#chan".to_owned(), vec!["@me".into(), "carol".into()]),
            ("#big1".to_owned(), vec!["me".into()]),
        ]))
    }

    fn join(state: &mut MetadataState, nick: &str, now: Instant) -> Handled {
        state.lifecycle(
            &line(&format!(":{nick}!u@h JOIN #chan")),
            &rosters(),
            "me",
            false,
            now,
        )
    }

    fn sent(state: &mut MetadataState, now: Instant) -> Vec<String> {
        wire(&state.tick(now, joined).send)
    }

    #[test]
    fn channel_names_compare_alike_when_leaving() {
        let now = Instant::now();
        let mut state = MetadataState::new(true);
        for nick in ["bob", "dave"] {
            let text = format!(":srv METADATA {nick} avatar * :https://example.com/{nick}");
            state.observe(&line(&text), now, "me", joined);
        }
        let rosters = PresenceIndex::from_rosters(&HashMap::from([
            ("#Room".to_owned(), vec!["me".into(), "bob".into()]),
            ("#Other".to_owned(), vec!["me".into(), "dave".into()]),
        ]));
        let gone = |state: &mut MetadataState, text: &str| -> usize {
            state
                .lifecycle(&line(text), &rosters, "me", false, now)
                .events
                .len()
        };
        // Spelled differently from the roster, still the same channel.
        assert_eq!(gone(&mut state, ":bob!u@h PART #room"), 1);
        assert_eq!(gone(&mut state, ":me!u@h PART #OTHER"), 1, "dave only here");
    }

    #[test]
    fn later_joiners_are_looked_up_once_after_a_pause() {
        let now = Instant::now();
        let mut state = ready(now);
        join(&mut state, "bob", now);
        assert_eq!(state.next_deadline(), Some(now + LOOKUP_DELAY));
        assert!(sent(&mut state, now + Duration::from_secs(1)).is_empty());
        // Joining again, or a burst of repeated JOINs, adds nothing.
        join(&mut state, "BOB", now);
        assert_eq!(
            sent(&mut state, now + LOOKUP_DELAY),
            ["METADATA bob GET avatar"]
        );
        assert!(sent(&mut state, now + Duration::from_secs(10)).is_empty());
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me bob avatar * :https://example.com/b",
                now
            ),
            [Event::UserAvatar {
                nickname: "bob".into(),
                url: Some("https://example.com/b".into())
            }]
        );
        assert_eq!(state.next_deadline(), None, "nothing left to do");

        // Users with a known avatar, users sharing another channel, our own
        // JOIN, replayed history and joins before subscribing: no lookup.
        join(&mut state, "bob", now);
        join(&mut state, "carol", now);
        join(&mut state, "me", now);
        state.lifecycle(&line(":dave!u@h JOIN #chan"), &rosters(), "me", true, now);
        let mut fresh = MetadataState::new(true);
        join(&mut fresh, "erin", now);
        assert!(state.lookups.is_empty() && fresh.lookups.is_empty());
    }

    #[test]
    fn announced_or_departed_joiners_are_not_looked_up() {
        let now = Instant::now();
        let later = now + LOOKUP_DELAY;
        let mut state = ready(now);
        // The server announced the avatar (or its absence) by itself.
        join(&mut state, "bob", now);
        observe(
            &mut state,
            ":srv METADATA bob avatar * :https://example.com/b",
            now,
        );
        join(&mut state, "carol2", now);
        observe(&mut state, ":srv 766 * carol2 avatar :not set", now);
        // Left before the lookup was sent.
        join(&mut state, "dave", now);
        state.lifecycle(&line(":dave!u@h PART #chan"), &rosters(), "me", false, now);
        join(&mut state, "erin", now);
        state.lifecycle(&line(":erin!u@h QUIT :bye"), &rosters(), "me", false, now);
        assert!(sent(&mut state, later).is_empty());
        assert!(state.lookups.is_empty());
    }

    #[test]
    fn answers_for_users_who_left_are_dropped_unless_the_name_is_back() {
        let now = Instant::now();
        let later = now + LOOKUP_DELAY;
        let mut state = ready(now);
        join(&mut state, "bob", now);
        assert_eq!(sent(&mut state, later), ["METADATA bob GET avatar"]);
        state.lifecycle(&line(":bob!u@h QUIT :bye"), &rosters(), "me", false, later);
        assert!(
            observe(
                &mut state,
                ":srv 761 me bob avatar * :https://example.com/old",
                later
            )
            .is_empty(),
            "departed user: answer ignored"
        );

        // Someone takes the name again before the answer: the answer came
        // after their JOIN, so it describes them.
        join(&mut state, "carol2", now);
        let t = later + LOOKUP_INTERVAL;
        assert_eq!(sent(&mut state, t), ["METADATA carol2 GET avatar"]);
        state.lifecycle(&line(":carol2!u@h PART #chan"), &rosters(), "me", false, t);
        join(&mut state, "carol2", t);
        assert_eq!(
            observe(
                &mut state,
                ":srv 761 me carol2 avatar * :https://example.com/new",
                t
            ),
            [Event::UserAvatar {
                nickname: "carol2".into(),
                url: Some("https://example.com/new".into())
            }]
        );
    }

    #[test]
    fn lookups_are_bounded_spaced_and_time_out() {
        let now = Instant::now();
        let mut state = ready(now);
        let mut notes = 0;
        for n in 0..200 {
            notes += join(&mut state, &format!("u{n}"), now).notes.len();
        }
        assert_eq!(state.lookups.len(), MAX_LOOKUPS);
        assert_eq!(notes, 1, "one note for the whole burst");

        // At most one request per interval, and a bounded number in flight.
        let mut t = now + LOOKUP_DELAY;
        let mut requests = 0;
        for _ in 0..40 {
            requests += sent(&mut state, t).len();
            t += Duration::from_millis(100);
        }
        assert_eq!(
            requests, MAX_LOOKUPS_IN_FLIGHT,
            "4 s at 2/s, capped in flight"
        );
        assert_eq!(state.in_flight(), MAX_LOOKUPS_IN_FLIGHT);

        // Unanswered: abandoned after the timeout, freeing the slots; a
        // late answer is dropped, then the entry goes away.
        let first_sent = now + LOOKUP_DELAY;
        let timeout = first_sent + LOOKUP_TIMEOUT;
        // The first one is given up and its slot reused at once.
        assert_eq!(state.tick(timeout, joined).send.len(), 1);
        assert_eq!(state.in_flight(), MAX_LOOKUPS_IN_FLIGHT);
        assert!(
            observe(
                &mut state,
                ":srv 761 me u0 avatar * :https://example.com/0",
                timeout
            )
            .is_empty()
        );
        // Everything drains eventually; nothing is retried after a timeout.
        let mut t = timeout + LOOKUP_TIMEOUT * 3;
        let mut requests = 0;
        for _ in 0..2000 {
            requests += sent(&mut state, t).len();
            t += Duration::from_millis(500);
        }
        assert!(requests <= MAX_LOOKUPS, "{requests}");
        assert!(state.lookups.is_empty());
        assert_eq!(state.next_deadline(), None);
        // With the table empty, the next overflow is reported again.
        assert!(join(&mut state, "late", t).notes.is_empty());
    }

    #[test]
    fn rate_limits_and_failures_end_or_postpone_lookups() {
        let now = Instant::now();
        let mut state = ready(now);
        join(&mut state, "bob", now);
        join(&mut state, "dave", now);
        let t = now + LOOKUP_DELAY;
        assert_eq!(sent(&mut state, t), ["METADATA bob GET avatar"]);
        // Rate limited: retried once after the server's delay, and every
        // lookup waits for it.
        observe(
            &mut state,
            "FAIL METADATA RATE_LIMITED bob avatar 10 :slow",
            t,
        );
        assert!(sent(&mut state, t + Duration::from_secs(5)).is_empty());
        let resumed = t + Duration::from_secs(10);
        assert_eq!(sent(&mut state, resumed).len(), 1);
        assert_eq!(
            sent(&mut state, resumed + LOOKUP_INTERVAL).len(),
            1,
            "both users asked again"
        );
        observe(
            &mut state,
            "FAIL METADATA RATE_LIMITED bob avatar 10 :slow",
            resumed,
        );
        observe(
            &mut state,
            "FAIL METADATA INVALID_TARGET dave :no such nick",
            resumed,
        );
        assert!(
            state.lookups.is_empty(),
            "second limit and bad targets end it"
        );
    }

    #[test]
    fn nickname_changes_follow_the_lookup() {
        let now = Instant::now();
        let mut state = ready(now);
        join(&mut state, "bob", now);
        state.lifecycle(&line(":bob!u@h NICK robert"), &rosters(), "me", false, now);
        assert_eq!(
            sent(&mut state, now + LOOKUP_DELAY),
            ["METADATA robert GET avatar"]
        );
        // Renamed again after the request: the old answer is dropped and
        // the new name asked.
        state.lifecycle(
            &line(":robert!u@h NICK rob"),
            &rosters(),
            "me",
            false,
            now + LOOKUP_DELAY,
        );
        assert!(
            observe(
                &mut state,
                ":srv 761 me robert avatar * :https://example.com/r",
                now + LOOKUP_DELAY
            )
            .is_empty()
        );
        assert_eq!(
            sent(&mut state, now + LOOKUP_DELAY + LOOKUP_INTERVAL),
            ["METADATA rob GET avatar"]
        );
    }
}
