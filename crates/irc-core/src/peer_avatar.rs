// SPDX-License-Identifier: MPL-2.0
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
//!   text follows unchanged. A connection in ISO-2022-JP cannot read SI and
//!   shows U+FFFD in its place, so `\x03<digit>U+FFFD` counts as the mark
//!   too (D025).
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
//! no channel-wide WHO and no query to users without the mark. IRCnet's
//! ircd (2.11 and later) puts the server's ID between the hop count and the
//! realname in 352 (`:<hops> <sid> <realname>`, the ID being a digit and
//! three of `[0-9A-Z]`; `s_user.c` `who_one`). Bouncers (Tiarra) do not
//! pass 042 (RPL_YOURID) on, so the ID is not tied to it: a realname that
//! does not start with a mark as it is gets that word skipped when it has
//! this shape and something follows, and is checked again. A 315 or 401
//! name may carry `@network` (Tiarra); it is matched before the `@`.
//! Lookups and
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
//! ([`marked_realname`]): KVIrc's, or on ISO-2022-JP,
//! CayenChat's invisible one (there is no switch); the shared URL can change while
//! connected.
//!
//! A user who queried us is remembered (bounded, only while they share a
//! channel with us; gone on QUIT or leaving, followed on NICK). When the
//! shared URL changes, or stops, each of them is told with a private
//! `NOTICE <nick> :\x01AVATAR <url>\x01` (`\x01AVATAR\x01` for none), one per
//! two seconds, only the latest state. Nothing goes to channels: clients
//! that do not speak CTCP AVATAR would show it. Someone who queried us also
//! speaks CTCP AVATAR whatever their realname shows, so they are queried
//! once in return (the usual queue and bounds; not if they were queried or
//! answered already, or have a metadata avatar), and the two clients do not
//! keep asking each other (D025).
//!
//! Pictures are tied to a mask, not to presence (D058). CTCP AVATAR is a
//! stop-gap for servers without metadata, so it is not held to the
//! occupancy rules metadata follows. A picture learned from a user is
//! reported as [`Event::PeerAvatar`] (the application keeps it for the
//! name) and remembered in a [`PeerAvatarCache`] under the mask
//! `nick!user@host`, loosened as `avatar_mask` describes. The cache lives in
//! memory across connections of a network. Whoever is seen with a
//! remembered mask (a JOIN, a live message, ...) gets the picture at once,
//! without a lookup, and is still queried once: only a query makes the
//! sender tell us of a later change. Others are found as above. A NICK
//! carries the picture to the new name and mask, an answer without a
//! picture (or a lookup of ours finding no mark) drops it, and QUIT or
//! leaving ends only what this struct tracks about the user: the picture
//! stays with the name and the mask. Metadata is passed on untouched; it is
//! only noted here, so users who have a metadata avatar are not asked, and
//! the application prefers it. The per-user state lives in the connection's
//! worker, so a reconnect starts with the cache only.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use cayenchat_model::names::CaseMapping;
use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix};
use tokio::time::Instant;

use crate::{
    Event, PeerAvatarCache,
    avatar_mask::AvatarMask,
    metadata::{Handled, MAX_AVATAR_USERS, MAX_PUBLISHED_AVATAR_BYTES, avatar_value, numeric},
    presence::PresenceIndex,
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
/// Users who queried us and are told when what we share changes, and the
/// pause between two such notices.
const MAX_NOTIFIED_USERS: usize = MAX_REPLY_RECIPIENTS;
const NOTIFY_INTERVAL: Duration = PROBE_INTERVAL;

/// The realname sent at registration, with KVIrc's avatar mark when we
/// share an avatar.
pub(crate) fn realname(base: &str, advertise: bool) -> String {
    if advertise {
        format!("\u{3}{AVATAR_BIT}\u{f}{base}")
    } else {
        base.to_owned()
    }
}

/// Whether a connection in `encoding` (a WHATWG label) can carry KVIrc's
/// mark. ISO-2022-JP cannot: SI (0x0F) is its shift-in code and
/// `encoding_rs` turns it into U+FFFD on receiving and `&#65533;` on
/// sending.
pub(crate) fn carries_mark(encoding: &str) -> bool {
    encoding_rs::Encoding::for_label(encoding.as_bytes()) != Some(encoding_rs::ISO_2022_JP)
}

/// CayenChat's own mark, recognized when receiving only: the text
/// `\x03<digit>\x03` (nine printable ASCII characters, not control
/// characters) at the start of a realname. It survives every encoding and
/// Tiarra. We never add it; a user behind a bouncer types it into the
/// realname.
const TEXT_MARK_PREFIX: &str = "\\x03";
const TEXT_MARK_SUFFIX: &str = "\\x03";

/// CayenChat's invisible mark, added on ISO-2022-JP (D057): the raw control
/// codes bold on, reverse on, reverse off, bold off. They hold no digit and
/// ISO-2022-JP carries them; other clients show nothing for them. Its
/// presence always means "has an avatar".
pub(crate) const INVISIBLE_MARK: &str = "\u{2}\u{16}\u{16}\u{2}";

/// Which mark the realname sent in `USER` and `SETNAME` starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RealnameMark {
    None,
    /// KVIrc's ETX, digit and SI.
    Kvirc,
    /// [`INVISIBLE_MARK`].
    Invisible,
}

/// `base` with `mark` in front. The invisible mark replaces any mark the
/// user already typed, so it is never doubled.
pub(crate) fn marked_realname(base: &str, mark: RealnameMark) -> String {
    match mark {
        RealnameMark::None => base.to_owned(),
        RealnameMark::Kvirc => realname(base, true),
        RealnameMark::Invisible => format!("{INVISIBLE_MARK}{}", without_mark(base)),
    }
}

/// The digit of the textual mark at the start of `realname`, and the
/// length of the mark.
fn text_mark(realname: &str) -> Option<(u8, usize)> {
    let rest = realname.strip_prefix(TEXT_MARK_PREFIX)?;
    let (&digit, rest) = rest.as_bytes().split_first()?;
    (digit.is_ascii_digit() && rest.starts_with(TEXT_MARK_SUFFIX.as_bytes()))
        .then_some((digit, TEXT_MARK_PREFIX.len() + 1 + TEXT_MARK_SUFFIX.len()))
}

/// The digit of KVIrc's mark at the start of `realname`, and the length of
/// the mark: ETX, a digit `0`–`7`, and SI. A connection that cannot carry SI
/// (ISO-2022-JP) reads it as U+FFFD, so that form counts too, whatever the
/// connection's encoding: where SI is readable it never turns into U+FFFD.
fn kvirc_mark(realname: &str) -> Option<(u8, usize)> {
    let rest = realname.strip_prefix('\u{3}')?;
    let digit = *rest.as_bytes().first()?;
    if !(b'0'..=b'7').contains(&digit) {
        return None;
    }
    // The digit is ASCII, so index 1 is a character boundary.
    let rest = &rest[1..];
    if rest.starts_with('\u{f}') {
        Some((digit, 3))
    } else if rest.starts_with('\u{FFFD}') {
        Some((digit, 2 + '\u{FFFD}'.len_utf8()))
    } else {
        None
    }
}

/// A realname without its leading marks (KVIrc's ETX, a digit, SI or its
/// U+FFFD reading; the invisible one; the textual `\x03<digit>\x03`, in any
/// order and number), as the user configured it.
pub(crate) fn without_mark(mut realname: &str) -> &str {
    loop {
        let rest = if let Some(rest) = realname.strip_prefix(INVISIBLE_MARK) {
            rest
        } else if let Some((_, len)) = kvirc_mark(realname) {
            &realname[len..]
        } else if let Some((_, len)) = text_mark(realname) {
            &realname[len..]
        } else {
            return realname;
        };
        realname = rest;
    }
}

/// Whether a realname starts with a mark for an avatar: the invisible one,
/// KVIrc's, or the textual one, whose digit must also have the fours place
/// set.
pub(crate) fn advertises_avatar(realname: &str) -> bool {
    if realname.starts_with(INVISIBLE_MARK) {
        return true;
    }
    if let Some((digit, _)) = text_mark(realname) {
        return matches!(digit, b'4'..=b'7');
    }
    kvirc_mark(realname).is_some_and(|(digit, _)| (digit - b'0') & AVATAR_BIT != 0)
}

/// The realname after an IRCnet server ID (a digit and three of `[0-9A-Z]`,
/// then a space), or `real` unchanged when it does not start with one.
fn skip_server_id(real: &str) -> &str {
    match real.split_once(' ') {
        Some((id, rest))
            if id.len() == 4
                && id.as_bytes()[0].is_ascii_digit()
                && id.as_bytes()[1..]
                    .iter()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => real,
    }
}

/// A nickname in a reply's name parameter without the `@network` some
/// bouncers (Tiarra in multi-server mode) append; `@` is not valid in nicks.
fn without_network(name: &str) -> &str {
    name.split_once('@').map_or(name, |(nick, _)| nick)
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

/// What is known about each present user's avatars. The application shows
/// the metadata one when there is one; it is kept here to not ask such
/// users for a CTCP AVATAR.
#[derive(Debug)]
struct Sources {
    nickname: String,
    metadata: Option<String>,
    peer: Option<String>,
}

/// The username and host of a live message's sender, if its prefix has them.
pub(crate) fn speaker_identity(message: &IrcMessage) -> Option<(String, String)> {
    if !matches!(
        message.command,
        IrcCommand::PRIVMSG(..) | IrcCommand::NOTICE(..)
    ) {
        return None;
    }
    prefix_identity(message)
}

fn prefix_identity(message: &IrcMessage) -> Option<(String, String)> {
    match &message.prefix {
        Some(Prefix::Nickname(_, user, host)) if !host.is_empty() => {
            Some((user.clone(), host.clone()))
        }
        _ => None,
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
    /// Pictures by user mask, kept across connections (D058).
    cache: PeerAvatarCache,
    /// The server's CASEMAPPING, for the nickname in a cache key.
    casemapping: CaseMapping,
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
    /// Users who queried us and are present, by nickname key.
    askers: HashMap<String, Asker>,
    next_notify: Option<Instant>,
}

/// A user who queried us for our avatar.
#[derive(Debug)]
struct Asker {
    nickname: String,
    /// The URL they were last given (`None`: no avatar).
    known: Option<String>,
}

impl PeerAvatars {
    #[cfg(test)]
    pub(crate) fn new(utf8: bool, share: Option<String>) -> Self {
        Self::with_cache(utf8, share, PeerAvatarCache::default())
    }

    pub(crate) fn with_cache(utf8: bool, share: Option<String>, cache: PeerAvatarCache) -> Self {
        Self {
            utf8,
            share: share.filter(|url| shareable(url, utf8).is_ok()),
            cache,
            casemapping: CaseMapping::default(),
            avatars: HashMap::new(),
            stages: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: Vec::new(),
            next_probe: None,
            skipped: 0,
            replies: VecDeque::new(),
            replied: HashMap::new(),
            askers: HashMap::new(),
            next_notify: None,
        }
    }

    pub(crate) fn set_casemapping(&mut self, casemapping: CaseMapping) {
        self.casemapping = casemapping;
    }

    /// Changes the URL answered to queries; `None` stops answering. The
    /// realname mark sent at registration is not changed.
    pub(crate) fn set_share(&mut self, url: Option<String>) {
        self.share = url.filter(|url| shareable(url, self.utf8).is_ok());
    }

    /// A user who queried us is a client that speaks CTCP AVATAR: they are
    /// told of later changes, one by one ([`PeerAvatars::notify`]). What
    /// they are taken to know changes only when they are actually answered
    /// ([`PeerAvatars::mark_told`]), so an unanswered query never hides a
    /// pending change or clear.
    fn remember_asker(&mut self, sender: &str, presence: &PresenceIndex) {
        if !valid_sender(sender) || !presence.shares(sender, None) {
            return;
        }
        let key = nickname_key(sender);
        if !self.askers.contains_key(&key) && self.askers.len() >= MAX_NOTIFIED_USERS {
            return;
        }
        self.askers.entry(key).or_insert_with(|| Asker {
            nickname: sender.to_owned(),
            known: None,
        });
    }

    /// A user who queried us speaks CTCP AVATAR even when their realname
    /// mark did not reach us (a bouncer, ISO-2022-JP), so they are queried
    /// once too, through the usual queue. Someone already queried or who
    /// answered is not asked again, so two such clients do not keep asking
    /// each other; nor is someone with a metadata avatar.
    fn ask_back(&mut self, sender: &str, presence: &PresenceIndex) -> Option<String> {
        if !valid_sender(sender) || !presence.shares(sender, None) {
            return None;
        }
        let key = nickname_key(sender);
        let from_metadata = self
            .avatars
            .get(&key)
            .is_some_and(|sources| sources.metadata.is_some());
        if from_metadata || self.stages.get(&key) == Some(&Stage::Queried) {
            return None;
        }
        self.enqueue(ProbeKind::Query, sender, key)
    }

    /// Records that an asker was just given what we share now.
    fn mark_told(&mut self, sender: &str) {
        if let Some(asker) = self.askers.get_mut(&nickname_key(sender)) {
            asker.known = self.share.clone();
        }
    }

    /// Tells the next asker still present whose picture differs from what
    /// we share now, at most one per [`NOTIFY_INTERVAL`]. Changes made in
    /// between are folded: only the latest state is compared and sent, to
    /// the user alone, never to a channel.
    fn notify(
        &mut self,
        now: Instant,
        current_nick: &str,
        presence: &PresenceIndex,
    ) -> Option<IrcCommand> {
        if self.next_notify.is_some_and(|due| due > now) {
            return None;
        }
        loop {
            let key = self
                .askers
                .iter()
                .find(|(_, asker)| asker.known != self.share)
                .map(|(key, _)| key.clone())?;
            let asker = self.askers.get_mut(&key)?;
            if same_nickname(&asker.nickname, current_nick)
                || !valid_sender(&asker.nickname)
                || !presence.shares(&asker.nickname, None)
            {
                self.askers.remove(&key);
                continue;
            }
            asker.known = self.share.clone();
            self.next_notify = Some(now + NOTIFY_INTERVAL);
            let text = match &self.share {
                Some(url) => format!("\u{1}{CTCP_TAG} {url}\u{1}"),
                None => format!("\u{1}{CTCP_TAG}\u{1}"),
            };
            return Some(IrcCommand::NOTICE(asker.nickname.clone(), text));
        }
    }

    /// Handles CTCP AVATAR traffic and replies to our lookups. `Some` means
    /// the message is consumed: it must not become a chat row or server
    /// line. WHOIS and other people's WHO replies are only read.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        presence: &PresenceIndex,
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
                {
                    self.remember_asker(sender, presence);
                    handled.notes.extend(self.ask_back(sender, presence));
                    if let Some(url) = self.share.clone()
                        && self.may_reply(sender, now)
                    {
                        self.mark_told(sender);
                        handled.send.push(IrcCommand::NOTICE(
                            sender.to_owned(),
                            format!("\u{1}{CTCP_TAG} {url}\u{1}"),
                        ));
                    }
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
                    presence.shares(sender, None)
                } else {
                    valid_channel(target) && presence.in_channel(target, sender)
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
                if let Some((user, host)) = prefix_identity(message) {
                    self.cache.set(
                        &AvatarMask::new(self.casemapping, sender, &user, &host),
                        url.as_deref(),
                    );
                }
                handled.events.extend(self.set_peer(sender, url));
                Some(handled)
            }
            _ => self.numeric_reply(message, current_nick, presence, now),
        }
    }

    fn numeric_reply(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        presence: &PresenceIndex,
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
            // <flags> :<hops> <realname>; with a server ID (IRCnet)
            // :<hops> <sid> <realname>
            352 if args.len() >= 8 => {
                let nick = &args[5];
                let found = ours(ProbeKind::Who, nick);
                let stale = found.is_some_and(|index| self.in_flight[index].stale);
                let mut events = Vec::new();
                if !stale {
                    let trailing = args.last().map(String::as_str).unwrap_or_default();
                    let real = trailing.split_once(' ').map_or("", |(_, real)| real);
                    // Bouncers do not pass 042 on, so the ID is not tied to
                    // it: a word of its shape is skipped only when the
                    // realname does not start with a mark as it is.
                    let real = if advertises_avatar(real) {
                        real
                    } else {
                        skip_server_id(real)
                    };
                    // Only our own lookup can report what it found; another
                    // user's WHO stays a server line, with nothing to attach.
                    let identity = found.map(|_| (args[2].as_str(), args[3].as_str()));
                    events.extend(self.realname_seen(nick, real, identity, current_nick, presence));
                }
                found.map(|_| Handled {
                    events,
                    ..Handled::default()
                })
            }
            // RPL_ENDOFWHO: <me> <mask> :End of WHO
            315 => {
                let index = ours(ProbeKind::Who, without_network(args.get(1)?))?;
                self.in_flight.remove(index);
                Some(Handled::default())
            }
            // RPL_WHOISUSER: <me> <nick> <user> <host> * :<realname>. The
            // user asked for it; it is only read.
            311 if args.len() > 5 => {
                let real = args.last().map(String::as_str).unwrap_or_default();
                self.realname_seen(&args[1], real, None, current_nick, presence);
                None
            }
            // ERR_NOSUCHNICK for a lookup or query of ours: they are gone.
            401 => {
                let key = nickname_key(without_network(args.get(1)?));
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
    /// once, unless the server's metadata gave an avatar. A user without the
    /// mark has no CTCP AVATAR picture now: the name's and, when `identity`
    /// (username and host) is given, the mask's are dropped, which is
    /// reported.
    fn realname_seen(
        &mut self,
        nick: &str,
        real: &str,
        identity: Option<(&str, &str)>,
        current_nick: &str,
        presence: &PresenceIndex,
    ) -> Option<Event> {
        if same_nickname(nick, current_nick) || !valid_sender(nick) || !presence.shares(nick, None)
        {
            return None;
        }
        let key = nickname_key(nick);
        if !advertises_avatar(real) {
            if !self.stages.contains_key(&key) && self.stages.len() < MAX_PROBED_USERS {
                self.stages.insert(key, Stage::Seen);
            }
            let (user, host) = identity?;
            self.cache
                .set(&AvatarMask::new(self.casemapping, nick, user, host), None);
            self.set_peer(nick, None);
            return Some(Event::PeerAvatar {
                nickname: nick.to_owned(),
                url: None,
            });
        }
        let from_metadata = self
            .avatars
            .get(&key)
            .is_some_and(|sources| sources.metadata.is_some());
        if self.stages.get(&key) != Some(&Stage::Queried) && !from_metadata {
            self.enqueue(ProbeKind::Query, nick, key);
        }
        None
    }

    /// The username and host of a live message's sender, copied only for
    /// someone not looked at yet (the only case `remembered` can use), so
    /// the usual message copies nothing.
    pub(crate) fn unseen_speaker_identity(&self, message: &IrcMessage) -> Option<(String, String)> {
        let key = nickname_key(message.source_nickname()?);
        if self.stages.contains_key(&key) || self.avatars.contains_key(&key) {
            return None;
        }
        speaker_identity(message)
    }

    /// Someone's WHO reply (any WHO, not only our lookup) names a mask with
    /// a remembered picture: they get it without being asked, like a
    /// speaker. The line itself stays a server line.
    pub(crate) fn remembered_who(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        presence: &PresenceIndex,
    ) -> Option<Event> {
        let (352, args) = numeric(message)? else {
            return None;
        };
        // <me> <channel> <user> <host> <server> <nick> ...
        if args.len() < 8 {
            return None;
        }
        self.remembered(&args[5], &args[2], &args[3], current_nick, Some(presence))
    }

    /// A user seen with `user`@`host` (a live speaker, or someone who
    /// joined) whose mask has a remembered picture gets it at once, and is
    /// still asked once: the query is what makes them tell us of a later
    /// change (D025). With `presence` they must share a channel with us.
    pub(crate) fn remembered(
        &mut self,
        nick: &str,
        user: &str,
        host: &str,
        current_nick: &str,
        presence: Option<&PresenceIndex>,
    ) -> Option<Event> {
        if same_nickname(nick, current_nick)
            || !valid_sender(nick)
            || presence.is_some_and(|presence| !presence.shares(nick, None))
        {
            return None;
        }
        // Like `speaker`: anyone already looked at is not looked up again,
        // which also keeps this off the per-message path.
        let key = nickname_key(nick);
        if self.stages.contains_key(&key) || self.avatars.contains_key(&key) {
            return None;
        }
        let url = self
            .cache
            .get(&AvatarMask::new(self.casemapping, nick, user, host))?;
        // A full queue only skips the query; the picture is shown anyway.
        let _ = self.enqueue(ProbeKind::Query, nick, key);
        self.set_peer(nick, Some(url))
    }

    /// Someone spoke live where we can see them: look up their realname
    /// once, if nothing is known about them yet.
    pub(crate) fn speaker(
        &mut self,
        nick: &str,
        current_nick: &str,
        presence: &PresenceIndex,
    ) -> Option<String> {
        if same_nickname(nick, current_nick) || !valid_sender(nick) {
            return None;
        }
        let key = nickname_key(nick);
        if self.stages.contains_key(&key) || self.avatars.contains_key(&key) {
            return None;
        }
        if !presence.shares(nick, None) {
            return None;
        }
        self.enqueue(ProbeKind::Who, nick, key)
    }

    /// The user asked for `nick`'s avatar: queue a CTCP AVATAR query even
    /// if one was sent before. It still goes through the queue, so the
    /// spacing, the bound and the one-query-per-user rule apply.
    pub(crate) fn request(
        &mut self,
        nick: &str,
        current_nick: &str,
        presence: &PresenceIndex,
    ) -> Option<String> {
        if same_nickname(nick, current_nick) || !valid_sender(nick) {
            return None;
        }
        if !presence.shares(nick, None) {
            return Some(format!(
                "Not asking {nick} for an avatar: no channel is shared with them."
            ));
        }
        let key = nickname_key(nick);
        self.in_flight
            .retain(|probe| probe.kind != ProbeKind::Query || probe.key != key);
        self.enqueue(ProbeKind::Query, nick, key)
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
        let notice = self
            .askers
            .values()
            .any(|asker| asker.known != self.share)
            .then(|| self.next_notify.unwrap_or_else(Instant::now));
        send.into_iter().chain(expiry).chain(notice).min()
    }

    /// Gives up unanswered probes and sends the next one when due.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        current_nick: &str,
        presence: &PresenceIndex,
    ) -> Handled {
        let mut handled = Handled::default();
        handled
            .send
            .extend(self.notify(now, current_nick, presence));
        self.in_flight.retain(|probe| probe.deadline > now);
        if self.next_probe.is_some_and(|due| due > now)
            || self.in_flight.len() >= MAX_PROBES_IN_FLIGHT
        {
            return handled;
        }
        while let Some(mut probe) = self.queue.pop_front() {
            if same_nickname(&probe.nickname, current_nick)
                || !presence.shares(&probe.nickname, None)
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

    /// Follows who is present, judged from the membership published before
    /// `message`, like metadata's lifecycle: NICK moves a user's picture
    /// and what we asked; QUIT, or PART/KICK from the last shared channel,
    /// ends what we know and ask about them (their picture stays with the
    /// name and the mask in the application and the cache, D058). Call it
    /// before passing metadata's events for the same message through
    /// [`PeerAvatars::observe_metadata`].
    pub(crate) fn lifecycle(
        &mut self,
        message: &IrcMessage,
        presence: &PresenceIndex,
        current_nick: &str,
    ) -> Vec<Event> {
        let Some(actor) = message.source_nickname() else {
            return Vec::new();
        };
        let ours = |nick: &str| same_nickname(nick, current_nick);
        if let IrcCommand::NICK(new) = &message.command {
            return self.rename(actor, new, prefix_identity(message));
        }
        for nick in presence.departed(message, actor, ours) {
            self.forget(&nick);
        }
        Vec::new()
    }

    /// Someone who joined (live) and whose mask has a remembered picture
    /// gets it without being asked. The joiner is in the channel by
    /// definition, whatever the membership published so far says.
    pub(crate) fn joined(&mut self, message: &IrcMessage, current_nick: &str) -> Option<Event> {
        let IrcCommand::JOIN(..) = &message.command else {
            return None;
        };
        let (user, host) = prefix_identity(message)?;
        self.remembered(message.source_nickname()?, &user, &host, current_nick, None)
    }

    /// The user left: what we were asking about them is dropped (answers
    /// still out are swallowed unused).
    fn forget(&mut self, nick: &str) {
        let key = nickname_key(nick);
        self.stages.remove(&key);
        self.askers.remove(&key);
        self.queue.retain(|probe| probe.key != key);
        self.in_flight
            .retain(|probe| probe.kind == ProbeKind::Who || probe.key != key);
        for probe in &mut self.in_flight {
            if probe.key == key {
                probe.stale = true;
            }
        }
        self.avatars.remove(&key);
    }

    /// `identity` is the username and host of the NICK message's prefix; the
    /// picture is remembered for the new name's mask too.
    fn rename(&mut self, from: &str, to: &str, identity: Option<(String, String)>) -> Vec<Event> {
        let (from_key, to_key) = (nickname_key(from), nickname_key(to));
        if from_key == to_key {
            if let Some(sources) = self.avatars.get_mut(&from_key) {
                sources.nickname = to.to_owned();
            }
            if let Some(asker) = self.askers.get_mut(&from_key) {
                asker.nickname = to.to_owned();
            }
            return Vec::new();
        }
        // Anything under the new name belonged to someone who left it.
        self.forget(to);
        let mut events = Vec::new();
        if let Some(stage) = self.stages.remove(&from_key) {
            self.stages.insert(to_key.clone(), stage);
        }
        if let Some(mut asker) = self.askers.remove(&from_key)
            && valid_sender(to)
        {
            asker.nickname = to.to_owned();
            self.askers.insert(to_key.clone(), asker);
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
            // The picture goes with them: to the new name (the old one
            // keeps its last known picture) and to the new mask.
            if let Some(url) = &sources.peer {
                if let Some((user, host)) = &identity {
                    self.cache.set(
                        &AvatarMask::new(self.casemapping, to, user, host),
                        Some(url),
                    );
                }
                events.push(Event::PeerAvatar {
                    nickname: to.to_owned(),
                    url: Some(url.clone()),
                });
            }
            self.avatars.insert(to_key, sources);
        }
        events
    }

    /// Records a user's CTCP AVATAR picture (`None`: they have none) and
    /// reports it. It is reported even when unchanged here: the
    /// application keeps names across the user's absences, and this table
    /// does not.
    fn set_peer(&mut self, nickname: &str, url: Option<String>) -> Option<Event> {
        let key = nickname_key(nickname);
        if url.is_some() || self.avatars.contains_key(&key) {
            self.track(nickname, |sources| sources.peer = url.clone());
        }
        Some(Event::PeerAvatar {
            nickname: nickname.to_owned(),
            url,
        })
    }

    /// Applies one source's change. A new user beyond the bound is ignored.
    fn track(&mut self, nickname: &str, change: impl FnOnce(&mut Sources)) {
        let key = nickname_key(nickname);
        if !self.avatars.contains_key(&key) && self.avatars.len() >= MAX_AVATAR_USERS {
            return;
        }
        let sources = self.avatars.entry(key.clone()).or_insert_with(|| Sources {
            nickname: nickname.to_owned(),
            metadata: None,
            peer: None,
        });
        change(sources);
        sources.nickname = nickname.to_owned();
        if sources.metadata.is_none() && sources.peer.is_none() {
            self.avatars.remove(&key);
        }
    }

    /// Notes metadata's avatars, which win over ours in the application
    /// and make asking unnecessary. Moves were already made by
    /// [`PeerAvatars::lifecycle`].
    pub(crate) fn observe_metadata(&mut self, events: &[Event]) {
        for event in events {
            match event {
                Event::UserAvatar { nickname, url } => {
                    self.track(nickname, |sources| sources.metadata = url.clone());
                }
                Event::AvatarsReset => {
                    self.avatars.retain(|_, sources| {
                        sources.metadata = None;
                        sources.peer.is_some()
                    });
                }
                _ => {}
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "alice";

    fn line(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    /// `#a` holds us, kv (a KVIrc user), bob and carol; `#b` holds us and
    /// carol.
    fn rosters() -> PresenceIndex {
        PresenceIndex::from_rosters(&HashMap::from([
            (
                "#a".to_owned(),
                vec!["@alice".into(), "kv".into(), "+bob".into(), "carol".into()],
            ),
            ("#b".to_owned(), vec!["alice".into(), "carol".into()]),
        ]))
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
                Event::UserAvatar { nickname, url } | Event::PeerAvatar { nickname, url } => {
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
    fn the_textual_mark_advertises_an_avatar_at_the_start_only() {
        for digit in ['4', '5', '6', '7'] {
            assert!(
                advertises_avatar(&format!("\\x03{digit}\\x03Name")),
                "{digit}"
            );
        }
        for real in [
            "\\x030\\x03Name",
            "\\x033\\x03Name",
            "\\x038\\x03Name",
            "\\X034\\x03Name",
            "\\x034\\X03Name",
            "\\x03a\\x03Name",
            "\\x0344\\x03Name",
            "\\x034Name",
            "Name \\x034\\x03",
            " \\x034\\x03Name",
            "\\x034",
        ] {
            assert!(!advertises_avatar(real), "{real:?}");
        }
        assert!(advertises_avatar("\\x034\\x03"));
    }

    #[test]
    fn both_marks_are_stripped_to_show_the_realname() {
        assert_eq!(without_mark("\\x034\\x03Alice"), "Alice");
        assert_eq!(without_mark("\\x030\\x03Alice"), "Alice");
        assert_eq!(without_mark("\\x034\\x03"), "");
        assert_eq!(without_mark("\\X034\\x03Alice"), "\\X034\\x03Alice");
        assert_eq!(without_mark("Alice \\x034\\x03"), "Alice \\x034\\x03");
        assert_eq!(without_mark("\u{3}4\u{f}\\x034\\x03Bob"), "Bob");
    }

    #[test]
    fn the_invisible_mark_means_an_avatar_and_is_stripped_in_any_order() {
        let invisible = INVISIBLE_MARK;
        assert!(advertises_avatar(&format!("{invisible}Alice")));
        assert!(advertises_avatar(invisible));
        for real in [
            "\u{2}\u{16}Alice",
            "\u{2}\u{16}\u{2}\u{16}Alice",
            " \u{2}\u{16}\u{16}\u{2}Alice",
            "Alice\u{2}\u{16}\u{16}\u{2}",
        ] {
            assert!(!advertises_avatar(real), "{real:?}");
        }
        let kvirc = "\u{3}4\u{f}";
        let text = "\\x034\\x03";
        for marks in [
            [invisible, kvirc, text],
            [invisible, text, kvirc],
            [kvirc, invisible, text],
            [kvirc, text, invisible],
            [text, invisible, kvirc],
            [text, kvirc, invisible],
        ] {
            assert_eq!(without_mark(&format!("{}Bob", marks.concat())), "Bob");
        }
        assert_eq!(without_mark(&format!("{invisible}{invisible}Bob")), "Bob");
        assert_eq!(
            without_mark("Bob\u{2}\u{16}\u{16}\u{2}"),
            "Bob\u{2}\u{16}\u{16}\u{2}"
        );
        assert_eq!(
            marked_realname("\u{3}4\u{f}Bob", RealnameMark::Invisible),
            format!("{invisible}Bob")
        );
    }

    #[test]
    fn iso_2022_jp_cannot_carry_the_kvirc_mark_but_the_textual_one_survives() {
        assert!(carries_mark("UTF-8"));
        assert!(carries_mark("Shift_JIS"));
        assert!(carries_mark("EUC-JP"));
        assert!(!carries_mark("ISO-2022-JP"));
        let marked = realname("名前", true);
        let (bytes, _, _) = encoding_rs::ISO_2022_JP.encode(&marked);
        let (back, _) = encoding_rs::ISO_2022_JP.decode_without_bom_handling(&bytes);
        assert!(!advertises_avatar(&back), "the KVIrc mark is broken");
        let text = "\\x034\\x03日本語";
        let (bytes, _, lost) = encoding_rs::ISO_2022_JP.encode(text);
        assert!(!lost);
        let (back, _) = encoding_rs::ISO_2022_JP.decode_without_bom_handling(&bytes);
        assert_eq!(back, text);
        assert!(advertises_avatar(&back));
        assert_eq!(without_mark(&back), "日本語");
    }

    #[test]
    fn the_kvirc_mark_read_as_replacement_character_still_counts() {
        assert!(advertises_avatar("\u{3}4\u{FFFD}名前"));
        assert!(advertises_avatar("\u{3}7\u{FFFD}"));
        assert!(!advertises_avatar("\u{3}3\u{FFFD}名前"));
        assert!(!advertises_avatar("\u{3}8\u{FFFD}名前"));
        assert!(!advertises_avatar("\u{FFFD}4\u{FFFD}名前"));
        assert_eq!(without_mark("\u{3}4\u{FFFD}名前"), "名前");
        assert_eq!(without_mark("\u{3}3\u{FFFD}名前"), "名前");
        assert_eq!(without_mark("\u{3}4\u{FFFD}\u{3}4\u{f}Bob"), "Bob");
        assert_eq!(without_mark("\u{3}8\u{FFFD}名前"), "\u{3}8\u{FFFD}名前");
    }

    #[test]
    fn a_kvirc_mark_received_on_iso_2022_jp_is_recognized_in_a_who_reply() {
        // A UTF-8 client sends the mark's SI byte as it is.
        let sent = realname("name", true);
        let (decoded, _) = encoding_rs::ISO_2022_JP.decode_without_bom_handling(sent.as_bytes());
        assert!(decoded.starts_with("\u{3}4\u{FFFD}"), "{decoded:?}");
        let now = Instant::now();
        let mut peers = PeerAvatars::new(false, None);
        let rosters = rosters();
        peers.speaker("kv", ME, &rosters);
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO kv"]);
        let who = format!(":srv 352 alice #a ~kv host srv kv H :0 {decoded}");
        assert!(observe(&mut peers, &who, now).is_some());
        assert_eq!(
            wire(&peers.tick(now + PROBE_INTERVAL, ME, &rosters).send),
            ["PRIVMSG kv \u{1}AVATAR\u{1}"]
        );
    }

    #[test]
    fn a_user_who_queried_us_is_queried_once_in_return() {
        let now = Instant::now();
        let rosters = rosters();
        let mut peers = PeerAvatars::new(true, None);
        let query = ":kv!u@h PRIVMSG alice :\u{1}AVATAR\u{1}";
        for _ in 0..3 {
            observe(&mut peers, query, now).unwrap();
        }
        assert_eq!(peers.queue.len(), 1);
        assert_eq!(
            wire(&peers.tick(now, ME, &rosters).send),
            ["PRIVMSG kv \u{1}AVATAR\u{1}"]
        );
        // Asking again after it was sent, or answered, queues nothing.
        observe(&mut peers, query, now).unwrap();
        assert!(peers.queue.is_empty());
        observe(
            &mut peers,
            ":kv!u@h NOTICE alice :\u{1}AVATAR https://example.com/kv.png\u{1}",
            now,
        )
        .unwrap();
        observe(&mut peers, query, now).unwrap();
        assert!(peers.queue.is_empty());
        // No shared channel, or a metadata avatar: not asked.
        observe(
            &mut peers,
            ":mallory!u@h PRIVMSG alice :\u{1}AVATAR\u{1}",
            now,
        )
        .unwrap();
        assert!(peers.queue.is_empty());
        peers.observe_metadata(&[Event::UserAvatar {
            nickname: "bob".into(),
            url: Some("https://example.com/bob.png".into()),
        }]);
        observe(&mut peers, ":bob!u@h PRIVMSG alice :\u{1}AVATAR\u{1}", now).unwrap();
        assert!(peers.queue.is_empty());
        // Channel queries and replayed ones are not asked back either.
        observe(&mut peers, ":carol!u@h PRIVMSG #a :\u{1}AVATAR\u{1}", now).unwrap();
        assert!(peers.queue.is_empty());
        // The usual bounds apply.
        let members: Vec<String> = (0..100).map(|index| format!("u{index}")).collect();
        let big =
            PresenceIndex::from_rosters(&HashMap::from([("#big".to_owned(), members.clone())]));
        let mut peers = PeerAvatars::new(true, None);
        for nick in &members {
            let text = format!(":{nick}!u@h PRIVMSG alice :\u{1}AVATAR\u{1}");
            peers.observe(&line(&text), ME, &big, false, now).unwrap();
        }
        assert_eq!(peers.queue.len(), MAX_QUEUED_PROBES);
        let mut sent = 0;
        let mut at = now;
        for _ in 0..10 {
            sent += peers.tick(at, ME, &big).send.len();
            at += PROBE_INTERVAL;
        }
        assert_eq!(sent, MAX_PROBES_IN_FLIGHT);
    }

    #[test]
    fn two_clients_asking_each_other_stop_after_one_round() {
        // An ISO-2022-JP client (ja) and a UTF-8 one (en), sharing a URL.
        let rosters = PresenceIndex::from_rosters(&HashMap::from([(
            "#a".to_owned(),
            vec!["ja".to_owned(), "en".to_owned()],
        )]));
        let now = Instant::now();
        let mut ja = PeerAvatars::new(false, Some("https://example.com/ja.png".into()));
        let mut en = PeerAvatars::new(true, Some("https://example.com/en.png".into()));
        // The wire line `command` (sent to `to_nick`) as `from` sends it.
        let deliver = |from: &str,
                       to: &mut PeerAvatars,
                       to_nick: &str,
                       command: &IrcCommand,
                       now: Instant|
         -> Handled {
            let wire = wire(std::slice::from_ref(command))[0].clone();
            let (verb, rest) = wire.split_once(' ').unwrap();
            let (_, text) = rest.split_once(' ').unwrap();
            let message = line(&format!(":{from}!u@h {verb} {to_nick} {text}"));
            to.observe(&message, to_nick, &rosters, false, now).unwrap()
        };
        // en looked ja up and queries it; ja answers and asks back.
        let query = IrcCommand::PRIVMSG("ja".into(), "\u{1}AVATAR\u{1}".into());
        let handled = deliver("en", &mut ja, "ja", &query, now);
        let answer = handled.send;
        assert_eq!(answer.len(), 1);
        let handled = deliver("ja", &mut en, "en", &answer[0], now);
        assert_eq!(avatars(&handled.events), ["ja=https://example.com/ja.png"]);
        // ja queries en once; en answers and does not ask ja again.
        let later = now + PROBE_INTERVAL;
        let sent = ja.tick(later, "ja", &rosters).send;
        assert_eq!(wire(&sent), ["PRIVMSG en \u{1}AVATAR\u{1}"]);
        let handled = deliver("ja", &mut en, "en", &sent[0], later);
        assert_eq!(handled.send.len(), 1);
        assert!(en.queue.is_empty(), "en already queried ja");
        let handled = deliver("en", &mut ja, "ja", &handled.send[0], later);
        assert_eq!(avatars(&handled.events), ["en=https://example.com/en.png"]);
        for step in 1..5 {
            let at = later + PROBE_INTERVAL * step;
            assert!(ja.tick(at, "ja", &rosters).send.is_empty());
            assert!(en.tick(at, "en", &rosters).send.is_empty());
        }
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
    fn a_manual_request_queries_again_even_after_the_user_was_asked() {
        let mut peers = PeerAvatars::new(true, None);
        let presence = rosters();
        let start = Instant::now();
        // Already queried (and answered without an avatar).
        let note = peers.request("bob", ME, &presence);
        assert_eq!(note, None);
        let sent = peers.tick(start, ME, &presence).send;
        assert_eq!(wire(&sent), ["PRIVMSG bob \u{1}AVATAR\u{1}"]);
        observe(
            &mut peers,
            ":bob!u@h NOTICE alice :\u{1}AVATAR \u{1}",
            start,
        );
        // Asking again goes out once more, after the probe spacing.
        assert_eq!(peers.request("bob", ME, &presence), None);
        let later = start + PROBE_INTERVAL;
        let sent = peers.tick(later, ME, &presence).send;
        assert_eq!(wire(&sent), ["PRIVMSG bob \u{1}AVATAR\u{1}"]);
        // Nobody is asked who shares no channel with us, or ourselves.
        assert!(peers.request("stranger", ME, &presence).is_some());
        assert_eq!(peers.request(ME, ME, &presence), None);
        assert!(peers.queue.is_empty());
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
        // An empty answer removes it too. It is reported again even though
        // nothing is held here: the application keeps names beyond a
        // user's presence and must hear that the picture is gone.
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        let handled = observe(&mut peers, ":kv!u@h NOTICE alice :\u{1}AVATAR \u{1}", now).unwrap();
        assert_eq!(avatars(&handled.events), ["kv=-"]);
        let handled = observe(&mut peers, ":kv!u@h NOTICE alice :\u{1}AVATAR\u{1}", now).unwrap();
        assert_eq!(avatars(&handled.events), ["kv=-"]);
        assert!(peers.avatars.is_empty());
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
        let rosters =
            PresenceIndex::from_rosters(&HashMap::from([("#big".to_owned(), members.clone())]));
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
    fn what_we_know_follows_one_user_and_the_picture_goes_with_the_name() {
        let rosters = rosters();
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        // NICK moves it, reported for the new name; the old name keeps its
        // picture in the application and nothing says otherwise.
        let events = peers.lifecycle(&line(":kv!u@h NICK kv2"), &rosters, ME);
        assert_eq!(avatars(&events), ["kv2=https://example.com/kv.png"]);
        assert!(peers.avatars.contains_key("kv2") && !peers.avatars.contains_key("kv"));
        let moved = PresenceIndex::from_rosters(&HashMap::from([(
            "#a".to_owned(),
            vec!["alice".into(), "kv2".into(), "bob".into()],
        )]));
        // QUIT ends what we track, silently: the picture stays with the
        // name (D058), and the next holder is looked up afresh.
        let events = peers.lifecycle(&line(":kv2!u@h QUIT :bye"), &moved, ME);
        assert!(events.is_empty());
        assert!(peers.avatars.is_empty());
        // PART from the last shared channel ends it; from one of two not.
        let mut peers = PeerAvatars::new(true, None);
        observe(
            &mut peers,
            ":carol!u@h NOTICE #b :\u{1}AVATAR https://example.com/c.png\u{1}",
            Instant::now(),
        );
        peers.lifecycle(&line(":carol!u@h PART #a"), &rosters, ME);
        assert!(peers.avatars.contains_key("carol"));
        let only_b = PresenceIndex::from_rosters(&HashMap::from([(
            "#b".to_owned(),
            vec!["alice".to_owned(), "carol".to_owned()],
        )]));
        peers.lifecycle(&line(":carol!u@h PART #b"), &only_b, ME);
        assert!(!peers.avatars.contains_key("carol"));
        // Our own PART ends everyone we shared only through that channel.
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        peers.lifecycle(&line(":alice!u@h PART #a"), &rosters, ME);
        assert!(peers.avatars.is_empty());
        // Renaming onto a name with a picture replaces it with the mover's.
        let mut peers = with_kv_avatar("https://example.com/kv.png");
        observe(
            &mut peers,
            ":bob!u@h NOTICE #a :\u{1}AVATAR https://example.com/bob.png\u{1}",
            Instant::now(),
        );
        let events = peers.lifecycle(&line(":bob!u@h NICK kv"), &rosters, ME);
        assert_eq!(avatars(&events), ["kv=https://example.com/bob.png"]);
        assert_eq!(peers.avatars.len(), 1);
    }

    #[test]
    fn channel_names_compare_alike_when_leaving() {
        let rosters = PresenceIndex::from_rosters(&HashMap::from([
            ("#Room".to_owned(), vec!["alice".into(), "bob".into()]),
            ("#Other".to_owned(), vec!["alice".into(), "carol".into()]),
        ]));
        let known = |nick: &str| {
            let mut peers = PeerAvatars::new(true, None);
            observe(
                &mut peers,
                &format!(":{nick}!u@h NOTICE #a :\u{1}AVATAR https://example.com/x.png\u{1}"),
                Instant::now(),
            );
            peers
        };
        let mut peers = known("bob");
        peers.lifecycle(&line(":bob!u@h PART #room"), &rosters, ME);
        assert!(peers.avatars.is_empty());
        let mut peers = known("carol");
        peers.lifecycle(&line(":alice!u@h PART #OTHER"), &rosters, ME);
        assert!(peers.avatars.is_empty());
        let mut peers = known("carol");
        peers.lifecycle(&line(":carol!u@h PART #room"), &rosters, ME);
        assert!(
            peers.avatars.contains_key("carol"),
            "carol still shares #Other"
        );
    }

    /// Looks up `kv` with a 352 carrying `trailing`, after an optional 042,
    /// and returns what the next tick sends.
    fn who_reply_sends(yourid: bool, trailing: &str) -> Vec<String> {
        let now = Instant::now();
        let rosters = rosters();
        let mut peers = PeerAvatars::new(true, None);
        if yourid {
            let id = ":srv 042 alice 392LAGUR7 :your unique ID";
            assert!(observe(&mut peers, id, now).is_none(), "042 stays a line");
        }
        peers.speaker("kv", ME, &rosters);
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO kv"]);
        let reply = format!(":srv 352 alice * ~kv host srv kv H :{trailing}");
        assert!(observe(&mut peers, &reply, now).is_some());
        assert!(observe(&mut peers, ":srv 315 alice kv :End of WHO", now).is_some());
        wire(&peers.tick(now + PROBE_INTERVAL, ME, &rosters).send)
    }

    #[test]
    fn ircnet_server_id_is_skipped_after_042() {
        let query = ["PRIVMSG kv \u{1}AVATAR\u{1}"];
        assert_eq!(who_reply_sends(true, "0 392L \\x034\\x03Dalk_cayen"), query);
        assert_eq!(who_reply_sends(true, "0 392L \u{3}4\u{f}KVIrc user"), query);
        assert!(who_reply_sends(true, "0 392L Plain name").is_empty());
        // Not a server ID (shape), or nothing after it: read as before.
        assert!(who_reply_sends(true, "0 392l \\x034\\x03Name").is_empty());
        assert!(who_reply_sends(true, "0 392 \\x034\\x03Name").is_empty());
        assert_eq!(who_reply_sends(true, "0 \\x034\\x03Name"), query);
    }

    #[test]
    fn server_id_is_skipped_without_042() {
        let query = ["PRIVMSG kv \u{1}AVATAR\u{1}"];
        assert_eq!(who_reply_sends(false, "0 1ABC \\x034\\x03Name"), query);
        assert_eq!(
            who_reply_sends(false, "0 392L \u{2}\u{16}\u{16}\u{2}Name"),
            query
        );
        assert_eq!(who_reply_sends(false, "0 392L \u{3}4\u{f}Name"), query);
        assert_eq!(who_reply_sends(false, "0 \\x034\\x03Name"), query);
        assert_eq!(who_reply_sends(false, "0 \u{3}4\u{f}Name"), query);
        // The first word is not an ID: not a mark.
        assert!(who_reply_sends(false, "0 hello \u{2}\u{16}\u{16}\u{2}").is_empty());
        assert!(who_reply_sends(false, "0 392L Plain name").is_empty());
    }

    #[test]
    fn network_suffix_in_315_and_401_is_ignored() {
        let now = Instant::now();
        let rosters = rosters();
        let mut peers = PeerAvatars::new(true, None);
        peers.speaker("kv", ME, &rosters);
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO kv"]);
        assert!(
            observe(
                &mut peers,
                ":srv 315 alice kv@ircnet :End of WHO list.",
                now
            )
            .is_some()
        );
        assert!(peers.in_flight.is_empty());
        peers.speaker("bob", ME, &rosters);
        assert_eq!(
            wire(&peers.tick(now + PROBE_INTERVAL, ME, &rosters).send),
            ["WHO bob"]
        );
        assert!(observe(&mut peers, ":srv 401 alice bob@ircnet :No such nick", now).is_some());
        assert!(peers.in_flight.is_empty());
    }

    #[test]
    fn tiarra_multi_server_who_is_followed_by_a_query() {
        let now = Instant::now();
        let rosters = rosters();
        let mut peers = PeerAvatars::new(true, None);
        peers.speaker("kv", ME, &rosters);
        assert_eq!(wire(&peers.tick(now, ME, &rosters).send), ["WHO kv"]);
        let reply = ":dh.ircnet.ne.jp 352 alice *@ircnet ~koshian bouncer.example.net \
                     dh.ircnet.ne.jp kv H :0 392L \u{2}\u{16}\u{16}\u{2}Example Name";
        assert!(observe(&mut peers, reply, now).is_some());
        let end = ":dh.ircnet.ne.jp 315 alice kv@ircnet :End of WHO list.";
        assert!(observe(&mut peers, end, now).is_some());
        assert_eq!(
            wire(&peers.tick(now + PROBE_INTERVAL, ME, &rosters).send),
            ["PRIVMSG kv \u{1}AVATAR\u{1}"]
        );
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
    fn metadata_stops_queries_and_peer_pictures_are_reported_apart() {
        let rosters = rosters();
        let meta = |nick: &str, url: Option<&str>| Event::UserAvatar {
            nickname: nick.into(),
            url: url.map(str::to_owned),
        };
        let both = |peers: &PeerAvatars, nick: &str| {
            peers
                .avatars
                .get(nick)
                .map(|sources| (sources.metadata.clone(), sources.peer.clone()))
        };
        let mut peers = PeerAvatars::new(true, None);
        // Metadata events are only noted; the application merges them.
        peers.observe_metadata(&[meta("kv", Some("https://meta.example/kv"))]);
        // A marked user with a metadata avatar is not queried.
        let whois = ":srv 311 alice kv ~kv host * :\u{3}4\u{f}KV";
        observe(&mut peers, whois, Instant::now());
        assert!(peers.queue.is_empty());
        // A peer answer is reported as its own kind, whatever metadata says.
        let handled = observe(
            &mut peers,
            ":kv!u@h NOTICE alice :\u{1}AVATAR https://peer.example/kv.png\u{1}",
            Instant::now(),
        )
        .unwrap();
        assert_eq!(avatars(&handled.events), ["kv=https://peer.example/kv.png"]);
        assert!(matches!(handled.events[0], Event::PeerAvatar { .. }));
        peers.observe_metadata(&[meta("kv", None)]);
        assert_eq!(
            both(&peers, "kv"),
            Some((None, Some("https://peer.example/kv.png".into())))
        );
        peers.observe_metadata(&[meta("kv", Some("https://meta.example/kv2"))]);
        // NICK moves both; only the peer picture is reported (metadata
        // reports its own move).
        let events = peers.lifecycle(&line(":kv!u@h NICK kv2"), &rosters, ME);
        assert_eq!(avatars(&events), ["kv2=https://peer.example/kv.png"]);
        assert_eq!(
            both(&peers, "kv2"),
            Some((
                Some("https://meta.example/kv2".into()),
                Some("https://peer.example/kv.png".into())
            ))
        );
        // Losing metadata forgets its avatars and keeps ours.
        peers.observe_metadata(&[Event::AvatarsReset]);
        assert_eq!(
            both(&peers, "kv2"),
            Some((None, Some("https://peer.example/kv.png".into())))
        );
        // A departure ends what we track and says nothing: the picture
        // stays with the name in the application.
        let events = peers.lifecycle(&line(":kv2!u@h QUIT :bye"), &rosters, ME);
        assert!(events.is_empty());
        assert!(peers.avatars.is_empty());
    }

    const URL: &str = "https://example.com/me.png";
    const URL2: &str = "https://example.com/new.png";

    fn ask(peers: &mut PeerAvatars, nick: &str, now: Instant) {
        let text = format!(":{nick}!u@h PRIVMSG alice :\u{1}AVATAR\u{1}");
        observe(peers, &text, now).unwrap();
        // These tests follow the notices; the query in return has its own.
        peers.queue.clear();
    }

    fn tick(peers: &mut PeerAvatars, now: Instant) -> Vec<String> {
        wire(&peers.tick(now, ME, &rosters()).send)
    }

    #[test]
    fn only_users_who_asked_are_told_of_a_change_one_at_a_time() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        ask(&mut peers, "kv", now);
        ask(&mut peers, "bob", now + Duration::from_secs(1));
        // carol never asked; asking from outside our channels is not kept;
        // channel queries and replayed ones are not asks.
        ask(&mut peers, "stranger", now);
        observe(&mut peers, ":carol!u@h PRIVMSG #a :\u{1}AVATAR\u{1}", now);
        let replayed = line(":carol!u@h PRIVMSG alice :\u{1}AVATAR\u{1}");
        peers.observe(&replayed, ME, &rosters(), true, now);
        // They already have the URL they were answered with.
        assert_eq!(peers.next_deadline(), None);
        assert!(tick(&mut peers, now).is_empty());

        peers.set_share(Some(URL2.into()));
        let mut told = Vec::new();
        let mut at = now;
        for _ in 0..2 {
            assert!(peers.next_deadline().is_some());
            let sent = tick(&mut peers, at);
            assert_eq!(sent.len(), 1);
            told.extend(sent);
            // The next one waits for the interval.
            assert!(tick(&mut peers, at).is_empty());
            at += NOTIFY_INTERVAL;
        }
        told.sort();
        assert_eq!(
            told,
            [
                format!("NOTICE bob :\u{1}AVATAR {URL2}\u{1}"),
                format!("NOTICE kv :\u{1}AVATAR {URL2}\u{1}"),
            ]
        );
        assert_eq!(peers.next_deadline(), None);
        assert!(tick(&mut peers, at + NOTIFY_INTERVAL).is_empty());
    }

    #[test]
    fn stopping_sends_an_empty_notice_and_quick_changes_fold() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        ask(&mut peers, "kv", now);
        peers.set_share(None);
        assert_eq!(tick(&mut peers, now), ["NOTICE kv \u{1}AVATAR\u{1}"]);
        // Starting again is told as the new URL.
        peers.set_share(Some(URL2.into()));
        let later = now + NOTIFY_INTERVAL;
        assert_eq!(
            tick(&mut peers, later),
            [format!("NOTICE kv :\u{1}AVATAR {URL2}\u{1}")]
        );
        // Several changes before the next notice: only the last is sent,
        // and going back to what they know sends nothing.
        let later = later + NOTIFY_INTERVAL;
        peers.set_share(None);
        peers.set_share(Some(URL.into()));
        peers.set_share(Some(URL2.into()));
        assert_eq!(peers.next_deadline(), None);
        assert!(tick(&mut peers, later).is_empty());
        peers.set_share(Some(URL.into()));
        peers.set_share(None);
        assert_eq!(tick(&mut peers, later), ["NOTICE kv \u{1}AVATAR\u{1}"]);
        // Someone who asked while nothing was shared knows nothing: no
        // empty notice, but the first URL is sent.
        let mut peers = PeerAvatars::new(true, None);
        ask(&mut peers, "kv", now);
        peers.set_share(None);
        assert_eq!(peers.next_deadline(), None);
        peers.set_share(Some(URL.into()));
        assert_eq!(
            tick(&mut peers, now),
            [format!("NOTICE kv :\u{1}AVATAR {URL}\u{1}")]
        );
        // A URL that cannot be shared stops sharing.
        peers.set_share(Some("file:///etc/passwd".into()));
        assert_eq!(
            tick(&mut peers, now + NOTIFY_INTERVAL),
            ["NOTICE kv \u{1}AVATAR\u{1}"]
        );
    }

    #[test]
    fn askers_who_left_are_forgotten_and_renamed_ones_followed() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        for nick in ["kv", "bob", "carol"] {
            ask(&mut peers, nick, now);
        }
        let all = rosters();
        peers.lifecycle(&line(":kv!u@h QUIT :bye"), &all, ME);
        // bob leaves #a, the only channel they share with us.
        peers.lifecycle(&line(":bob!u@h PART #a"), &all, ME);
        // carol is still in #b after leaving #a.
        peers.lifecycle(&line(":carol!u@h PART #a"), &all, ME);
        assert_eq!(peers.askers.len(), 1);
        peers.lifecycle(&line(":carol!u@h NICK carol2"), &all, ME);
        let renamed = PresenceIndex::from_rosters(&HashMap::from([(
            "#b".to_owned(),
            vec!["alice".into(), "carol2".into()],
        )]));
        peers.set_share(None);
        assert_eq!(
            wire(&peers.tick(now, ME, &renamed).send),
            ["NOTICE carol2 \u{1}AVATAR\u{1}"]
        );
        // Someone who stopped sharing a channel with us without an event
        // we saw is dropped, not told.
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        ask(&mut peers, "kv", now);
        peers.set_share(None);
        assert!(peers.tick(now, ME, &renamed).send.is_empty());
        assert!(peers.askers.is_empty());
        assert_eq!(peers.next_deadline(), None);
    }

    #[test]
    fn an_unanswered_query_does_not_hide_a_pending_clear() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        ask(&mut peers, "kv", now);
        peers.set_share(Some(URL2.into()));
        assert_eq!(tick(&mut peers, now).len(), 1);
        peers.set_share(None);
        // Asked again before the clear went out: nothing is answered, and
        // the clear is still sent.
        ask(&mut peers, "kv", now);
        assert_eq!(
            tick(&mut peers, now + NOTIFY_INTERVAL),
            ["NOTICE kv \u{1}AVATAR\u{1}"]
        );
    }

    #[test]
    fn an_invalid_new_nickname_is_never_notified() {
        let now = Instant::now();
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        ask(&mut peers, "kv", now);
        peers.lifecycle(&line(":kv!u@h NICK :#a"), &rosters(), ME);
        peers.set_share(None);
        let channel = PresenceIndex::from_rosters(&HashMap::from([(
            "#a".to_owned(),
            vec!["alice".into(), "#a".into()],
        )]));
        assert!(peers.tick(now, ME, &channel).send.is_empty());
        assert!(peers.askers.is_empty());
    }

    #[test]
    fn askers_are_bounded() {
        let now = Instant::now();
        let names: Vec<String> = (0..MAX_NOTIFIED_USERS + 10)
            .map(|index| format!("user{index}"))
            .collect();
        let presence = PresenceIndex::from_rosters(&HashMap::from([(
            "#a".to_owned(),
            std::iter::once("alice".to_owned())
                .chain(names.clone())
                .collect(),
        )]));
        let mut peers = PeerAvatars::new(true, Some(URL.into()));
        for name in &names {
            let text = format!(":{name}!u@h PRIVMSG alice :\u{1}AVATAR\u{1}");
            peers.observe(&line(&text), ME, &presence, false, now);
        }
        assert_eq!(peers.askers.len(), MAX_NOTIFIED_USERS);
        // Invalid nicknames are never kept.
        peers.askers.clear();
        peers.observe(
            &line(":#chan!u@h PRIVMSG alice :\u{1}AVATAR\u{1}"),
            ME,
            &presence,
            false,
            now,
        );
        assert!(peers.askers.is_empty());
    }

    #[test]
    fn a_notice_from_a_user_replaces_the_avatar_they_were_asked_for() {
        let now = Instant::now();
        let mut peers = with_kv_avatar("https://peer.example/old.png");
        // kv is already queried (stage Queried); a later private notice
        // still replaces it, and an empty one removes it.
        let text = ":kv!u@h NOTICE alice :\u{1}AVATAR https://peer.example/new.png\u{1}";
        let handled = observe(&mut peers, text, now).unwrap();
        assert_eq!(
            avatars(&handled.events),
            ["kv=https://peer.example/new.png"]
        );
        let handled = observe(&mut peers, ":kv!u@h NOTICE alice :\u{1}AVATAR\u{1}", now).unwrap();
        assert_eq!(avatars(&handled.events), ["kv=-"]);
    }

    const BOB_PNG: &str = "https://example.com/bob.png";

    /// bob answered from `bob!~bob@host1.example.org`, in a connection that
    /// shares `cache`.
    fn bob_answered(cache: &PeerAvatarCache) -> PeerAvatars {
        let mut peers = PeerAvatars::with_cache(true, None, cache.clone());
        let notice =
            format!(":bob!~bob@host1.example.org NOTICE alice :\u{1}AVATAR {BOB_PNG}\u{1}");
        let handled = observe(&mut peers, &notice, Instant::now()).unwrap();
        assert_eq!(avatars(&handled.events), [format!("bob={BOB_PNG}")]);
        assert_eq!(cache.len(), 1, "remembered under bob's mask");
        peers
    }

    /// The probes waiting to be sent, as (kind, nickname).
    fn queued(peers: &PeerAvatars) -> Vec<(ProbeKind, &str)> {
        peers
            .queue
            .iter()
            .map(|probe| (probe.kind, probe.nickname.as_str()))
            .collect()
    }

    /// #612: after a reconnect the remembered picture used to stand in for
    /// the query, so bob never counted us as an asker and never told us of
    /// his change. The query goes out, and his notice replaces the picture.
    #[test]
    fn a_remembered_picture_still_asks_so_a_later_change_is_told() {
        let rosters = rosters();
        let cache = PeerAvatarCache::default();
        drop(bob_answered(&cache));
        let mut again = PeerAvatars::with_cache(true, None, cache.clone());
        let now = Instant::now();
        let joined = line(":bob!~bob@host1.example.org JOIN #a");
        assert!(again.joined(&joined, ME).is_some());
        assert_eq!(
            wire(&again.tick(now, ME, &rosters).send),
            ["PRIVMSG bob \u{1}AVATAR\u{1}"]
        );
        let changed = "https://example.com/bob-new.png";
        let notice =
            format!(":bob!~bob@host1.example.org NOTICE alice :\u{1}AVATAR {changed}\u{1}");
        let handled = observe(&mut again, &notice, now).unwrap();
        assert_eq!(avatars(&handled.events), [format!("bob={changed}")]);
        // Asked once per connection: nothing more goes out.
        assert!(
            again
                .tick(now + PROBE_INTERVAL, ME, &rosters)
                .send
                .is_empty()
        );
    }

    #[test]
    fn the_same_mask_gets_its_picture_back_at_once_and_is_asked_once() {
        let rosters = rosters();
        let cache = PeerAvatarCache::default();
        let mut peers = bob_answered(&cache);
        // bob quits; what we track is gone, the mask is remembered.
        peers.lifecycle(&line(":bob!~bob@host1.example.org QUIT :bye"), &rosters, ME);
        assert!(peers.avatars.is_empty());
        // Back with another host label and a different ident mark: the same
        // mask, so the join brings the picture, and one query (no WHO)
        // makes bob tell us of later changes.
        let joined = line(":Bob!bob@host7.EXAMPLE.org JOIN #a");
        let event = peers.joined(&joined, ME).unwrap();
        assert_eq!(avatars(&[event]), [format!("Bob={BOB_PNG}")]);
        assert_eq!(
            peers.speaker("bob", ME, &rosters),
            None,
            "nothing to look up"
        );
        assert_eq!(queued(&peers), [(ProbeKind::Query, "Bob")]);
        // A reconnect: a new worker state with the shared cache is the same.
        let mut again = PeerAvatars::with_cache(true, None, cache.clone());
        let speaking = line(":bob!bob@host2.example.org PRIVMSG #a :hi");
        let identity = speaker_identity(&speaking).unwrap();
        let event = again
            .remembered("bob", &identity.0, &identity.1, ME, Some(&rosters))
            .unwrap();
        assert_eq!(avatars(&[event]), [format!("bob={BOB_PNG}")]);
        assert_eq!(again.speaker("bob", ME, &rosters), None);
        assert_eq!(queued(&again), [(ProbeKind::Query, "bob")]);
        // Nothing is repeated for someone already looked at.
        assert!(
            again
                .remembered("bob", "bob", "x.example.org", ME, Some(&rosters))
                .is_none()
        );
    }

    #[test]
    fn a_who_reply_for_a_remembered_mask_restores_the_picture_and_asks_once() {
        let rosters = rosters();
        let cache = PeerAvatarCache::default();
        drop(bob_answered(&cache));
        // A reconnect: bob is already in the channel and only the channel's
        // WHO tells us his mask; the picture comes back and he is asked once.
        let mut again = PeerAvatars::with_cache(true, None, cache.clone());
        let who =
            line(":srv 352 alice #a ~bob host9.example.org srv bob H :0 \u{3}4\u{f}KVIrc user");
        let event = again.remembered_who(&who, ME, &rosters).unwrap();
        assert_eq!(avatars(&[event]), [format!("bob={BOB_PNG}")]);
        assert!(
            again
                .observe(&who, ME, &rosters, false, Instant::now())
                .is_none()
        );
        assert_eq!(queued(&again), [(ProbeKind::Query, "bob")]);
        // A reply for someone who left is ignored, and so is a mask nobody
        // remembers.
        let gone = line(":srv 352 alice #a ~bob host9.example.org srv ghost H :0 x");
        assert!(again.remembered_who(&gone, ME, &rosters).is_none());
        let other = line(":srv 352 alice #a ~bob other.net srv carol H :0 x");
        assert!(again.remembered_who(&other, ME, &rosters).is_none());
    }

    #[test]
    fn another_nick_or_user_is_asked_afresh() {
        let rosters = PresenceIndex::from_rosters(&HashMap::from([(
            "#a".to_owned(),
            vec!["alice".into(), "bob".into(), "bob_".into(), "carol".into()],
        )]));
        let cache = PeerAvatarCache::default();
        bob_answered(&cache);
        // KVIrc's rule: the nick is part of the key, so bob_ is asked.
        let mut peers = PeerAvatars::with_cache(true, None, cache.clone());
        let join = line(":bob_!~bob@host1.example.org JOIN #a");
        assert!(peers.joined(&join, ME).is_none());
        peers.speaker("bob_", ME, &rosters);
        assert_eq!(
            wire(&peers.tick(Instant::now(), ME, &rosters).send),
            ["WHO bob_"]
        );
        // Another user or another network part is another person.
        for join in [
            ":bob!carol@host1.example.org JOIN #a",
            ":bob!bob@host1.example.net JOIN #a",
            ":bob!bob@203.0.113.9 JOIN #a",
        ] {
            let mut peers = PeerAvatars::with_cache(true, None, cache.clone());
            assert!(peers.joined(&line(join), ME).is_none(), "{join}");
        }
        // Ourselves and other commands never match.
        let mut peers = PeerAvatars::with_cache(true, None, cache);
        assert!(
            peers
                .joined(&line(":alice!~bob@host1.example.org JOIN #a"), ME)
                .is_none()
        );
        assert!(
            peers
                .joined(&line(":bob!~bob@host1.example.org PART #a"), ME)
                .is_none()
        );
    }

    #[test]
    fn an_answer_without_a_picture_or_an_unmarked_realname_forgets_the_mask() {
        let cache = PeerAvatarCache::default();
        let mut peers = bob_answered(&cache);
        let handled = observe(
            &mut peers,
            ":bob!~bob@host1.example.org NOTICE alice :\u{1}AVATAR\u{1}",
            Instant::now(),
        )
        .unwrap();
        assert_eq!(avatars(&handled.events), ["bob=-"]);
        assert!(cache.is_empty());
        // A lookup of ours that finds no mark also drops the old picture.
        let mut peers = bob_answered(&cache);
        peers.avatars.clear();
        peers.stages.clear();
        peers.in_flight.push(Probe {
            kind: ProbeKind::Who,
            nickname: "bob".into(),
            key: "bob".into(),
            deadline: Instant::now() + PROBE_TIMEOUT,
            stale: false,
        });
        let handled = observe(
            &mut peers,
            ":srv 352 alice #a ~bob host1.example.org srv bob H :0 Bob",
            Instant::now(),
        )
        .unwrap();
        assert_eq!(avatars(&handled.events), ["bob=-"]);
        assert!(cache.is_empty());
    }
}
