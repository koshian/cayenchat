//! Tells replayed history apart from live messages, so history does not
//! raise desktop notifications again.
//!
//! A server-time tag alone never marks history: an old timestamp is shown
//! as that time but notifies like any other line (D022).
//!
//! History batches are followed whether or not `batch` was negotiated. A
//! server may only send batches to a client that asked for them, but this
//! tracker already recognized unsolicited `chathistory`/`znc.in/playback`
//! batches before the option existed, and keeps doing so for compatibility.
//! Negotiating `batch` (opt-in per server) is what makes servers and bouncers
//! actually send them; losing the capability clears what was being followed.

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix};

use crate::tags::tag_value;

/// Open history batches remembered at once. A server that never ends its
/// batches evicts its oldest ones instead of growing the table.
const MAX_OPEN_BATCHES: usize = 64;
/// Longest batch reference kept. References are short opaque identifiers;
/// a longer one is not followed, so its messages count as live.
const MAX_REFERENCE_BYTES: usize = 64;
/// Batch types that carry history rather than live traffic. irc-proto 1.1.0
/// upper-cases every batch type it parses (`BatchSubCommand::CUSTOM`), so
/// the raw spelling is lost and these are compared in upper case. Batch
/// references (`+ref`, `-ref`, the `batch` tag) keep their case and are
/// compared exactly, as the specification requires.
const HISTORY_BATCH_TYPES: [&str; 2] = ["CHATHISTORY", "ZNC.IN/PLAYBACK"];

/// Remembers the open batches that carry history: `chathistory` and
/// `znc.in/playback` batches and any batch opened inside one. Other batches
/// (netsplit, multiline, unknown vendor types) are not stored at all, so
/// their messages stay live; only history ancestry matters here, and it is
/// settled when a batch opens, so messages are classified one by one as they
/// arrive rather than buffered until a batch ends.
#[derive(Debug, Default)]
pub(crate) struct ReplayTracker {
    /// References of open history batches, oldest first.
    history_batches: Vec<String>,
}

impl ReplayTracker {
    /// Follows `BATCH +ref type` / `BATCH -ref`. A batch opened inside a
    /// history batch is history too, whatever its own type.
    pub(crate) fn observe(&mut self, message: &IrcMessage) {
        let IrcCommand::BATCH(reference, kind, _) = &message.command else {
            return;
        };
        if let Some(reference) = reference.strip_prefix('-') {
            self.forget(reference);
        } else if let Some(reference) = reference.strip_prefix('+') {
            if reference.is_empty() || reference.len() > MAX_REFERENCE_BYTES {
                return;
            }
            let history = kind
                .as_ref()
                .is_some_and(|kind| HISTORY_BATCH_TYPES.contains(&kind.to_str()))
                || self.in_history_batch(message);
            // A reused reference replaces the batch it named, so a stale
            // history batch cannot mute a later live one.
            self.forget(reference);
            if history {
                if self.history_batches.len() >= MAX_OPEN_BATCHES {
                    self.history_batches.remove(0);
                }
                self.history_batches.push(reference.to_owned());
            }
        }
    }

    /// Forgets every open batch: the capability was withdrawn. A new
    /// connection starts with a new tracker.
    pub(crate) fn reset(&mut self) {
        self.history_batches = Vec::new();
    }

    /// Whether a PRIVMSG or NOTICE is not a live message from a user: it is
    /// in a history batch, or was sent by the server or bouncer itself
    /// without a user mask. Tiarra's Log::Recent, for example, replays
    /// channel logs as NOTICEs from `:tiarra`. A message in an unknown or
    /// already ended batch is live.
    pub(crate) fn replayed(&self, message: &IrcMessage) -> bool {
        let from_user = matches!(
            &message.prefix,
            Some(Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()
        );
        !from_user || self.in_history_batch(message)
    }

    fn in_history_batch(&self, message: &IrcMessage) -> bool {
        tag_value(message, "batch")
            .is_some_and(|reference| self.history_batches.iter().any(|open| open == reference))
    }

    fn forget(&mut self, reference: &str) {
        self.history_batches.retain(|open| open != reference);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> IrcMessage {
        line.parse().unwrap()
    }

    fn tagged(tags: &str) -> IrcMessage {
        parse(&format!("@{tags} :alice!u@h PRIVMSG #chan :me: hi"))
    }

    #[test]
    fn server_and_bouncer_lines_without_a_user_mask_are_replayed() {
        let tracker = ReplayTracker::default();
        assert!(tracker.replayed(&parse(":tiarra NOTICE #chan :12:34 <alice> me: hi")));
        assert!(tracker.replayed(&parse(":irc.example NOTICE #chan :maintenance")));
        assert!(!tracker.replayed(&parse(":alice!u@h PRIVMSG #chan :me: hi")));
        assert!(!tracker.replayed(&parse(":alice!u@h NOTICE #chan :me: hi")));
    }

    #[test]
    fn history_batches_are_replayed_but_old_server_times_are_not() {
        let mut tracker = ReplayTracker::default();
        tracker.observe(&parse(":srv BATCH +h1 chathistory #chan"));
        tracker.observe(&parse(":srv BATCH +n1 netsplit a.example b.example"));
        tracker.observe(&parse("@batch=h1 :srv BATCH +h2 draft/multiline #chan"));
        tracker.observe(&parse(":znc BATCH +p1 znc.in/playback #chan"));
        assert!(tracker.replayed(&tagged("batch=h1")));
        assert!(tracker.replayed(&tagged("batch=h2")));
        assert!(tracker.replayed(&tagged("batch=p1")));
        assert!(!tracker.replayed(&tagged("batch=n1")));
        tracker.observe(&parse(":srv BATCH -h1"));
        assert!(!tracker.replayed(&tagged("batch=h1")));

        // Duplicate keys: the last occurrence decides.
        assert!(tracker.replayed(&tagged("batch=n1;batch=p1")));
        assert!(!tracker.replayed(&tagged("batch=p1;batch=")));

        // Timestamps, however old, do not make a live line history.
        assert!(!tracker.replayed(&tagged("time=2011-10-19T16:40:51.620Z")));
        assert!(!tracker.replayed(&tagged("time=2011-10-19T16:40:51.620Z;batch=n1")));
    }

    #[test]
    fn nesting_follows_history_ancestry_only() {
        let mut tracker = ReplayTracker::default();
        // History inside an unknown batch is history by its own type.
        tracker.observe(&parse(":srv BATCH +outer example.com/wrapper"));
        tracker.observe(&parse("@batch=outer :srv BATCH +hist chathistory #chan"));
        // Anything inside history is history, however deep.
        tracker.observe(&parse("@batch=hist :srv BATCH +inner example.com/unknown"));
        tracker.observe(&parse("@batch=inner :srv BATCH +deeper netsplit a b"));
        assert!(!tracker.replayed(&tagged("batch=outer")));
        for reference in ["hist", "inner", "deeper"] {
            assert!(
                tracker.replayed(&tagged(&format!("batch={reference}"))),
                "{reference}"
            );
        }
        // A child keeps its classification when the parent ends first.
        tracker.observe(&parse("@batch=outer :srv BATCH -hist"));
        assert!(!tracker.replayed(&tagged("batch=hist")));
        assert!(tracker.replayed(&tagged("batch=inner")));
        // A batch opened after the parent ended is not history by ancestry.
        tracker.observe(&parse("@batch=hist :srv BATCH +late example.com/unknown"));
        assert!(!tracker.replayed(&tagged("batch=late")));
    }

    #[test]
    fn concurrent_batches_and_live_lines_interleave() {
        let mut tracker = ReplayTracker::default();
        tracker.observe(&parse(":srv BATCH +a chathistory #one"));
        tracker.observe(&parse(":srv BATCH +b netsplit x y"));
        assert!(tracker.replayed(&tagged("batch=a")));
        // Live traffic between history lines stays live.
        assert!(!tracker.replayed(&parse(":bob!u@h PRIVMSG #one :me: live")));
        assert!(!tracker.replayed(&tagged("batch=b")));
        tracker.observe(&parse(":srv BATCH +c znc.in/playback #two"));
        tracker.observe(&parse(":srv BATCH -a"));
        assert!(!tracker.replayed(&tagged("batch=a")));
        assert!(tracker.replayed(&tagged("batch=c")));
        tracker.observe(&parse(":srv BATCH -b"));
        assert!(tracker.replayed(&tagged("batch=c")));
    }

    #[test]
    fn references_are_case_sensitive_and_types_are_not() {
        let mut tracker = ReplayTracker::default();
        tracker.observe(&parse(":srv BATCH +Ab1 CHATHISTORY #chan"));
        assert!(tracker.replayed(&tagged("batch=Ab1")));
        assert!(!tracker.replayed(&tagged("batch=ab1")));
        assert!(!tracker.replayed(&tagged("batch=AB1")));
        tracker.observe(&parse(":srv BATCH -ab1"));
        assert!(tracker.replayed(&tagged("batch=Ab1")));
        tracker.observe(&parse(":srv BATCH +z ZNC.in/Playback #chan"));
        assert!(tracker.replayed(&tagged("batch=z")));
    }

    #[test]
    fn unknown_duplicate_and_malformed_references_do_not_mute_live_lines() {
        let mut tracker = ReplayTracker::default();
        // Unknown types and references never mute.
        tracker.observe(&parse(":srv BATCH +u example.com/unknown"));
        assert!(!tracker.replayed(&tagged("batch=u")));
        assert!(!tracker.replayed(&tagged("batch=never-opened")));
        // Ending an unknown batch is ignored.
        tracker.observe(&parse(":srv BATCH -never-opened"));
        // Reusing an open history reference for a live batch unmutes it.
        tracker.observe(&parse(":srv BATCH +dup chathistory #chan"));
        assert!(tracker.replayed(&tagged("batch=dup")));
        tracker.observe(&parse(":srv BATCH +dup example.com/live"));
        assert!(!tracker.replayed(&tagged("batch=dup")));
        // Opening the same history reference twice keeps one entry.
        tracker.observe(&parse(":srv BATCH +dup chathistory #chan"));
        tracker.observe(&parse(":srv BATCH +dup chathistory #chan"));
        assert_eq!(tracker.history_batches, ["dup"]);
        // No type, an empty reference or an oversized one is not followed.
        tracker.observe(&parse(":srv BATCH +notype"));
        tracker.observe(&parse(":srv BATCH + chathistory #chan"));
        let long = "r".repeat(MAX_REFERENCE_BYTES + 1);
        tracker.observe(&parse(&format!(":srv BATCH +{long} chathistory #chan")));
        assert_eq!(tracker.history_batches, ["dup"]);
        let fits = "r".repeat(MAX_REFERENCE_BYTES);
        tracker.observe(&parse(&format!(":srv BATCH +{fits} chathistory #chan")));
        assert!(tracker.replayed(&tagged(&format!("batch={fits}"))));
    }

    #[test]
    fn missing_endings_evict_the_oldest_batch_within_the_limit() {
        let mut tracker = ReplayTracker::default();
        for index in 0..MAX_OPEN_BATCHES + 10 {
            tracker.observe(&parse(&format!(":srv BATCH +h{index} chathistory #chan")));
            // Unknown batches never take space.
            tracker.observe(&parse(&format!(":srv BATCH +u{index} netsplit a b")));
        }
        assert_eq!(tracker.history_batches.len(), MAX_OPEN_BATCHES);
        assert!(!tracker.replayed(&tagged("batch=h0")));
        assert!(!tracker.replayed(&tagged("batch=h9")));
        assert!(tracker.replayed(&tagged("batch=h10")));
        let newest = format!("batch=h{}", MAX_OPEN_BATCHES + 9);
        assert!(tracker.replayed(&tagged(&newest)));
        // A full table still follows nesting for the newest batches.
        tracker.observe(&parse(&format!(
            "@batch=h{} :srv BATCH +child example.com/x",
            MAX_OPEN_BATCHES + 9
        )));
        assert!(tracker.replayed(&tagged("batch=child")));
        assert_eq!(tracker.history_batches.len(), MAX_OPEN_BATCHES);

        tracker.reset();
        assert!(tracker.history_batches.is_empty());
        assert!(!tracker.replayed(&tagged("batch=child")));
        // Lines without a user mask stay replayed after a reset.
        assert!(tracker.replayed(&parse(":tiarra NOTICE #chan :<alice> hi")));
    }
}
