// SPDX-License-Identifier: MPL-2.0
//! The connection diagnostics of each server: the lines the View menu shows
//! and copies, kept apart from the conversation logs (no log file, unread
//! count, notification or combined log) and bounded per server and view.

use std::{
    collections::VecDeque,
    time::{Duration, SystemTime},
};

/// Newest lines kept per server and view.
pub const DIAGNOSTIC_LIMIT: usize = 1000;

/// The first diagnostic sequence. Messages count from `SEQUENCE_ORIGIN`
/// (2^62) up and older history from there down, so 0 up is never reached by
/// either: a sequence names one line of the messages or one of the
/// diagnostics, and a search orders the diagnostics before the messages, so
/// its first stop is the newest message, as the screen's order reads.
pub(crate) const FIRST_SEQUENCE: u64 = 0;

/// Which connection log the View menu shows and copies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticsView {
    /// Connection stages and notes.
    #[default]
    Overview,
    /// The decoded IRC lines, with their direction.
    Protocol,
}

/// One connection log line: when it happened, the time since the attempt
/// began, and its text.
#[derive(Clone, Debug)]
pub struct DiagnosticLine {
    /// Identity and order key from the diagnostics' own counter, which is
    /// not the message counter; never reused.
    pub sequence: u64,
    pub at: SystemTime,
    pub elapsed: Option<Duration>,
    pub text: String,
}

impl DiagnosticLine {
    /// Where the line stands in the order of all diagnostic lines ever
    /// recorded: a row number for a selection that survives trimming.
    pub fn position(&self) -> usize {
        (self.sequence - FIRST_SEQUENCE) as usize
    }

    /// The line as drawn and copied: local date and time with milliseconds
    /// and UTC offset, then the elapsed time.
    pub fn render(&self) -> String {
        let at =
            chrono::DateTime::<chrono::Local>::from(self.at).format("%Y-%m-%d %H:%M:%S%.3f %:z");
        match self.elapsed {
            Some(elapsed) => format!("{at} [{:.1}s] {}", elapsed.as_secs_f32(), self.text),
            None => format!("{at} {}", self.text),
        }
    }
}

/// The two views' lines of one server, oldest first.
#[derive(Default)]
pub(crate) struct ServerDiagnostics {
    overview: VecDeque<DiagnosticLine>,
    protocol: VecDeque<DiagnosticLine>,
}

impl ServerDiagnostics {
    pub(crate) fn lines(&self, view: DiagnosticsView) -> &VecDeque<DiagnosticLine> {
        match view {
            DiagnosticsView::Overview => &self.overview,
            DiagnosticsView::Protocol => &self.protocol,
        }
    }

    /// Adds a line to one view; drops the oldest without shifting.
    pub(crate) fn push(&mut self, view: DiagnosticsView, line: DiagnosticLine) {
        let lines = match view {
            DiagnosticsView::Overview => &mut self.overview,
            DiagnosticsView::Protocol => &mut self.protocol,
        };
        lines.push_back(line);
        if lines.len() > DIAGNOSTIC_LIMIT {
            lines.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppState, NetworkConfig};
    use cayenchat_model::NetworkId;

    fn state() -> AppState {
        AppState::with_networks(vec![
            NetworkConfig {
                id: NetworkId(1),
                name: "a".into(),
                channels: Vec::new(),
            },
            NetworkConfig {
                id: NetworkId(2),
                name: "b".into(),
                channels: Vec::new(),
            },
        ])
    }

    fn record(state: &mut AppState, network: u32, view: DiagnosticsView, text: &str, secs: u64) {
        state.record_diagnostic(
            NetworkId(network),
            view,
            SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            Some(Duration::from_millis(1_250)),
            text.into(),
        );
    }

    #[test]
    fn a_line_carries_its_own_recorded_time_and_the_elapsed_time() {
        let mut state = state();
        record(
            &mut state,
            1,
            DiagnosticsView::Protocol,
            "← :s 001 me :hi",
            1_000_000_000,
        );
        record(
            &mut state,
            1,
            DiagnosticsView::Protocol,
            "← :s 001 me :hi",
            1_000_086_400,
        );
        let lines = state.diagnostics(NetworkId(1), DiagnosticsView::Protocol);
        let early = lines[0].render();
        // Date, time with milliseconds and offset, then the elapsed time.
        assert!(shape(&early), "{early}");
        assert!(early.ends_with(" [1.2s] ← :s 001 me :hi"), "{early}");
        // The time is the recorded one: a day later changes the line.
        assert_ne!(early, lines[1].render());
    }

    /// `YYYY-MM-DD HH:MM:SS.mmm ±HH:MM ` at the start.
    fn shape(text: &str) -> bool {
        let bytes = text.as_bytes();
        let digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22];
        bytes.len() > 30
            && digits.iter().all(|&i| bytes[i].is_ascii_digit())
            && [bytes[4], bytes[7], bytes[13], bytes[16], bytes[19]] == *b"--::."
            && bytes[10] == b' '
            && matches!(bytes[24], b'+' | b'-')
    }

    #[test]
    fn views_are_kept_apart_bounded_and_cleared_together() {
        let mut state = state();
        record(&mut state, 1, DiagnosticsView::Overview, "note", 1);
        for n in 0..DIAGNOSTIC_LIMIT + 5 {
            record(
                &mut state,
                1,
                DiagnosticsView::Protocol,
                &format!("→ {n}"),
                1,
            );
        }
        assert_eq!(
            state
                .diagnostics(NetworkId(1), DiagnosticsView::Overview)
                .len(),
            1
        );
        let wire = state.diagnostics(NetworkId(1), DiagnosticsView::Protocol);
        assert_eq!(wire.len(), DIAGNOSTIC_LIMIT);
        assert_eq!(wire[0].text, "→ 5");
        // Sequences only grow and are never reused after trimming.
        assert!(
            wire.iter()
                .zip(wire.iter().skip(1))
                .all(|(a, b)| a.sequence < b.sequence)
        );
        state.clear_diagnostics(NetworkId(1));
        assert!(
            state
                .diagnostics(NetworkId(1), DiagnosticsView::Overview)
                .is_empty()
        );
        assert!(
            state
                .diagnostics(NetworkId(1), DiagnosticsView::Protocol)
                .is_empty()
        );
    }

    #[test]
    fn a_line_leaves_the_message_change_detectors_alone() {
        let mut state = state();
        let last = state.last_message_sequence();
        let latest = state.latest_sequence();
        let log = state.log_revision();
        let all = crate::search::revision(&state, crate::search::Scope::All, None);
        record(&mut state, 1, DiagnosticsView::Protocol, "→ PING", 1);
        record(&mut state, 1, DiagnosticsView::Overview, "note", 1);
        assert_eq!(state.last_message_sequence(), last);
        assert_eq!(state.latest_sequence(), latest);
        assert_eq!(state.log_revision(), log);
        assert_eq!(
            crate::search::revision(&state, crate::search::Scope::All, None),
            all
        );
    }

    #[test]
    fn servers_are_kept_apart_and_never_reach_the_conversation_logs() {
        let mut state = state();
        record(&mut state, 1, DiagnosticsView::Overview, "one", 1);
        record(&mut state, 9, DiagnosticsView::Overview, "unknown", 1);
        assert!(
            state
                .diagnostics(NetworkId(2), DiagnosticsView::Overview)
                .is_empty()
        );
        assert!(
            state
                .diagnostics(NetworkId(9), DiagnosticsView::Overview)
                .is_empty()
        );
        assert!(state.server_messages(NetworkId(1)).is_empty());
        assert!(state.system_messages().is_empty());
        state.sync_networks(&[NetworkConfig {
            id: NetworkId(2),
            name: "b".into(),
            channels: Vec::new(),
        }]);
        assert!(
            state
                .diagnostics(NetworkId(1), DiagnosticsView::Overview)
                .is_empty()
        );
    }
}
