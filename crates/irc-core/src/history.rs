//! Channel history requested with IRCv3 `draft/chathistory` (opt-in per
//! server, experimental).
//!
//! Two kinds of request share one bounded queue and one outstanding slot:
//!
//! - when we join a channel, `CHATHISTORY LATEST <channel> * <limit>` asks
//!   for its latest lines (recent history);
//! - when we join a channel again after a reconnect and the application
//!   knows the newest message it received before the link dropped
//!   ([`HistoryResume`]), `CHATHISTORY LATEST <channel> <reference>
//!   <limit>` asks only for what came after it (reconnect gap recovery);
//! - when the user scrolls to the top of a channel's log, the application
//!   asks for one older page, `CHATHISTORY BEFORE <channel> <reference>
//!   <limit>`, where the reference is the oldest message it holds (see
//!   [`MessageReference`]).
//!
//! - once per connection, `CHATHISTORY TARGETS <from> <to> <limit>` asks which
//!   conversations had messages in the time since the previous connection
//!   ended (direct messages the server kept while we were away). Each
//!   direct-message target it names gets the same `LATEST` request as a
//!   joined channel, with the peer's nickname as target. Channel targets are
//!   ignored: joined channels ask for their own history on join.
//!
//! Requests go out one at a time, so joining many channels does not burst
//! commands. The reply is a `chathistory` batch whose single parameter is
//! the channel; every line in it (and in batches nested inside it) is
//! consumed here, never treated as live traffic, and its PRIVMSG/NOTICE
//! lines are buffered (bounded) until the batch ends and then reported
//! together. A batch that never ends reports nothing, `FAIL CHATHISTORY` or
//! a timeout ends the request, and everything lives in the connection's
//! worker, so a reconnect starts empty and cannot receive an old
//! connection's answer. Every older-page request ends in exactly one
//! [`Finished`] (lines, the beginning of history, or a failure), so the
//! application never waits for an answer that cannot come.
//!
//! Only negotiated `batch` makes a reply recognizable, so the capability is
//! requested only after `batch` is acknowledged (see `cap`). The
//! specification also allows replies without batches; this client does not
//! use that form.

use std::{collections::VecDeque, time::Duration, time::SystemTime};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix, Response};
use tokio::time::Instant;

use crate::tags;

/// Lines asked for per request. The server's `CHATHISTORY` limit lowers it;
/// this client never asks for more.
pub const HISTORY_LIMIT: usize = 50;
/// Lines kept from one reply. Servers MAY return more than asked for; the
/// rest is dropped.
pub const MAX_HISTORY_LINES: usize = 100;
/// Requests waiting to be sent. Joining more channels at once skips the
/// rest; an older page asked for while it is full fails at once.
const MAX_QUEUED: usize = 64;
/// Direct-message targets asked history for after one `TARGETS` reply.
const MAX_DISCOVERED: usize = 16;
/// Entries kept from one `TARGETS` reply (servers may return more than
/// asked for).
const MAX_TARGET_ENTRIES: usize = 64;
/// The batch type of a `TARGETS` reply, as irc-proto spells it.
const TARGETS_BATCH: &str = "DRAFT/CHATHISTORY-TARGETS";
/// A request without a reply is given up after this long, and the next one
/// is sent.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Abandoned requests whose late reply is still swallowed.
const MAX_ABANDONED: usize = 8;
/// Nested batches followed inside one reply.
const MAX_NESTED: usize = 16;
/// Longest batch reference followed.
const MAX_REFERENCE_BYTES: usize = 64;
/// Longest msgid used as a reference; longer ones are not kept by the
/// application either (`model::NativeMessageId`).
const MAX_MSGID_BYTES: usize = 128;
/// The reply tag saying no older (for BEFORE) messages remain.
const END_TAG: &str = "draft/chathistory-end";
/// Channels a connection may resume; more than a network can have
/// conversations.
const MAX_RESUME: usize = 1024;
/// How far a timestamp reference for gap recovery reaches back, for clock
/// skew between the servers of a network (the specification suggests 1 to
/// 10 s). The lines this repeats are already shown and dropped as
/// duplicates.
const RESUME_SKEW: Duration = Duration::from_secs(5);

/// One line of requested history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryMessage {
    pub sender: String,
    pub text: String,
    pub notice: bool,
    pub server_time: Option<SystemTime>,
    pub msgid: Option<String>,
}

/// A message the application holds, as a CHATHISTORY reference: its
/// `msgid` and its server time, whichever it has. The connection picks the
/// type the server accepts (`MSGREFTYPES`), preferring msgid because it is
/// exact; a timestamp reference skips other messages of the same
/// millisecond.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageReference {
    pub msgid: Option<String>,
    pub time: Option<SystemTime>,
}

/// Where a channel's log stopped when the previous connection ended: the
/// newest message received from the server before the link dropped. On
/// this connection our JOIN of `channel` asks only for what came after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryResume {
    pub channel: String,
    pub after: MessageReference,
}

/// How an older-page request ended ([`crate::Event::OlderChannelHistory`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OlderHistoryStatus {
    /// Lines arrived and the server did not say they were the oldest.
    More,
    /// The server has nothing older: an empty reply, or one marked
    /// `draft/chathistory-end`.
    Beginning,
    /// Nothing can be said: the request failed, timed out, could not be
    /// sent, or the capability went away.
    Failed,
}

/// What a request asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    /// The latest lines, on join.
    Latest,
    /// The latest lines after a reference, on the first join after a
    /// reconnect.
    Resume,
    /// One page before a reference, for the application's request
    /// `request`.
    Before { request: u64 },
    /// Which conversations had messages between `from` and `to`.
    Targets { from: SystemTime, to: SystemTime },
}

/// One conversation named by a `TARGETS` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryTarget {
    pub name: String,
    pub latest: SystemTime,
}

/// A request that ended.
#[derive(Debug, PartialEq)]
pub(crate) struct Finished {
    pub channel: String,
    pub page: Page,
    pub messages: Vec<HistoryMessage>,
    /// How many entries a `TARGETS` reply had that were kept.
    pub targets: usize,
    /// The reply said nothing older remains (BEFORE only).
    pub end: bool,
    /// A resumed request whose reply reached its limit without the end
    /// tag: lines between the reference and the oldest one returned may be
    /// missing.
    pub incomplete: bool,
    /// The request failed or was given up; `messages` is empty.
    pub failed: bool,
    pub note: Option<String>,
}

impl Finished {
    fn failed(channel: String, page: Page, note: Option<String>) -> Self {
        Self {
            channel,
            page,
            messages: Vec::new(),
            targets: 0,
            end: false,
            incomplete: false,
            failed: true,
            note,
        }
    }

    /// An older-page request that cannot be sent on this connection.
    pub(crate) fn failed_older(channel: String, request: u64) -> Self {
        let note = format!("Older history for {channel} is not available on this connection.");
        Self::failed(channel, Page::Before { request }, Some(note))
    }

    /// The status reported for an older page.
    pub(crate) fn older_status(&self) -> OlderHistoryStatus {
        if self.failed {
            OlderHistoryStatus::Failed
        } else if self.end || self.messages.is_empty() {
            OlderHistoryStatus::Beginning
        } else {
            OlderHistoryStatus::More
        }
    }
}

/// What [`HistoryRequests::observe`] did with an incoming line.
#[derive(Debug, PartialEq)]
pub(crate) enum Observed {
    /// Not part of a history reply; process it as usual.
    Unrelated,
    /// Part of a reply; nothing else may see it.
    Consumed,
    /// A request ended: its lines (possibly none), or a failure.
    Finished(Finished),
}

#[derive(Debug)]
struct Queued {
    channel: String,
    page: Page,
    /// The command's arguments after `CHATHISTORY <subcommand> <channel>`.
    selector: String,
    limit: usize,
}

#[derive(Debug)]
struct Outstanding {
    channel: String,
    page: Page,
    sent: Instant,
    /// The reply batch's reference once it opened.
    reference: Option<String>,
    /// The reply batch carried `draft/chathistory-end`.
    end: bool,
    /// Lines asked for.
    limit: usize,
    nested: Vec<String>,
    messages: Vec<HistoryMessage>,
    targets: Vec<HistoryTarget>,
    dropped: usize,
}

/// Which reference types the server accepts (`MSGREFTYPES`). Without the
/// token a server supports both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReferenceTypes {
    msgid: bool,
    timestamp: bool,
}

impl Default for ReferenceTypes {
    fn default() -> Self {
        Self {
            msgid: true,
            timestamp: true,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HistoryRequests {
    /// The server's `CHATHISTORY` ISUPPORT value; 0 or absent means none.
    server_limit: Option<usize>,
    reference_types: ReferenceTypes,
    queue: VecDeque<Queued>,
    outstanding: Option<Outstanding>,
    /// Channels whose first join resumes after a reference; each entry is
    /// used (or dropped) by that join.
    resume: Vec<HistoryResume>,
    /// Channels of abandoned requests: a late reply is swallowed, not shown
    /// as live or as another request's reply.
    abandoned: VecDeque<String>,
    /// A late reply being swallowed, and its nested batches.
    discarding: Vec<String>,
}

impl HistoryRequests {
    /// Requests for a connection that resumes `resume` (at most
    /// [`MAX_RESUME`] channels).
    pub(crate) fn with_resume(mut resume: Vec<HistoryResume>) -> Self {
        resume.truncate(MAX_RESUME);
        Self {
            resume,
            ..Self::default()
        }
    }

    /// Reads `CHATHISTORY=<n>` and `MSGREFTYPES=<types>` (and their
    /// removal) from RPL_ISUPPORT.
    pub(crate) fn isupport(&mut self, message: &IrcMessage) {
        let IrcCommand::Response(Response::RPL_ISUPPORT, args) = &message.command else {
            return;
        };
        // The first argument is our nickname, the last the trailing text.
        for token in args.iter().skip(1).flat_map(|arg| arg.split_whitespace()) {
            if token.eq_ignore_ascii_case("-CHATHISTORY") {
                self.server_limit = None;
            } else if token.eq_ignore_ascii_case("-MSGREFTYPES") {
                self.reference_types = ReferenceTypes::default();
            } else if let Some((name, value)) = token.split_once('=') {
                if name.eq_ignore_ascii_case("CHATHISTORY") {
                    self.server_limit = value.parse().ok();
                } else if name.eq_ignore_ascii_case("MSGREFTYPES") {
                    // Unknown types are ignored; a list naming none of ours
                    // leaves no usable reference.
                    let listed = |kind: &str| value.split(',').any(|item| item == kind);
                    self.reference_types = ReferenceTypes {
                        msgid: listed("msgid"),
                        timestamp: listed("timestamp"),
                    };
                }
            }
        }
    }

    /// Lines to ask for: our own limit, lowered by the server's.
    pub(crate) fn limit(&self) -> usize {
        match self.server_limit {
            Some(limit) if limit > 0 => limit.min(HISTORY_LIMIT),
            _ => HISTORY_LIMIT,
        }
    }

    /// The `msgid=` or `timestamp=` selector for `reference`, preferring
    /// msgid, within what the server accepts.
    fn selector(&self, reference: &MessageReference) -> Option<String> {
        let msgid = reference.msgid.as_deref().filter(|id| {
            !id.is_empty()
                && id.len() <= MAX_MSGID_BYTES
                && id.bytes().all(|b| b.is_ascii_graphic())
        });
        if self.reference_types.msgid
            && let Some(id) = msgid
        {
            return Some(format!("msgid={id}"));
        }
        if self.reference_types.timestamp
            && let Some(time) = reference.time
        {
            return Some(format!("timestamp={}", tags::format_server_time(time)));
        }
        None
    }

    /// Queues a request for `channel`'s latest lines, unless one is already
    /// queued or being answered. The first join of a channel this
    /// connection resumes asks only for the lines after its reference, when
    /// the server accepts one of its reference types; otherwise, and on
    /// later joins, for the latest lines. Returns a note when the queue is
    /// full.
    pub(crate) fn enqueue(&mut self, channel: &str) -> Option<String> {
        let latest = |name: &str, page: &Page| {
            matches!(page, Page::Latest | Page::Resume) && crate::text::same_nickname(name, channel)
        };
        if self
            .queue
            .iter()
            .any(|queued| latest(&queued.channel, &queued.page))
            || self
                .outstanding
                .as_ref()
                .is_some_and(|o| latest(&o.channel, &o.page))
        {
            return None;
        }
        if self.queue.len() >= MAX_QUEUED {
            return Some(format!(
                "History for {channel} was not requested: {MAX_QUEUED} requests are already waiting."
            ));
        }
        let resume = self
            .resume
            .iter()
            .position(|resume| crate::text::same_nickname(&resume.channel, channel))
            .map(|index| self.resume.swap_remove(index));
        let resumed = resume.and_then(|resume| {
            let mut after = resume.after;
            after.time = after.time.map(|time| {
                time.checked_sub(RESUME_SKEW)
                    .unwrap_or(std::time::UNIX_EPOCH)
            });
            self.selector(&after)
        });
        let (page, selector) = match resumed {
            Some(selector) => (Page::Resume, selector),
            None => (Page::Latest, "*".into()),
        };
        self.queue.push_back(Queued {
            channel: channel.to_owned(),
            page,
            selector,
            // Lowered to the server's limit when sent.
            limit: HISTORY_LIMIT,
        });
        None
    }

    /// Queues one `TARGETS` request for the conversations with messages
    /// since `since` (the previous connection's end, or a day back), ahead
    /// of everything else. At most once per connection: the caller decides.
    pub(crate) fn enqueue_targets(&mut self, since: SystemTime, now: SystemTime) {
        if self.queue.len() >= MAX_QUEUED {
            return;
        }
        self.queue.push_front(Queued {
            channel: "*".into(),
            page: Page::Targets {
                from: since.min(now),
                // Servers store their own clock; leave room for skew.
                to: now + Duration::from_secs(300),
            },
            selector: String::new(),
            limit: MAX_DISCOVERED,
        });
    }

    /// The direct-message peers worth asking history for: nicknames only
    /// (joined channels ask on join), each once whatever its case, the
    /// newest first, at most [`MAX_DISCOVERED`].
    pub(crate) fn direct_targets(targets: &[HistoryTarget]) -> Vec<String> {
        let mut peers: Vec<&HistoryTarget> = targets
            .iter()
            .filter(|target| {
                !crate::valid_channel(&target.name)
                    && crate::valid_nickname(&target.name)
                    && !target.name.contains(['!', '@'])
            })
            .collect();
        peers.sort_by_key(|target| std::cmp::Reverse(target.latest));
        let mut names: Vec<String> = Vec::new();
        for target in peers {
            if names.len() == MAX_DISCOVERED {
                break;
            }
            if !names
                .iter()
                .any(|name| crate::text::same_nickname(name, &target.name))
            {
                names.push(target.name.clone());
            }
        }
        names
    }

    /// A resumed request failed: the channel still gets its latest lines,
    /// as on any join, unless the queue is full.
    fn fall_back(&mut self, finished: &Finished) {
        if finished.page == Page::Resume && self.queue.len() < MAX_QUEUED {
            self.queue.push_back(Queued {
                channel: finished.channel.clone(),
                page: Page::Latest,
                selector: "*".into(),
                limit: HISTORY_LIMIT,
            });
        }
    }

    /// Queues the application's request `request` for up to `limit` lines of
    /// `channel` before `reference`. It goes ahead of queued recent-history
    /// requests, because the user is waiting for it. A request that cannot
    /// be queued ends at once.
    pub(crate) fn enqueue_older(
        &mut self,
        channel: &str,
        request: u64,
        reference: &MessageReference,
        limit: usize,
    ) -> Result<(), Finished> {
        let page = Page::Before { request };
        let fail = |note: String| Finished::failed(channel.to_owned(), page.clone(), Some(note));
        let Some(selector) = self.selector(reference) else {
            return Err(fail(format!(
                "Older history for {channel} was not requested: the server accepts no reference this client has."
            )));
        };
        let older = |name: &str, page: &Page| {
            matches!(page, Page::Before { .. }) && crate::text::same_nickname(name, channel)
        };
        if self
            .queue
            .iter()
            .any(|queued| older(&queued.channel, &queued.page))
            || self
                .outstanding
                .as_ref()
                .is_some_and(|o| older(&o.channel, &o.page))
        {
            return Err(fail(format!(
                "Older history for {channel} is already being requested."
            )));
        }
        if self.queue.len() >= MAX_QUEUED {
            return Err(fail(format!(
                "Older history for {channel} was not requested: {MAX_QUEUED} requests are already waiting."
            )));
        }
        self.queue.push_front(Queued {
            channel: channel.to_owned(),
            page,
            selector,
            limit: limit.max(1),
        });
        Ok(())
    }

    /// Drops queued requests for a channel we left. Older-page requests
    /// among them end as failures.
    pub(crate) fn forget(&mut self, channel: &str) -> Vec<Finished> {
        let mut ended = Vec::new();
        self.queue.retain(|queued| {
            if !crate::text::same_nickname(&queued.channel, channel) {
                return true;
            }
            if let Page::Before { .. } = queued.page {
                ended.push(Finished::failed(
                    queued.channel.clone(),
                    queued.page.clone(),
                    None,
                ));
            }
            false
        });
        ended
    }

    /// The next request when none is being answered.
    pub(crate) fn next_request(&mut self, now: Instant) -> Option<(String, Page, IrcCommand)> {
        if self.outstanding.is_some() {
            return None;
        }
        let queued = self.queue.pop_front()?;
        let limit = queued.limit.min(self.limit());
        let command = match &queued.page {
            Page::Targets { from, to } => IrcCommand::Raw(
                "CHATHISTORY".into(),
                vec![
                    "TARGETS".into(),
                    format!("timestamp={}", tags::format_server_time(*from)),
                    format!("timestamp={}", tags::format_server_time(*to)),
                    limit.to_string(),
                ],
            ),
            page => IrcCommand::Raw(
                "CHATHISTORY".into(),
                vec![
                    if matches!(page, Page::Before { .. }) {
                        "BEFORE"
                    } else {
                        "LATEST"
                    }
                    .into(),
                    queued.channel.clone(),
                    queued.selector,
                    limit.to_string(),
                ],
            ),
        };
        self.outstanding = Some(Outstanding {
            channel: queued.channel.clone(),
            page: queued.page.clone(),
            sent: now,
            reference: None,
            end: false,
            limit,
            nested: Vec::new(),
            messages: Vec::new(),
            targets: Vec::new(),
            dropped: 0,
        });
        Some((queued.channel, queued.page, command))
    }

    /// When the outstanding request times out; no timer runs otherwise.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.outstanding
            .as_ref()
            .map(|outstanding| outstanding.sent + RESPONSE_TIMEOUT)
    }

    /// Gives up the outstanding request if it timed out.
    pub(crate) fn tick(&mut self, now: Instant) -> Option<Observed> {
        let outstanding = self.outstanding.as_ref()?;
        if now < outstanding.sent + RESPONSE_TIMEOUT {
            return None;
        }
        let outstanding = self.outstanding.take()?;
        if self.abandoned.len() >= MAX_ABANDONED {
            self.abandoned.pop_front();
        }
        self.abandoned.push_back(outstanding.channel.clone());
        let note = format!(
            "History for {} was not answered within {} s.",
            outstanding.channel,
            RESPONSE_TIMEOUT.as_secs()
        );
        let finished = Finished::failed(outstanding.channel, outstanding.page, Some(note));
        self.fall_back(&finished);
        Some(Observed::Finished(finished))
    }

    /// The capability went away: nothing is requested any more. Returns the
    /// requests that end without lines: the one being answered and every
    /// queued older-page request.
    pub(crate) fn reset(&mut self) -> Vec<Finished> {
        let mut ended: Vec<Finished> = self
            .outstanding
            .take()
            .map(|o| Finished::failed(o.channel, o.page, None))
            .into_iter()
            .collect();
        ended.extend(self.queue.drain(..).filter_map(|queued| {
            matches!(queued.page, Page::Before { .. })
                .then(|| Finished::failed(queued.channel, queued.page, None))
        }));
        // Resume entries not used yet stay: a channel joined later on this
        // connection is still the one that was cut off.
        *self = Self {
            server_limit: self.server_limit,
            reference_types: self.reference_types,
            resume: std::mem::take(&mut self.resume),
            ..Self::default()
        };
        ended
    }

    pub(crate) fn observe(&mut self, message: &IrcMessage) -> Observed {
        let batch_tag = tags::tag_value(message, "batch");
        if let IrcCommand::BATCH(reference, kind, params) = &message.command {
            return self.observe_batch(
                reference,
                kind.as_ref().map(|k| k.to_str()),
                params,
                batch_tag,
                tags::has_tag(message, END_TAG),
            );
        }
        if let Some(reference) = batch_tag
            && self.discarding.iter().any(|open| open == reference)
        {
            return Observed::Consumed;
        }
        let Some(outstanding) = self.outstanding.as_mut() else {
            return Observed::Unrelated;
        };
        if let Some(reference) = batch_tag
            && (outstanding.reference.as_deref() == Some(reference)
                || outstanding.nested.iter().any(|open| open == reference))
        {
            if let Page::Targets { .. } = outstanding.page {
                if let Some(target) = target_line(message)
                    && outstanding.targets.len() < MAX_TARGET_ENTRIES
                {
                    outstanding.targets.push(target);
                }
                return Observed::Consumed;
            }
            // Without event-playback only PRIVMSG and NOTICE may appear;
            // anything else is dropped rather than applied as live state.
            if let Some(line) = history_line(message, &outstanding.channel) {
                if outstanding.messages.len() < MAX_HISTORY_LINES {
                    outstanding.messages.push(line);
                } else {
                    outstanding.dropped += 1;
                }
            }
            return Observed::Consumed;
        }
        if outstanding.reference.is_none()
            && let Some(note) = failure(message, &outstanding.channel)
        {
            let outstanding = self.outstanding.take().expect("outstanding request");
            let note = if matches!(outstanding.page, Page::Targets { .. }) {
                note.replacen("History for *", "History targets", 1)
            } else {
                note
            };
            let finished = Finished::failed(outstanding.channel, outstanding.page, Some(note));
            self.fall_back(&finished);
            return Observed::Finished(finished);
        }
        Observed::Unrelated
    }

    fn observe_batch(
        &mut self,
        reference: &str,
        kind: Option<&str>,
        params: &Option<Vec<String>>,
        parent: Option<&str>,
        end: bool,
    ) -> Observed {
        if let Some(reference) = reference.strip_prefix('-') {
            if let Some(index) = self.discarding.iter().position(|open| open == reference) {
                self.discarding.remove(index);
                return Observed::Consumed;
            }
            let Some(outstanding) = self.outstanding.as_mut() else {
                return Observed::Unrelated;
            };
            if let Some(index) = outstanding.nested.iter().position(|open| open == reference) {
                outstanding.nested.remove(index);
                return Observed::Consumed;
            }
            if outstanding.reference.as_deref() != Some(reference) {
                return Observed::Unrelated;
            }
            let outstanding = self.outstanding.take().expect("outstanding request");
            let note = (outstanding.dropped > 0).then(|| {
                format!(
                    "History for {} had {} more lines than kept ({MAX_HISTORY_LINES}).",
                    outstanding.channel, outstanding.dropped
                )
            });
            let mut note = note;
            if matches!(outstanding.page, Page::Targets { .. }) {
                // Each direct-message peer asks for its latest lines like a
                // joined channel; the caller sends them one at a time.
                let peers = Self::direct_targets(&outstanding.targets);
                for peer in &peers {
                    if let Some(full) = self.enqueue(peer) {
                        note = Some(full);
                        break;
                    }
                }
                note = note.or_else(|| {
                    Some(format!(
                        "History targets: {} direct-message conversation(s) asked for history.",
                        peers.len()
                    ))
                });
            }
            let incomplete = outstanding.page == Page::Resume
                && !outstanding.end
                && outstanding.messages.len() + outstanding.dropped >= outstanding.limit;
            return Observed::Finished(Finished {
                channel: outstanding.channel,
                page: outstanding.page,
                messages: outstanding.messages,
                targets: outstanding.targets.len(),
                end: outstanding.end,
                incomplete,
                failed: false,
                note,
            });
        }
        let Some(reference) = reference.strip_prefix('+') else {
            return Observed::Unrelated;
        };
        if reference.is_empty() || reference.len() > MAX_REFERENCE_BYTES {
            return Observed::Unrelated;
        }
        // A batch inside a reply belongs to it.
        if let Some(parent) = parent {
            if self.discarding.iter().any(|open| open == parent) {
                if self.discarding.len() < MAX_NESTED + 1 {
                    self.discarding.push(reference.to_owned());
                }
                return Observed::Consumed;
            }
            if let Some(outstanding) = self.outstanding.as_mut()
                && (outstanding.reference.as_deref() == Some(parent)
                    || outstanding.nested.iter().any(|open| open == parent))
            {
                if outstanding.nested.len() < MAX_NESTED {
                    outstanding.nested.push(reference.to_owned());
                }
                return Observed::Consumed;
            }
        }
        // irc-proto upper-cases batch types. A TARGETS reply's batch has no
        // target parameter.
        if kind == Some(TARGETS_BATCH) {
            if let Some(outstanding) = self.outstanding.as_mut()
                && outstanding.reference.is_none()
                && matches!(outstanding.page, Page::Targets { .. })
            {
                outstanding.reference = Some(reference.to_owned());
                return Observed::Consumed;
            }
            return Observed::Unrelated;
        }
        if kind != Some("CHATHISTORY") {
            return Observed::Unrelated;
        }
        let Some(target) = params.as_ref().and_then(|params| params.first()) else {
            return Observed::Unrelated;
        };
        if let Some(outstanding) = self.outstanding.as_mut()
            && outstanding.reference.is_none()
            && crate::text::same_nickname(&outstanding.channel, target)
        {
            outstanding.reference = Some(reference.to_owned());
            outstanding.end = end;
            return Observed::Consumed;
        }
        if let Some(index) = self
            .abandoned
            .iter()
            .position(|channel| crate::text::same_nickname(channel, target))
        {
            self.abandoned.remove(index);
            self.discarding.push(reference.to_owned());
            return Observed::Consumed;
        }
        // Someone else's history batch (bouncer playback): replay handling.
        Observed::Unrelated
    }
}

/// One `CHATHISTORY TARGETS <target> <timestamp>` line of a reply.
fn target_line(message: &IrcMessage) -> Option<HistoryTarget> {
    let IrcCommand::Raw(verb, args) = &message.command else {
        return None;
    };
    if !verb.eq_ignore_ascii_case("CHATHISTORY") {
        return None;
    }
    let [subcommand, name, latest, ..] = args.as_slice() else {
        return None;
    };
    if !subcommand.eq_ignore_ascii_case("TARGETS") || name.is_empty() || name.len() > 64 {
        return None;
    }
    Some(HistoryTarget {
        name: name.clone(),
        latest: tags::parse_server_time(latest)?,
    })
}

/// A PRIVMSG or NOTICE in a reply about `channel` (a channel, or a peer's
/// nickname for a direct-message conversation).
fn history_line(message: &IrcMessage, channel: &str) -> Option<HistoryMessage> {
    let (target, text, notice) = match &message.command {
        IrcCommand::PRIVMSG(target, text) => (target, text, false),
        IrcCommand::NOTICE(target, text) => (target, text, true),
        _ => return None,
    };
    if crate::valid_channel(channel) {
        if !crate::text::same_nickname(target, channel) {
            return None;
        }
    } else {
        // A conversation with `channel`: they wrote to us (the target is
        // us, whoever we are) or we wrote to them.
        let from_peer = matches!(&message.prefix,
            Some(Prefix::Nickname(nickname, _, _)) if crate::text::same_nickname(nickname, channel));
        if !from_peer && !crate::text::same_nickname(target, channel) {
            return None;
        }
    }
    // CTCP other than ACTION is never chat, live or from history (D029).
    if text.starts_with('\u{1}') && crate::text::action_text(text).is_none() {
        return None;
    }
    let sender = match &message.prefix {
        Some(Prefix::Nickname(nickname, _, _)) => nickname.clone(),
        Some(Prefix::ServerName(name)) => name.clone(),
        None => "server".into(),
    };
    Some(HistoryMessage {
        sender,
        text: text.clone(),
        notice,
        // The reply carries the time the server recorded; a missing or
        // invalid one shows the receipt time.
        server_time: tags::tag_value(message, "time").and_then(tags::parse_server_time),
        msgid: tags::msgid(message).map(str::to_owned),
    })
}

/// `FAIL CHATHISTORY <code> ...` or a numeric rejecting the command, while
/// waiting for the reply to `channel`.
fn failure(message: &IrcMessage, channel: &str) -> Option<String> {
    match &message.command {
        IrcCommand::Raw(verb, args)
            if verb.eq_ignore_ascii_case("FAIL")
                && args
                    .first()
                    .is_some_and(|command| command.eq_ignore_ascii_case("CHATHISTORY")) =>
        {
            let code = args.get(1).map_or("", String::as_str);
            let description = args.last().map_or("", String::as_str);
            Some(format!(
                "History for {channel} is unavailable ({code}): {description}"
            ))
        }
        IrcCommand::Response(
            response @ (Response::ERR_UNKNOWNCOMMAND | Response::ERR_NEEDMOREPARAMS),
            args,
        ) if args
            .get(1)
            .is_some_and(|command| command.eq_ignore_ascii_case("CHATHISTORY")) =>
        {
            Some(format!(
                "History for {channel} is unavailable: the server rejected CHATHISTORY ({response:?})."
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> IrcMessage {
        line.parse().unwrap()
    }

    fn feed(requests: &mut HistoryRequests, lines: &[&str]) -> Vec<Observed> {
        lines
            .iter()
            .map(|line| requests.observe(&parse(line)))
            .collect()
    }

    fn sent(requests: &mut HistoryRequests, now: Instant) -> Option<String> {
        requests
            .next_request(now)
            .map(|(_, _, command)| IrcMessage::from(command).to_string().trim_end().to_owned())
    }

    #[test]
    fn limits_follow_isupport_and_the_hard_cap() {
        let mut requests = HistoryRequests::default();
        assert_eq!(requests.limit(), HISTORY_LIMIT);
        requests.isupport(&parse(
            ":srv 005 me CHATHISTORY=20 CHANTYPES=# :are supported",
        ));
        assert_eq!(requests.limit(), 20);
        requests.isupport(&parse(":srv 005 me CHATHISTORY=1000 :are supported"));
        assert_eq!(requests.limit(), HISTORY_LIMIT, "our cap wins");
        requests.isupport(&parse(":srv 005 me CHATHISTORY=0 :are supported"));
        assert_eq!(requests.limit(), HISTORY_LIMIT, "0 means no server limit");
        requests.isupport(&parse(":srv 005 me CHATHISTORY=x :are supported"));
        assert_eq!(requests.limit(), HISTORY_LIMIT);
        requests.isupport(&parse(":srv 005 me CHATHISTORY=10 :are supported"));
        requests.isupport(&parse(":srv 005 me -CHATHISTORY :are supported"));
        assert_eq!(requests.limit(), HISTORY_LIMIT);
    }

    #[test]
    fn requests_go_out_one_at_a_time_without_duplicates() {
        let mut requests = HistoryRequests::default();
        requests.isupport(&parse(":srv 005 me CHATHISTORY=30 :are supported"));
        let now = Instant::now();
        assert!(requests.next_deadline().is_none(), "idle: no timer");
        for channel in ["#a", "#b", "#A"] {
            assert!(requests.enqueue(channel).is_none());
        }
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #a * 30")
        );
        assert!(requests.enqueue("#a").is_none(), "being answered");
        assert!(sent(&mut requests, now).is_none(), "one at a time");
        assert_eq!(requests.next_deadline(), Some(now + RESPONSE_TIMEOUT));
        let finished = feed(
            &mut requests,
            &[
                "@draft/chathistory-end :srv BATCH +r1 chathistory #a",
                "@batch=r1;time=2026-09-27T23:58:31.123Z;msgid=m1 :bob!u@h PRIVMSG #a :one",
                "@batch=r1;msgid=m2 :bob!u@h NOTICE #a :two",
                "@batch=r1 :bob!u@h PRIVMSG #a :\u{1}VERSION\u{1}",
                "@batch=r1 :bob!u@h NOTICE #a :\u{1}AVATAR bob.png\u{1}",
                ":srv BATCH -r1",
            ],
        );
        assert!(finished[..5].iter().all(|o| *o == Observed::Consumed));
        let Observed::Finished(Finished {
            channel,
            messages,
            note,
            ..
        }) = &finished[5]
        else {
            panic!("{finished:?}");
        };
        assert_eq!(channel, "#a");
        assert!(note.is_none());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].msgid.as_deref(), Some("m1"));
        assert!(messages[0].server_time.is_some());
        assert!(messages[1].notice && messages[1].server_time.is_none());
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #b * 30")
        );
        assert!(sent(&mut requests, now).is_none());
    }

    #[test]
    fn empty_replies_failures_and_unknown_commands_end_the_request() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        for channel in ["#empty", "#fail", "#old"] {
            requests.enqueue(channel);
        }
        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[":srv BATCH +e chathistory #empty", ":srv BATCH -e"],
        );
        assert!(
            matches!(&observed[1], Observed::Finished(Finished { messages, note: None, failed: false, .. }) if messages.is_empty())
        );

        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[":srv FAIL CHATHISTORY INVALID_TARGET LATEST #fail :Messages could not be retrieved"],
        );
        assert!(
            matches!(&observed[0], Observed::Finished(Finished { channel, note: Some(note), failed: true, .. })
            if channel == "#fail" && note.contains("INVALID_TARGET"))
        );

        sent(&mut requests, now);
        let observed = feed(&mut requests, &[":srv 421 me CHATHISTORY :Unknown command"]);
        assert!(
            matches!(&observed[0], Observed::Finished(Finished { channel, note: Some(_), .. }) if channel == "#old")
        );
        // Other failures are not ours.
        requests.enqueue("#x");
        sent(&mut requests, now);
        assert_eq!(
            feed(&mut requests, &[":srv FAIL METADATA KEY_INVALID x :bad"])[0],
            Observed::Unrelated
        );
    }

    #[test]
    fn live_lines_playback_and_other_batches_are_not_consumed() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue("#a");
        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[
                // Automatic playback for another channel, and a netsplit.
                ":srv BATCH +p chathistory #other",
                "@batch=p :bob!u@h PRIVMSG #other :played back",
                ":srv BATCH +n netsplit a b",
                // Live traffic while waiting.
                ":bob!u@h PRIVMSG #a :live",
                ":srv BATCH +r chathistory #a",
                // Live line between reply lines stays live.
                ":carol!u@h PRIVMSG #a :also live",
                "@batch=r :bob!u@h PRIVMSG #a :history",
                ":srv BATCH -p",
            ],
        );
        assert_eq!(
            observed,
            [
                Observed::Unrelated,
                Observed::Unrelated,
                Observed::Unrelated,
                Observed::Unrelated,
                Observed::Consumed,
                Observed::Unrelated,
                Observed::Consumed,
                Observed::Unrelated,
            ]
        );
    }

    #[test]
    fn nested_batches_and_non_message_lines_stay_inside_the_reply() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue("#a");
        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[
                ":srv BATCH +r chathistory #a",
                "@batch=r :srv BATCH +inner draft/multiline #a",
                "@batch=inner :bob!u@h PRIVMSG #a :nested",
                "@batch=inner :srv BATCH -inner",
                // Not allowed without event-playback: dropped, not applied.
                "@batch=r :bob!u@h JOIN #a",
                "@batch=r :bob!u@h PRIVMSG #elsewhere :wrong target",
                "@batch=r :bob!u@h PRIVMSG #a :after",
                ":srv BATCH -r",
            ],
        );
        assert!(
            observed[..7].iter().all(|o| *o == Observed::Consumed),
            "{observed:?}"
        );
        let Observed::Finished(Finished { messages, .. }) = &observed[7] else {
            panic!("{observed:?}");
        };
        let texts: Vec<_> = messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["nested", "after"]);
    }

    #[test]
    fn replies_are_bounded_and_unended_batches_report_nothing() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue("#a");
        sent(&mut requests, now);
        requests.observe(&parse(":srv BATCH +r chathistory #a"));
        for index in 0..MAX_HISTORY_LINES + 7 {
            let line = format!("@batch=r :bob!u@h PRIVMSG #a :{index}");
            assert_eq!(requests.observe(&parse(&line)), Observed::Consumed);
        }
        let Observed::Finished(Finished { messages, note, .. }) =
            requests.observe(&parse(":srv BATCH -r"))
        else {
            panic!();
        };
        assert_eq!(messages.len(), MAX_HISTORY_LINES);
        assert!(note.unwrap().contains("7 more lines"));

        // A reply that never ends yields nothing until the timeout.
        requests.enqueue("#b");
        sent(&mut requests, now);
        requests.observe(&parse(":srv BATCH +s chathistory #b"));
        requests.observe(&parse("@batch=s :bob!u@h PRIVMSG #b :partial"));
        assert!(requests.tick(now + RESPONSE_TIMEOUT / 2).is_none());
        let Some(Observed::Finished(Finished {
            channel,
            messages,
            note: Some(_),
            ..
        })) = requests.tick(now + RESPONSE_TIMEOUT)
        else {
            panic!();
        };
        assert_eq!(channel, "#b");
        assert!(messages.is_empty(), "partial replies are not shown");
        assert!(requests.next_deadline().is_none());
    }

    #[test]
    fn late_replies_to_abandoned_requests_are_swallowed() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue("#slow");
        requests.enqueue("#next");
        sent(&mut requests, now);
        assert!(requests.tick(now + RESPONSE_TIMEOUT).is_some());
        sent(&mut requests, now + RESPONSE_TIMEOUT);
        let observed = feed(
            &mut requests,
            &[
                ":srv BATCH +late chathistory #slow",
                "@batch=late :bob!u@h PRIVMSG #slow :too late",
                ":srv BATCH -late",
                ":srv BATCH +r chathistory #next",
                ":srv BATCH -r",
            ],
        );
        assert!(observed[..4].iter().all(|o| *o == Observed::Consumed));
        assert!(
            matches!(&observed[4], Observed::Finished(Finished { channel, .. }) if channel == "#next")
        );
    }

    #[test]
    fn reset_drops_the_queue_and_reports_the_outstanding_request() {
        let mut requests = HistoryRequests::default();
        requests.isupport(&parse(":srv 005 me CHATHISTORY=10 :are supported"));
        requests.enqueue("#a");
        requests.enqueue("#b");
        sent(&mut requests, Instant::now());
        let ended = requests.reset();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].channel, "#a");
        assert_eq!(ended[0].page, Page::Latest);
        assert!(requests.next_request(Instant::now()).is_none());
        assert_eq!(requests.limit(), 10, "ISUPPORT survives");
        // A reply arriving afterwards is not ours.
        assert_eq!(
            requests.observe(&parse(":srv BATCH +r chathistory #a")),
            Observed::Unrelated
        );
        requests.enqueue("#c");
        requests.forget("#C");
        assert!(requests.next_request(Instant::now()).is_none());
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut requests = HistoryRequests::default();
        for index in 0..MAX_QUEUED {
            assert!(requests.enqueue(&format!("#c{index}")).is_none());
        }
        assert!(requests.enqueue("#overflow").is_some());
        assert_eq!(requests.queue.len(), MAX_QUEUED);
    }

    fn reference(msgid: Option<&str>, secs: Option<u64>) -> MessageReference {
        MessageReference {
            msgid: msgid.map(str::to_owned),
            time: secs.map(|secs| std::time::UNIX_EPOCH + Duration::from_millis(secs * 1000 + 123)),
        }
    }

    fn finished(observed: Observed) -> Finished {
        match observed {
            Observed::Finished(finished) => finished,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn older_pages_ask_before_the_msgid_within_both_limits() {
        let mut requests = HistoryRequests::default();
        requests.isupport(&parse(
            ":srv 005 me CHATHISTORY=20 MSGREFTYPES=timestamp,msgid :are supported",
        ));
        let now = Instant::now();
        let both = reference(Some("m1"), Some(1_790_550_000));
        requests.enqueue_older("#a", 7, &both, 100).unwrap();
        // msgid is exact, so it is preferred whatever the server's order.
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY BEFORE #a msgid=m1 20"),
            "the server's limit"
        );
        requests.observe(&parse(":srv BATCH +r chathistory #a"));
        requests.observe(&parse(":srv BATCH -r"));

        requests.isupport(&parse(":srv 005 me CHATHISTORY=0 :are supported"));
        requests.enqueue_older("#a", 8, &both, 500).unwrap();
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY BEFORE #a msgid=m1 50"),
            "CayenChat's own cap"
        );
        requests.observe(&parse(":srv BATCH +s chathistory #a"));
        requests.observe(&parse(":srv BATCH -s"));

        requests.enqueue_older("#a", 9, &both, 3).unwrap();
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY BEFORE #a msgid=m1 3"),
            "the application's room"
        );
    }

    #[test]
    fn older_pages_fall_back_to_timestamps_the_server_accepts() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        let answer = |requests: &mut HistoryRequests, reference: &str| {
            requests.observe(&parse(&format!(":srv BATCH +{reference} chathistory #a")));
            requests.observe(&parse(&format!(":srv BATCH -{reference}")));
        };
        // No msgid (legacy encodings do not ask for message-tags).
        requests
            .enqueue_older("#a", 1, &reference(None, Some(1_790_550_000)), 50)
            .unwrap();
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY BEFORE #a timestamp=2026-09-27T23:00:00.123Z 50")
        );
        answer(&mut requests, "a");
        // An identifier unusable on the wire is not sent.
        requests
            .enqueue_older("#a", 2, &reference(Some("bad id"), Some(1_790_550_000)), 50)
            .unwrap();
        assert!(sent(&mut requests, now).unwrap().contains("timestamp="));
        answer(&mut requests, "b");
        // A server accepting only timestamps.
        requests.isupport(&parse(":srv 005 me MSGREFTYPES=timestamp :are supported"));
        requests
            .enqueue_older("#a", 3, &reference(Some("m1"), Some(1_790_550_000)), 50)
            .unwrap();
        assert!(sent(&mut requests, now).unwrap().contains("timestamp="));
        answer(&mut requests, "c");
        // A message with only a msgid cannot be referred to there.
        let refused = requests
            .enqueue_older("#a", 4, &reference(Some("m1"), None), 50)
            .unwrap_err();
        assert_eq!(refused.page, Page::Before { request: 4 });
        assert_eq!(refused.older_status(), OlderHistoryStatus::Failed);
        // Nothing we know.
        requests.isupport(&parse(":srv 005 me MSGREFTYPES=future :are supported"));
        assert!(
            requests
                .enqueue_older("#a", 5, &reference(Some("m1"), Some(1)), 50)
                .is_err()
        );
        // Removing the token restores both.
        requests.isupport(&parse(":srv 005 me -MSGREFTYPES :are supported"));
        requests
            .enqueue_older("#a", 6, &reference(Some("m1"), None), 50)
            .unwrap();
        assert!(sent(&mut requests, now).unwrap().contains("msgid=m1"));
    }

    #[test]
    fn older_pages_jump_the_join_queue_once_per_channel() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue("#a");
        requests.enqueue("#b");
        let anchor = reference(Some("m1"), None);
        requests.enqueue_older("#c", 1, &anchor, 50).unwrap();
        let again = requests.enqueue_older("#C", 2, &anchor, 50).unwrap_err();
        assert_eq!(again.page, Page::Before { request: 2 });
        assert!(
            sent(&mut requests, now)
                .unwrap()
                .starts_with("CHATHISTORY BEFORE #c")
        );
        // Still one per channel while it is being answered, while a LATEST
        // for the same channel may queue beside it.
        assert!(requests.enqueue_older("#c", 3, &anchor, 50).is_err());
        assert!(requests.enqueue("#c").is_none());
        assert_eq!(requests.queue.len(), 3);
        assert!(sent(&mut requests, now).is_none(), "one at a time");
    }

    #[test]
    fn older_page_replies_report_the_beginning_of_history() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        let anchor = reference(Some("m9"), None);

        requests.enqueue_older("#a", 1, &anchor, 50).unwrap();
        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[
                ":srv BATCH +r chathistory #a",
                "@batch=r;msgid=m7;time=2026-09-27T23:50:00.000Z :bob!u@h PRIVMSG #a :seven",
                "@batch=r;msgid=m8 :bob!u@h PRIVMSG #a :eight",
                ":srv BATCH -r",
            ],
        );
        let page = finished(observed.into_iter().last().unwrap());
        assert_eq!(page.page, Page::Before { request: 1 });
        assert_eq!(page.older_status(), OlderHistoryStatus::More);
        assert_eq!(page.messages.len(), 2);

        requests.enqueue_older("#a", 2, &anchor, 50).unwrap();
        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[
                "@draft/chathistory-end :srv BATCH +s chathistory #a",
                "@batch=s;msgid=m1 :bob!u@h PRIVMSG #a :first",
                ":srv BATCH -s",
            ],
        );
        let page = finished(observed.into_iter().last().unwrap());
        assert_eq!(page.older_status(), OlderHistoryStatus::Beginning);
        assert_eq!(page.messages.len(), 1);

        requests.enqueue_older("#a", 3, &anchor, 50).unwrap();
        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[":srv BATCH +t chathistory #a", ":srv BATCH -t"],
        );
        let page = finished(observed.into_iter().last().unwrap());
        assert_eq!(page.older_status(), OlderHistoryStatus::Beginning, "empty");

        requests.enqueue_older("#a", 4, &anchor, 50).unwrap();
        sent(&mut requests, now);
        let page = finished(requests.observe(&parse(
            ":srv FAIL CHATHISTORY INVALID_MSGREFTYPE BEFORE #a :msgid-based history requests are not supported",
        )));
        assert_eq!(page.page, Page::Before { request: 4 });
        assert_eq!(page.older_status(), OlderHistoryStatus::Failed);

        requests.enqueue_older("#a", 5, &anchor, 50).unwrap();
        sent(&mut requests, now);
        let Some(Observed::Finished(page)) = requests.tick(now + RESPONSE_TIMEOUT) else {
            panic!();
        };
        assert_eq!(page.page, Page::Before { request: 5 });
        assert_eq!(page.older_status(), OlderHistoryStatus::Failed);
    }

    #[test]
    fn parting_or_losing_the_capability_ends_older_page_requests() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        let anchor = reference(Some("m1"), None);
        requests.enqueue_older("#a", 1, &anchor, 50).unwrap();
        requests.enqueue("#b");
        requests.enqueue_older("#b", 2, &anchor, 50).unwrap();
        let ended = requests.forget("#B");
        assert_eq!(ended.len(), 1, "the LATEST is dropped silently");
        assert_eq!(ended[0].page, Page::Before { request: 2 });
        assert!(ended[0].failed);

        requests.enqueue_older("#c", 3, &anchor, 50).unwrap();
        sent(&mut requests, now);
        let ended = requests.reset();
        let pages: Vec<_> = ended.iter().map(|f| f.page.clone()).collect();
        assert_eq!(
            pages,
            [Page::Before { request: 3 }, Page::Before { request: 1 }],
            "the one being answered, then the queued one"
        );
        assert!(ended.iter().all(|f| f.failed && f.messages.is_empty()));
        assert!(requests.queue.is_empty() && requests.next_deadline().is_none());
    }

    fn resume(channel: &str, msgid: Option<&str>, secs: Option<u64>) -> HistoryResume {
        HistoryResume {
            channel: channel.into(),
            after: reference(msgid, secs),
        }
    }

    #[test]
    fn the_first_join_after_a_reconnect_asks_only_for_what_came_after() {
        let mut requests = HistoryRequests::with_resume(vec![
            resume("#a", Some("m9"), Some(1_790_550_000)),
            resume("#b", None, Some(1_790_550_000)),
        ]);
        requests.isupport(&parse(":srv 005 me CHATHISTORY=30 :are supported"));
        let now = Instant::now();
        requests.enqueue("#A");
        requests.enqueue("#b");
        requests.enqueue("#c");
        let (channel, page, _) = requests.next_request(now).unwrap();
        assert_eq!((channel.as_str(), &page), ("#A", &Page::Resume));
        requests.observe(&parse(":srv BATCH +r chathistory #a"));
        requests.observe(&parse(":srv BATCH -r"));
        // A timestamp reference reaches back a few seconds for clock skew;
        // what that repeats is dropped as duplicates by the application.
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #b timestamp=2026-09-27T22:59:55.123Z 30")
        );
        requests.observe(&parse(":srv BATCH +s chathistory #b"));
        requests.observe(&parse(":srv BATCH -s"));
        // A channel with nothing to resume from asks for its latest lines.
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #c * 30")
        );
        requests.observe(&parse(":srv BATCH +t chathistory #c"));
        requests.observe(&parse(":srv BATCH -t"));
        // Each reference is used once: joining again asks for the latest.
        requests.enqueue("#a");
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #a * 30")
        );
    }

    #[test]
    fn resuming_uses_msgid_first_and_nothing_the_server_refuses() {
        let now = Instant::now();
        let mut requests =
            HistoryRequests::with_resume(vec![resume("#a", Some("m9"), Some(1_790_550_000))]);
        requests.enqueue("#a");
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #a msgid=m9 50")
        );

        let mut requests = HistoryRequests::with_resume(vec![resume("#a", Some("m9"), None)]);
        requests.isupport(&parse(":srv 005 me MSGREFTYPES=timestamp :are supported"));
        requests.enqueue("#a");
        let (_, page, command) = requests.next_request(now).unwrap();
        assert_eq!(page, Page::Latest, "no usable reference: not a resume");
        assert!(
            IrcMessage::from(command)
                .to_string()
                .contains("LATEST #a * 50")
        );
    }

    #[test]
    fn a_full_resumed_reply_may_have_missed_lines() {
        let now = Instant::now();
        let full = |end: bool, lines: usize| {
            let mut requests = HistoryRequests::with_resume(vec![resume("#a", Some("m1"), None)]);
            requests.isupport(&parse(":srv 005 me CHATHISTORY=3 :are supported"));
            requests.enqueue("#a");
            sent(&mut requests, now);
            let tag = if end { "@draft/chathistory-end " } else { "" };
            requests.observe(&parse(&format!("{tag}:srv BATCH +r chathistory #a")));
            for index in 0..lines {
                requests.observe(&parse(&format!("@batch=r :bob!u@h PRIVMSG #a :{index}")));
            }
            finished(requests.observe(&parse(":srv BATCH -r")))
        };
        let reply = full(false, 3);
        assert_eq!(reply.page, Page::Resume);
        assert!(reply.incomplete);
        assert!(!full(true, 3).incomplete, "the server says that was all");
        assert!(!full(false, 2).incomplete, "fewer than asked for");
        assert!(full(false, 0).messages.is_empty());

        // A plain LATEST is never incomplete: its limit is the point.
        let mut requests = HistoryRequests::default();
        requests.isupport(&parse(":srv 005 me CHATHISTORY=3 :are supported"));
        requests.enqueue("#a");
        sent(&mut requests, now);
        requests.observe(&parse(":srv BATCH +r chathistory #a"));
        for index in 0..3 {
            requests.observe(&parse(&format!("@batch=r :bob!u@h PRIVMSG #a :{index}")));
        }
        assert!(!finished(requests.observe(&parse(":srv BATCH -r"))).incomplete);
    }

    #[test]
    fn a_failed_resume_falls_back_to_the_latest_lines() {
        let now = Instant::now();
        let mut requests = HistoryRequests::with_resume(vec![
            resume("#a", Some("m1"), None),
            resume("#b", Some("m2"), None),
        ]);
        requests.enqueue("#a");
        sent(&mut requests, now);
        let failed = finished(requests.observe(&parse(
            ":srv FAIL CHATHISTORY INVALID_MSGREFTYPE LATEST #a :msgid-based history requests are not supported",
        )));
        assert_eq!(failed.page, Page::Resume);
        assert!(failed.failed && !failed.incomplete);
        assert_eq!(
            sent(&mut requests, now).as_deref(),
            Some("CHATHISTORY LATEST #a * 50")
        );
        // A timeout falls back too, once: the fallback is a plain LATEST.
        let Some(Observed::Finished(_)) = requests.tick(now + RESPONSE_TIMEOUT) else {
            panic!();
        };
        assert!(requests.next_request(now).is_none());
        // Losing the capability keeps unused references for a later join.
        requests.reset();
        requests.enqueue("#b");
        assert!(sent(&mut requests, now).unwrap().contains("msgid=m2"));
    }

    fn at(secs: u64) -> SystemTime {
        std::time::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// Runs a TARGETS request answered with `lines` and returns what
    /// finished, with the requests that follow drained.
    fn discover(requests: &mut HistoryRequests, lines: &[&str]) -> (Finished, Vec<String>) {
        let now = Instant::now();
        requests.enqueue_targets(at(1_790_000_000), at(1_790_003_600));
        let mut all = vec![":srv BATCH +t draft/chathistory-targets".to_owned()];
        all.extend(lines.iter().map(|line| format!("@batch=t {line}")));
        all.push(":srv BATCH -t".into());
        let asked = sent(requests, now).unwrap();
        assert_eq!(
            asked,
            "CHATHISTORY TARGETS timestamp=2026-09-21T14:13:20.000Z timestamp=2026-09-21T15:18:20.000Z 16",
            "the window starts at the disconnect and leaves room for clock skew"
        );
        let observed = feed(
            requests,
            &all.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert!(
            observed[..observed.len() - 1]
                .iter()
                .all(|o| matches!(o, Observed::Consumed))
        );
        let finished = finished(observed.into_iter().last().unwrap());
        let mut followers = Vec::new();
        while let Some(line) = sent(requests, now) {
            followers.push(line);
            requests.outstanding = None;
        }
        (finished, followers)
    }

    #[test]
    fn targets_ask_for_the_latest_lines_of_unknown_direct_messages_only() {
        let mut requests = HistoryRequests::default();
        let (finished, followers) = discover(
            &mut requests,
            &[
                "CHATHISTORY TARGETS carol 2026-09-21T10:00:00.000Z",
                "CHATHISTORY TARGETS #chan 2026-09-21T10:30:00.000Z",
                "CHATHISTORY TARGETS Bob 2026-09-21T10:20:00.000Z",
                // The same peer in another case: once.
                "CHATHISTORY TARGETS BOB 2026-09-21T10:10:00.000Z",
                "CHATHISTORY TARGETS bad!name 2026-09-21T10:11:00.000Z",
                "CHATHISTORY TARGETS dave not-a-time",
            ],
        );
        assert_eq!(finished.targets, 5, "unparsable lines are skipped");
        assert!(!finished.failed && finished.messages.is_empty());
        // Newest first; the channel is not requested (a joined channel asks
        // on join); no case duplicates.
        assert_eq!(
            followers,
            [
                "CHATHISTORY LATEST Bob * 50",
                "CHATHISTORY LATEST carol * 50",
            ]
        );
    }

    #[test]
    fn discovery_is_bounded_and_shares_the_request_queue() {
        let mut requests = HistoryRequests::default();
        let lines: Vec<String> = (0..80)
            .map(|n| {
                format!(
                    "CHATHISTORY TARGETS peer{n} 2026-09-21T10:{:02}:00.000Z",
                    n % 60
                )
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (finished, followers) = discover(&mut requests, &refs);
        assert_eq!(finished.targets, MAX_TARGET_ENTRIES);
        assert_eq!(followers.len(), MAX_DISCOVERED);
        // A hostile server cannot make TARGETS requests pile up: the caller
        // asks once, and a full queue refuses another.
        for _ in 0..MAX_QUEUED + 5 {
            requests.enqueue_targets(at(1), at(2));
        }
        assert!(requests.queue.len() <= MAX_QUEUED);
    }

    #[test]
    fn a_failed_or_unanswered_targets_request_ends_without_asking_anything() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue_targets(at(1_790_000_000), at(1_790_003_600));
        sent(&mut requests, now).unwrap();
        let observed = requests.observe(&parse("FAIL CHATHISTORY INVALID_PARAMS TARGETS :bad"));
        let ended = finished(observed);
        assert!(ended.failed);
        assert!(
            ended
                .note
                .unwrap()
                .starts_with("History targets is unavailable")
        );
        assert!(sent(&mut requests, now).is_none());

        requests.enqueue_targets(at(1_790_000_000), at(1_790_003_600));
        sent(&mut requests, now).unwrap();
        let timed_out = requests.tick(now + RESPONSE_TIMEOUT).unwrap();
        assert!(finished(timed_out).failed);
    }

    #[test]
    fn direct_message_history_keeps_both_directions_only() {
        let mut requests = HistoryRequests::default();
        let now = Instant::now();
        requests.enqueue("bob");
        assert_eq!(
            sent(&mut requests, now).unwrap(),
            "CHATHISTORY LATEST bob * 50"
        );
        let observed = feed(
            &mut requests,
            &[
                ":srv BATCH +r chathistory bob",
                "@batch=r;msgid=1;time=2026-09-21T10:00:00.000Z :bob!u@h PRIVMSG alice :hi",
                "@batch=r;msgid=2;time=2026-09-21T10:01:00.000Z :alice!u@h PRIVMSG bob :yo",
                "@batch=r;msgid=3 :carol!u@h PRIVMSG alice :not this conversation",
                "@batch=r;msgid=4 :bob!u@h NOTICE alice :\u{1}VERSION\u{1}",
                ":srv BATCH -r",
            ],
        );
        let page = finished(observed.into_iter().last().unwrap());
        let lines: Vec<_> = page
            .messages
            .iter()
            .map(|m| (m.sender.as_str(), m.text.as_str()))
            .collect();
        assert_eq!(lines, [("bob", "hi"), ("alice", "yo")]);
    }
}
