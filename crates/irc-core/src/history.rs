//! Recent channel history requested with IRCv3 `draft/chathistory`
//! (opt-in per server, experimental).
//!
//! When we join a channel, `CHATHISTORY LATEST <channel> * <limit>` asks for
//! its latest lines. Requests go out one at a time from a bounded queue, so
//! joining many channels does not burst commands. The reply is a
//! `chathistory` batch whose single parameter is the channel; every line in
//! it (and in batches nested inside it) is consumed here, never treated as
//! live traffic, and its PRIVMSG/NOTICE lines are buffered (bounded) until the
//! batch ends and then reported together. A batch that never ends reports
//! nothing, `FAIL CHATHISTORY` or a timeout ends the request, and everything
//! lives in the connection's worker, so a reconnect starts empty and cannot
//! receive an old connection's answer.
//!
//! Only negotiated `batch` makes a reply recognizable, so the capability is
//! requested only after `batch` is acknowledged (see `cap`). The
//! specification also allows replies without batches; this client does not
//! use that form.

use std::{collections::VecDeque, time::Duration, time::SystemTime};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix, Response};
use tokio::time::Instant;

use crate::tags;

/// Lines asked for per channel. The server's `CHATHISTORY` limit lowers it;
/// this client never asks for more.
pub const HISTORY_LIMIT: usize = 50;
/// Lines kept from one reply. Servers MAY return more than asked for; the
/// rest is dropped.
pub const MAX_HISTORY_LINES: usize = 100;
/// Channels waiting for their request. Joining more at once skips the rest.
const MAX_QUEUED: usize = 64;
/// A request without a reply is given up after this long, and the next one
/// is sent.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Abandoned requests whose late reply is still swallowed.
const MAX_ABANDONED: usize = 8;
/// Nested batches followed inside one reply.
const MAX_NESTED: usize = 16;
/// Longest batch reference followed.
const MAX_REFERENCE_BYTES: usize = 64;

/// One line of requested history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryMessage {
    pub sender: String,
    pub text: String,
    pub notice: bool,
    pub server_time: Option<SystemTime>,
    pub msgid: Option<String>,
}

/// What [`HistoryRequests::observe`] did with an incoming line.
#[derive(Debug, PartialEq)]
pub(crate) enum Observed {
    /// Not part of a history reply; process it as usual.
    Unrelated,
    /// Part of a reply; nothing else may see it.
    Consumed,
    /// The request for `channel` ended: its lines (possibly none), or a
    /// failure note.
    Finished {
        channel: String,
        messages: Vec<HistoryMessage>,
        note: Option<String>,
    },
}

#[derive(Debug)]
struct Outstanding {
    channel: String,
    sent: Instant,
    /// The reply batch's reference once it opened.
    reference: Option<String>,
    nested: Vec<String>,
    messages: Vec<HistoryMessage>,
    dropped: usize,
}

#[derive(Debug, Default)]
pub(crate) struct HistoryRequests {
    /// The server's `CHATHISTORY` ISUPPORT value; 0 or absent means none.
    server_limit: Option<usize>,
    queue: VecDeque<String>,
    outstanding: Option<Outstanding>,
    /// Channels of abandoned requests: a late reply is swallowed, not shown
    /// as live or as another request's reply.
    abandoned: VecDeque<String>,
    /// A late reply being swallowed, and its nested batches.
    discarding: Vec<String>,
}

impl HistoryRequests {
    /// Reads `CHATHISTORY=<n>` (and `-CHATHISTORY`) from RPL_ISUPPORT.
    pub(crate) fn isupport(&mut self, message: &IrcMessage) {
        let IrcCommand::Response(Response::RPL_ISUPPORT, args) = &message.command else {
            return;
        };
        // The first argument is our nickname, the last the trailing text.
        for token in args.iter().skip(1).flat_map(|arg| arg.split_whitespace()) {
            if token.eq_ignore_ascii_case("-CHATHISTORY") {
                self.server_limit = None;
            } else if let Some((name, value)) = token.split_once('=')
                && name.eq_ignore_ascii_case("CHATHISTORY")
            {
                self.server_limit = value.parse().ok();
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

    /// Queues a request for `channel`, unless it is already queued or being
    /// answered. Returns a note when the queue is full.
    pub(crate) fn enqueue(&mut self, channel: &str) -> Option<String> {
        let same = |name: &String| crate::text::same_nickname(name, channel);
        if self.queue.iter().any(same)
            || self.outstanding.as_ref().is_some_and(|o| same(&o.channel))
        {
            return None;
        }
        if self.queue.len() >= MAX_QUEUED {
            return Some(format!(
                "History for {channel} was not requested: {MAX_QUEUED} channels are already waiting."
            ));
        }
        self.queue.push_back(channel.to_owned());
        None
    }

    /// Drops a queued request for a channel we left.
    pub(crate) fn forget(&mut self, channel: &str) {
        self.queue
            .retain(|name| !crate::text::same_nickname(name, channel));
    }

    /// The next request when none is being answered.
    pub(crate) fn next_request(&mut self, now: Instant) -> Option<(String, IrcCommand)> {
        if self.outstanding.is_some() {
            return None;
        }
        let channel = self.queue.pop_front()?;
        let command = IrcCommand::Raw(
            "CHATHISTORY".into(),
            vec![
                "LATEST".into(),
                channel.clone(),
                "*".into(),
                self.limit().to_string(),
            ],
        );
        self.outstanding = Some(Outstanding {
            channel: channel.clone(),
            sent: now,
            reference: None,
            nested: Vec::new(),
            messages: Vec::new(),
            dropped: 0,
        });
        Some((channel, command))
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
        Some(Observed::Finished {
            note: Some(format!(
                "History for {} was not answered within {} s.",
                outstanding.channel,
                RESPONSE_TIMEOUT.as_secs()
            )),
            channel: outstanding.channel,
            messages: Vec::new(),
        })
    }

    /// The capability went away: nothing is requested any more. Returns the
    /// request being answered, which ends without lines.
    pub(crate) fn reset(&mut self) -> Option<String> {
        let outstanding = self.outstanding.take();
        *self = Self {
            server_limit: self.server_limit,
            ..Self::default()
        };
        outstanding.map(|outstanding| outstanding.channel)
    }

    pub(crate) fn observe(&mut self, message: &IrcMessage) -> Observed {
        let batch_tag = tags::tag_value(message, "batch");
        if let IrcCommand::BATCH(reference, kind, params) = &message.command {
            return self.observe_batch(
                reference,
                kind.as_ref().map(|k| k.to_str()),
                params,
                batch_tag,
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
            return Observed::Finished {
                channel: outstanding.channel,
                messages: Vec::new(),
                note: Some(note),
            };
        }
        Observed::Unrelated
    }

    fn observe_batch(
        &mut self,
        reference: &str,
        kind: Option<&str>,
        params: &Option<Vec<String>>,
        parent: Option<&str>,
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
            return Observed::Finished {
                channel: outstanding.channel,
                messages: outstanding.messages,
                note,
            };
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
        // irc-proto upper-cases batch types.
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

/// A PRIVMSG or NOTICE from a user to `channel` in a reply.
fn history_line(message: &IrcMessage, channel: &str) -> Option<HistoryMessage> {
    let (target, text, notice) = match &message.command {
        IrcCommand::PRIVMSG(target, text) => (target, text, false),
        IrcCommand::NOTICE(target, text) => (target, text, true),
        _ => return None,
    };
    if !crate::text::same_nickname(target, channel) {
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
            .map(|(_, command)| IrcMessage::from(command).to_string().trim_end().to_owned())
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
                ":srv BATCH -r1",
            ],
        );
        assert!(finished[..3].iter().all(|o| *o == Observed::Consumed));
        let Observed::Finished {
            channel,
            messages,
            note,
        } = &finished[3]
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
            matches!(&observed[1], Observed::Finished { messages, note: None, .. } if messages.is_empty())
        );

        sent(&mut requests, now);
        let observed = feed(
            &mut requests,
            &[":srv FAIL CHATHISTORY INVALID_TARGET LATEST #fail :Messages could not be retrieved"],
        );
        assert!(
            matches!(&observed[0], Observed::Finished { channel, note: Some(note), .. }
            if channel == "#fail" && note.contains("INVALID_TARGET"))
        );

        sent(&mut requests, now);
        let observed = feed(&mut requests, &[":srv 421 me CHATHISTORY :Unknown command"]);
        assert!(
            matches!(&observed[0], Observed::Finished { channel, note: Some(_), .. } if channel == "#old")
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
        let Observed::Finished { messages, .. } = &observed[7] else {
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
        let Observed::Finished { messages, note, .. } = requests.observe(&parse(":srv BATCH -r"))
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
        let Some(Observed::Finished {
            channel,
            messages,
            note: Some(_),
        }) = requests.tick(now + RESPONSE_TIMEOUT)
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
        assert!(matches!(&observed[4], Observed::Finished { channel, .. } if channel == "#next"));
    }

    #[test]
    fn reset_drops_the_queue_and_reports_the_outstanding_request() {
        let mut requests = HistoryRequests::default();
        requests.isupport(&parse(":srv 005 me CHATHISTORY=10 :are supported"));
        requests.enqueue("#a");
        requests.enqueue("#b");
        sent(&mut requests, Instant::now());
        assert_eq!(requests.reset().as_deref(), Some("#a"));
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
}
