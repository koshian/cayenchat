//! Tells replayed history apart from live messages, so history does not
//! raise desktop notifications again.
//!
//! A server-time tag alone never marks history: an old timestamp is shown
//! as that time but notifies like any other line (D022).

use std::collections::HashSet;

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix};

use crate::tags::tag_value;
/// A misbehaving server could open batches without closing them.
const MAX_OPEN_BATCHES: usize = 64;
/// IRCv3 batch types that carry history rather than live traffic.
const HISTORY_BATCH_TYPES: [&str; 2] = ["CHATHISTORY", "ZNC.IN/PLAYBACK"];

/// Remembers open IRCv3 history batches (`chathistory`, `znc.in/playback`).
#[derive(Default)]
pub(crate) struct ReplayTracker {
    history_batches: HashSet<String>,
}

impl ReplayTracker {
    /// Follows `BATCH +ref type` / `BATCH -ref`. A batch nested in a history
    /// batch is history too.
    pub(crate) fn observe(&mut self, message: &IrcMessage) {
        let IrcCommand::BATCH(reference, kind, _) = &message.command else {
            return;
        };
        if let Some(reference) = reference.strip_prefix('-') {
            self.history_batches.remove(reference);
        } else if let Some(reference) = reference.strip_prefix('+') {
            let history = kind
                .as_ref()
                .is_some_and(|kind| HISTORY_BATCH_TYPES.contains(&kind.to_str()))
                || self.in_history_batch(message);
            if history && self.history_batches.len() < MAX_OPEN_BATCHES {
                self.history_batches.insert(reference.to_owned());
            }
        }
    }

    /// Whether a PRIVMSG or NOTICE is not a live message from a user: it is
    /// in a history batch, or was sent by the server or bouncer itself
    /// without a user mask. Tiarra's Log::Recent, for example, replays
    /// channel logs as NOTICEs from `:tiarra`.
    pub(crate) fn replayed(&self, message: &IrcMessage) -> bool {
        let from_user = matches!(
            &message.prefix,
            Some(Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()
        );
        !from_user || self.in_history_batch(message)
    }

    fn in_history_batch(&self, message: &IrcMessage) -> bool {
        tag_value(message, "batch")
            .is_some_and(|reference| self.history_batches.contains(reference))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> IrcMessage {
        line.parse().unwrap()
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
        let tagged = |tags: &str| parse(&format!("@{tags} :alice!u@h PRIVMSG #chan :me: hi"));
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
}
