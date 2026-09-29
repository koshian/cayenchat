//! Server-confirmed sending (opt-in per server): IRCv3 `echo-message` and
//! `labeled-response`.
//!
//! The application still shows a message the moment it is sent (an
//! optimistic local line). With `echo-message` the server sends the message
//! back with its final text, id and time; this module matches that echo to
//! the pending local line so it is confirmed in place, never shown twice.
//!
//! Matching, strongest first:
//!
//! 1. With `labeled-response`, each sent message carries a `label` tag (a
//!    short opaque counter, never reused on a connection). The echo, a
//!    labeled `ACK`, an error reply, or any of them inside a
//!    `labeled-response` batch names the label. Text is not compared, so a
//!    message the server rewrote still matches. Only labeled lines can match:
//!    an unlabeled copy of our own nickname's message is another client's.
//! 2. Without labels, the oldest pending message to the same target
//!    (IRC-casemapped) whose text equals the echo. A message the server
//!    changed cannot be recognized this way and is shown as a new line;
//!    that is the price of not having labels.
//!
//! Pending messages are bounded ([`MAX_PENDING`]) and expire after
//! [`PENDING_TTL`] (reported as unconfirmed). Nothing outlives the
//! connection: the application marks what was still pending when the link
//! ended as not confirmed.

use std::{collections::VecDeque, time::Duration};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Response, message::Tag};
use tokio::time::Instant;

use crate::{Event, tags, text::same_nickname};

/// Messages waiting for their echo; a sender beyond it is not tracked.
pub(crate) const MAX_PENDING: usize = 32;
/// An echo later than this is not matched; the line is reported unconfirmed.
const PENDING_TTL: Duration = Duration::from_secs(60);
/// Label batches followed at once.
const MAX_BATCHES: usize = 16;
/// Longest batch reference followed.
const MAX_REFERENCE_BYTES: usize = 64;

#[derive(Debug)]
struct Pending {
    local_id: u64,
    label: Option<String>,
    target: String,
    notice: bool,
    text: String,
    /// The line shown locally differs from the wire text (a CTCP ACTION, a
    /// redacted service secret): the server's text is not put in its place.
    keep_text: bool,
    sent: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct Echoes {
    pending: VecDeque<Pending>,
    next_label: u64,
    /// Open `labeled-response` batches: (reference, label).
    batches: Vec<(String, String)>,
}

/// A label of at most 64 bytes: `c` and the counter in base 36.
fn label_for(counter: u64) -> String {
    let mut digits = Vec::new();
    let mut n = counter;
    loop {
        digits.push(char::from_digit((n % 36) as u32, 36).expect("digit below 36"));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.push('c');
    digits.iter().rev().collect()
}

impl Echoes {
    /// Registers a message about to be sent and returns it with its label
    /// when `labeled`. `None` means it is not tracked (too many waiting).
    pub(crate) fn track(
        &mut self,
        local_id: u64,
        target: &str,
        notice: bool,
        text: &str,
        keep_text: bool,
        labeled: bool,
    ) -> Option<Option<String>> {
        if self.pending.len() >= MAX_PENDING {
            return None;
        }
        // A counter never repeats on a connection, so a label cannot be
        // reused while its request is outstanding.
        let label = labeled.then(|| {
            self.next_label += 1;
            label_for(self.next_label)
        });
        self.pending.push_back(Pending {
            local_id,
            label: label.clone(),
            target: target.to_owned(),
            notice,
            text: text.to_owned(),
            keep_text,
            sent: Instant::now(),
        });
        Some(label)
    }

    /// Stops tracking a message that could not be sent.
    pub(crate) fn untrack(&mut self, local_id: u64) {
        self.pending.retain(|pending| pending.local_id != local_id);
    }

    /// Ids still waiting, oldest first.
    pub(crate) fn outstanding(&self) -> Vec<u64> {
        self.pending.iter().map(|p| p.local_id).collect()
    }

    /// Messages whose echo did not come in time.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        while self
            .pending
            .front()
            .is_some_and(|p| now.duration_since(p.sent) >= PENDING_TTL)
        {
            let pending = self.pending.pop_front().expect("front exists");
            events.push(Event::OutgoingFailed {
                local_id: pending.local_id,
                reason: "The server did not confirm the message.".into(),
            });
        }
        events
    }

    /// Forgets every batch and pending message: the capability was withdrawn.
    /// Returns the failures to report.
    pub(crate) fn reset(&mut self) -> Vec<Event> {
        self.batches.clear();
        self.pending
            .drain(..)
            .map(|pending| Event::OutgoingFailed {
                local_id: pending.local_id,
                reason: "The server stopped confirming messages.".into(),
            })
            .collect()
    }

    fn take_labeled(&mut self, label: &str) -> Option<Pending> {
        let index = self
            .pending
            .iter()
            .position(|pending| pending.label.as_deref() == Some(label))?;
        self.pending.remove(index)
    }

    fn confirmed(pending: Pending, message: Option<&IrcMessage>) -> Event {
        let (text, msgid, server_time) = match message.map(|m| (&m.command, m)) {
            Some((IrcCommand::PRIVMSG(_, text) | IrcCommand::NOTICE(_, text), message)) => (
                // The final text replaces the local one only when it changed
                // and the local line is the text itself.
                (*text != pending.text && !pending.keep_text).then(|| text.clone()),
                tags::msgid(message).map(str::to_owned),
                tags::server_time(message, true),
            ),
            _ => (None, None, None),
        };
        Event::OutgoingConfirmed {
            local_id: pending.local_id,
            text,
            msgid,
            server_time,
        }
    }

    /// Looks at an incoming line. `Some` means it belonged to a message we
    /// sent (an echo, ACK, error, or their batch framing) and must not be
    /// shown as anything else. `labeled` tells whether `labeled-response`
    /// is enabled; `current_nick` is our nickname.
    pub(crate) fn observe(
        &mut self,
        message: &IrcMessage,
        current_nick: &str,
        labeled: bool,
    ) -> Option<Vec<Event>> {
        let mut events = Vec::new();
        // The start and end of a labeled batch.
        if let IrcCommand::BATCH(reference, kind, _) = &message.command {
            if let Some(reference) = reference.strip_prefix('+') {
                let label = tags::tag_value(message, "label");
                let is_labeled = kind
                    .as_ref()
                    .is_some_and(|k| k.to_str() == "LABELED-RESPONSE");
                if let Some(label) = label
                    && labeled
                    && (is_labeled
                        || self
                            .pending
                            .iter()
                            .any(|p| p.label.as_deref() == Some(label)))
                    && self
                        .pending
                        .iter()
                        .any(|p| p.label.as_deref() == Some(label))
                    && !reference.is_empty()
                    && reference.len() <= MAX_REFERENCE_BYTES
                {
                    if self.batches.len() >= MAX_BATCHES {
                        self.batches.remove(0);
                    }
                    self.batches.retain(|(open, _)| open != reference);
                    self.batches.push((reference.to_owned(), label.to_owned()));
                    return Some(events);
                }
            } else if let Some(reference) = reference.strip_prefix('-')
                && let Some(index) = self.batches.iter().position(|(open, _)| open == reference)
            {
                self.batches.remove(index);
                return Some(events);
            }
            return (!events.is_empty()).then_some(events);
        }
        // The label naming this line: its own tag, or its batch's.
        let label = tags::tag_value(message, "label")
            .map(str::to_owned)
            .or_else(|| {
                let reference = tags::tag_value(message, "batch")?;
                self.batches
                    .iter()
                    .find(|(open, _)| open == reference)
                    .map(|(_, label)| label.clone())
            });
        if labeled && let Some(label) = label {
            let ours = |m: &IrcMessage| {
                m.source_nickname()
                    .is_some_and(|nick| same_nickname(nick, current_nick))
            };
            match &message.command {
                IrcCommand::PRIVMSG(..) | IrcCommand::NOTICE(..) if ours(message) => {
                    if let Some(pending) = self.take_labeled(&label) {
                        events.push(Self::confirmed(pending, Some(message)));
                        return Some(events);
                    }
                }
                IrcCommand::Raw(verb, _) if verb.eq_ignore_ascii_case("ACK") => {
                    if let Some(pending) = self.take_labeled(&label) {
                        events.push(Self::confirmed(pending, None));
                        return Some(events);
                    }
                }
                IrcCommand::Raw(verb, args) if verb.eq_ignore_ascii_case("FAIL") => {
                    if let Some(pending) = self.take_labeled(&label) {
                        events.push(Event::OutgoingFailed {
                            local_id: pending.local_id,
                            reason: args.last().cloned().unwrap_or_default(),
                        });
                        return Some(events);
                    }
                }
                IrcCommand::Response(response, args) if is_error(response) => {
                    if let Some(pending) = self.take_labeled(&label) {
                        events.push(Event::OutgoingFailed {
                            local_id: pending.local_id,
                            reason: args.last().cloned().unwrap_or_default(),
                        });
                        return Some(events);
                    }
                }
                _ => {}
            }
            return (!events.is_empty()).then_some(events);
        }
        // No label: without labeled-response, the echo of one of ours.
        if !labeled
            && let IrcCommand::PRIVMSG(target, text) | IrcCommand::NOTICE(target, text) =
                &message.command
            && message
                .source_nickname()
                .is_some_and(|nick| same_nickname(nick, current_nick))
        {
            let notice = matches!(message.command, IrcCommand::NOTICE(..));
            if let Some(index) = self.pending.iter().position(|pending| {
                pending.label.is_none()
                    && pending.notice == notice
                    && same_nickname(&pending.target, target)
                    && pending.text == *text
            }) && let Some(pending) = self.pending.remove(index)
            {
                events.push(Self::confirmed(pending, Some(message)));
                return Some(events);
            }
        }
        (!events.is_empty()).then_some(events)
    }
}

/// Numeric replies that say a command failed (4xx and 5xx).
fn is_error(response: &Response) -> bool {
    (400..600).contains(&(*response as u16))
}

/// The message with `label` as its tag (added to what the caller built).
pub(crate) fn with_label(mut message: IrcMessage, label: Option<String>) -> IrcMessage {
    if let Some(label) = label {
        message.tags = Some(vec![Tag("label".into(), Some(label))]);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    fn now() -> Instant {
        Instant::now()
    }

    fn describe(events: Option<Vec<Event>>) -> Vec<String> {
        events
            .expect("belongs to a sent message")
            .into_iter()
            .map(|event| match event {
                Event::OutgoingConfirmed {
                    local_id,
                    text,
                    msgid,
                    ..
                } => format!(
                    "{local_id} ok {} {}",
                    text.unwrap_or_else(|| "=".into()),
                    msgid.unwrap_or_default()
                ),
                Event::OutgoingFailed { local_id, reason } => format!("{local_id} failed {reason}"),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn labels_are_short_distinct_and_never_reused() {
        let mut echoes = Echoes::default();
        let mut seen = std::collections::HashSet::new();
        for id in 0..1000u64 {
            echoes.pending.clear();
            let label = echoes
                .track(id, "#a", false, "x", false, true)
                .unwrap()
                .unwrap();
            assert!(label.len() <= 64 && label.is_ascii());
            assert!(seen.insert(label), "a label repeated");
        }
        assert_eq!(label_for(1), "c1");
        assert_eq!(label_for(36), "c10");
    }

    #[test]
    fn a_labeled_echo_confirms_with_the_servers_final_text() {
        let mut echoes = Echoes::default();
        let label = echoes
            .track(7, "#a", false, "hello  world", false, true)
            .unwrap()
            .unwrap();
        // Another client's message as us has no label: it is not ours.
        assert!(
            echoes
                .observe(&line(":me!u@h PRIVMSG #a :from my phone"), "me", true)
                .is_none()
        );
        let echo = format!(
            "@label={label};msgid=m9;time=2026-09-28T10:00:00.000Z :ME!u@h PRIVMSG #a :hello world"
        );
        assert_eq!(
            describe(echoes.observe(&line(&echo), "me", true)),
            ["7 ok hello world m9"],
            "the server collapsed the double space"
        );
        assert!(echoes.pending.is_empty());
        // A repeated echo finds nothing pending.
        assert!(echoes.observe(&line(&echo), "me", true).is_none());
    }

    #[test]
    fn acks_errors_and_batches_settle_labeled_messages() {
        let mut echoes = Echoes::default();
        let a = echoes
            .track(1, "#a", false, "one", false, true)
            .unwrap()
            .unwrap();
        let b = echoes
            .track(2, "#a", false, "two", false, true)
            .unwrap()
            .unwrap();
        let c = echoes
            .track(3, "bob", false, "three", false, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            describe(echoes.observe(&line(&format!("@label={a} :srv ACK")), "me", true)),
            ["1 ok = "]
        );
        assert_eq!(
            describe(echoes.observe(
                &line(&format!(
                    "@label={b} :srv 404 me #a :Cannot send to channel"
                )),
                "me",
                true
            )),
            ["2 failed Cannot send to channel"]
        );
        // A multi-message response: batch start carries the label; lines
        // inside are matched through the batch reference.
        let start = format!("@label={c} :srv BATCH +x labeled-response");
        assert_eq!(echoes.observe(&line(&start), "me", true), Some(Vec::new()));
        assert_eq!(
            describe(echoes.observe(
                &line("@batch=x;msgid=m1 :me!u@h PRIVMSG bob :three"),
                "me",
                true
            )),
            ["3 ok = m1"]
        );
        assert_eq!(
            echoes.observe(&line(":srv BATCH -x"), "me", true),
            Some(Vec::new())
        );
        assert!(echoes.batches.is_empty());
        // FAIL replies too.
        let d = echoes
            .track(4, "#a", false, "four", false, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            describe(echoes.observe(
                &line(&format!("@label={d} :srv FAIL PRIVMSG BLOCKED :no")),
                "me",
                true
            )),
            ["4 failed no"]
        );
    }

    #[test]
    fn without_labels_only_an_identical_text_to_the_same_target_matches() {
        let mut echoes = Echoes::default();
        assert_eq!(echoes.track(1, "#A", false, "hi", false, false), Some(None));
        echoes.track(2, "bob", true, "psst", false, false);
        // Other client's differing text, wrong target, wrong kind: not ours.
        for other in [
            ":me!u@h PRIVMSG #a :something else",
            ":me!u@h PRIVMSG #b :hi",
            ":me!u@h NOTICE #a :hi",
            ":alice!u@h PRIVMSG #a :hi",
        ] {
            assert!(
                echoes.observe(&line(other), "me", false).is_none(),
                "{other}"
            );
        }
        assert_eq!(
            describe(echoes.observe(&line("@msgid=e1 :Me!u@h PRIVMSG #a :hi"), "me", false)),
            ["1 ok = e1"]
        );
        assert_eq!(
            describe(echoes.observe(&line(":me!u@h NOTICE BOB :psst"), "me", false)),
            ["2 ok = "]
        );
    }

    #[test]
    fn pending_state_is_bounded_and_expires() {
        let mut echoes = Echoes::default();
        let start = now();
        for id in 0..MAX_PENDING as u64 {
            assert!(echoes.track(id, "#a", false, "x", false, true).is_some());
        }
        assert!(echoes.track(99, "#a", false, "x", false, true).is_none());
        assert_eq!(echoes.outstanding().len(), MAX_PENDING);
        let events = echoes.expire(start + PENDING_TTL + Duration::from_secs(1));
        assert_eq!(events.len(), MAX_PENDING);
        assert!(echoes.pending.is_empty());
        // Untracking a message that could not be sent.
        echoes.track(1, "#a", false, "x", false, true);
        echoes.untrack(1);
        assert!(echoes.pending.is_empty());
        echoes.track(2, "#a", false, "x", false, true);
        assert_eq!(echoes.reset().len(), 1);
        assert!(echoes.pending.is_empty());
    }

    #[test]
    fn a_self_message_is_confirmed_by_its_label_only() {
        let mut echoes = Echoes::default();
        let label = echoes
            .track(1, "me", false, "note", false, true)
            .unwrap()
            .unwrap();
        // The delivered copy has no label (the specification forbids it).
        assert!(
            echoes
                .observe(&line(":me!u@h PRIVMSG me :note"), "me", true)
                .is_none()
        );
        let echo = format!("@label={label} :me!u@h PRIVMSG me :note");
        assert_eq!(
            describe(echoes.observe(&line(&echo), "me", true)),
            ["1 ok = "]
        );
    }
}
