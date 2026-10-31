// SPDX-License-Identifier: MPL-2.0
//! Incremental search over the logs the application holds in memory.
//!
//! Nothing here reads files or asks a server: it walks the retained messages
//! of [`AppState`]. A search is a [`Scan`] that the caller advances in slices
//! of a bounded number of messages, so a large log set never blocks the UI
//! thread. Hits name a log and a [`Message::sequence`], never a row index, so
//! lines that arrive, are trimmed or are inserted above cannot make a hit
//! point at the wrong line.

use std::{borrow::Cow, ops::Range};

use cayenchat_model::{ConversationId, Message, NetworkId};

use crate::{AppState, DiagnosticsView, Selection};

/// The most hits one scan keeps. Past it the scan stops and says so.
pub const MAX_HITS: usize = 10_000;

/// A log that can be searched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Conversation(ConversationId),
    /// A server's own log.
    Server(NetworkId),
    /// A server's connection diagnostics in one view, searched as drawn
    /// (`DiagnosticLine::text`). Their sequences come from a counter that
    /// never meets the messages', so a sequence names one line in any hit.
    Diagnostics(NetworkId, DiagnosticsView),
}

/// One matching line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub source: Source,
    pub sequence: u64,
}

/// What a search covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The selected conversation, or the selected server's log.
    Selected,
    /// Every conversation and server log in memory.
    All,
}

/// A search term, case-folded once. Matching is a plain case-insensitive
/// substring match (Unicode lowercase, no normalization, no patterns). Text
/// is read once, front to back (Knuth-Morris-Pratt), so a long line of
/// repeated characters costs no more than any other line of that length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    folded: Vec<char>,
    /// For each prefix of `folded`, the length of its longest proper prefix
    /// that is also a suffix.
    fallback: Vec<usize>,
}

impl Query {
    /// `None` for an empty term, which searches nothing.
    pub fn new(term: &str) -> Option<Self> {
        let folded: Vec<char> = term.chars().flat_map(char::to_lowercase).collect();
        if folded.is_empty() {
            return None;
        }
        let mut fallback = vec![0; folded.len()];
        let mut matched = 0;
        for index in 1..folded.len() {
            while matched > 0 && folded[index] != folded[matched] {
                matched = fallback[matched - 1];
            }
            if folded[index] == folded[matched] {
                matched += 1;
            }
            fallback[index] = matched;
        }
        Some(Self { folded, fallback })
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.find_from(text, 0).is_some()
    }

    /// The non-overlapping matches in `text`, as byte ranges of `text`.
    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(range) = self.find_from(text, from) {
            from = range.end;
            found.push(range);
        }
        found
    }

    /// The first match that starts at or after byte `from`. A match covers
    /// whole characters: one that starts or ends inside the lowercase form of
    /// a character (`\u{130}` lowercases to two) does not count.
    fn find_from(&self, text: &str, from: usize) -> Option<Range<usize>> {
        let wanted = self.folded.len();
        let text_tail = &text[from..];
        // A character lowercases to at most three characters.
        if text_tail.len().saturating_mul(3) < wanted {
            return None;
        }
        // For each of the last `wanted` folded characters: the byte offset
        // of the character it came from, and whether it is that character's
        // first folded character. Bounded by the term, so no match needs a
        // backward walk to find where it starts.
        let mut origins = vec![(0usize, false); wanted];
        let mut count = 0usize;
        let mut matched = 0;
        for (offset, ch) in text_tail.char_indices() {
            let mut lower = ch.to_lowercase().peekable();
            let mut first = true;
            while let Some(folded) = lower.next() {
                origins[count % wanted] = (from + offset, first);
                first = false;
                count += 1;
                while matched > 0 && (matched == wanted || self.folded[matched] != folded) {
                    matched = self.fallback[matched - 1];
                }
                if self.folded[matched] == folded {
                    matched += 1;
                }
                if matched == wanted && lower.peek().is_none() {
                    // The oldest entry is where the match began.
                    let (start, on_boundary) = origins[count % wanted];
                    if on_boundary {
                        return Some(start..from + offset + ch.len_utf8());
                    }
                }
            }
        }
        None
    }
}

/// The retained messages of `source`; none when it is gone, and none for
/// diagnostics, which are not messages.
pub fn messages(state: &AppState, source: Source) -> &[Message] {
    match source {
        Source::Conversation(id) => state
            .conversation(id)
            .map_or(&[][..], |conversation| &conversation.messages),
        Source::Server(network) => state.server_messages(network),
        Source::Diagnostics(..) => &[],
    }
}

/// Whether the log still holds the line `hit` names.
pub fn hit_exists(state: &AppState, hit: Hit) -> bool {
    match hit.source {
        Source::Diagnostics(network, view) => state
            .diagnostics(network, view)
            .binary_search_by_key(&hit.sequence, |line| line.sequence)
            .is_ok(),
        _ => hit_message(state, hit).is_some(),
    }
}

/// The text of the line `hit` names, while the log still holds it.
pub fn hit_message(state: &AppState, hit: Hit) -> Option<&Message> {
    let messages = messages(state, hit.source);
    let index = messages
        .binary_search_by_key(&hit.sequence, |message| message.sequence)
        .ok()?;
    Some(&messages[index])
}

/// The diagnostics a search of `scope` reads. `diagnostics` is the view the
/// selected server shows, or `None` while the connection diagnostics are off
/// (nothing is drawn, so nothing is searched). The selected scope reads the
/// selected server's diagnostics in that view; the whole scope reads every
/// server's, in both views.
fn diagnostic_sources(
    state: &AppState,
    scope: Scope,
    diagnostics: Option<DiagnosticsView>,
) -> Vec<Source> {
    let Some(shown) = diagnostics else {
        return Vec::new();
    };
    match scope {
        Scope::Selected => match state.selection() {
            Selection::Server(network) => vec![Source::Diagnostics(network, shown)],
            _ => Vec::new(),
        },
        Scope::All => state
            .networks()
            .iter()
            .flat_map(|network| {
                [DiagnosticsView::Overview, DiagnosticsView::Protocol]
                    .map(|view| Source::Diagnostics(network.id, view))
            })
            .collect(),
    }
}

/// What a search of a scope looked at: see [`revision`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    messages: (u64, u64, usize),
    /// Newest diagnostic sequence and the number of diagnostic lines.
    diagnostics: (u64, usize),
}

/// A cheap value that changes whenever a log `scope` covers gained or lost
/// lines, to tell a finished search that it is out of date. Lines in other
/// logs do not change it for [`Scope::Selected`]. `diagnostics` is as for
/// [`Scan::new`]; the diagnostic lines are only counted when searched, so
/// they never make a search of the messages stale.
pub fn revision(state: &AppState, scope: Scope, diagnostics: Option<DiagnosticsView>) -> Revision {
    let mut diagnostic_lines = (0, 0);
    for source in diagnostic_sources(state, scope, diagnostics) {
        if let Source::Diagnostics(network, view) = source {
            let lines = state.diagnostics(network, view);
            diagnostic_lines.0 = diagnostic_lines
                .0
                .max(lines.back().map_or(0, |line| line.sequence));
            diagnostic_lines.1 += lines.len();
        }
    }
    let messages = if scope == Scope::Selected {
        let source = match state.selection() {
            Selection::Channel(id) => Some(Source::Conversation(id)),
            Selection::Server(network) => Some(Source::Server(network)),
            Selection::None => None,
        };
        let messages = source.map_or(&[][..], |source| messages(state, source));
        (
            messages.first().map_or(0, |message| message.sequence),
            messages.last().map_or(0, |message| message.sequence),
            messages.len(),
        )
    } else {
        let conversations: usize = state
            .conversations()
            .iter()
            .map(|conversation| conversation.messages.len())
            .sum();
        let servers: usize = state
            .networks()
            .iter()
            .map(|network| state.server_messages(network.id).len())
            .sum();
        (0, state.latest_sequence(), conversations + servers)
    };
    Revision {
        messages,
        diagnostics: diagnostic_lines,
    }
}

/// What reading `text` costs against a step's budget: one unit, plus one
/// per 64 bytes of text.
fn text_cost(text: &str) -> usize {
    1 + text.len() / 64
}

/// A search in progress. Advance it with [`Scan::step`] until it is done;
/// [`Scan::hits`] may be read at any time and ascends by sequence.
pub struct Scan {
    query: Query,
    sources: Vec<Source>,
    /// The source being read, and the first sequence of it not read yet.
    cursor: usize,
    next_sequence: u64,
    hits: Vec<Hit>,
    truncated: bool,
}

impl Scan {
    /// `diagnostics`: the view of the connection diagnostics the window
    /// shows, or `None` while they are off, in which case they are not read.
    pub fn new(
        state: &AppState,
        scope: Scope,
        diagnostics: Option<DiagnosticsView>,
        query: Query,
    ) -> Self {
        let mut sources = match scope {
            Scope::Selected => match state.selection() {
                Selection::Channel(id) => vec![Source::Conversation(id)],
                Selection::Server(network) => vec![Source::Server(network)],
                Selection::None => Vec::new(),
            },
            Scope::All => state
                .conversations()
                .iter()
                .map(|conversation| Source::Conversation(conversation.id))
                .chain(
                    state
                        .networks()
                        .iter()
                        .map(|network| Source::Server(network.id)),
                )
                .collect(),
        };
        sources.extend(diagnostic_sources(state, scope, diagnostics));
        Self {
            query,
            sources,
            cursor: 0,
            next_sequence: 0,
            hits: Vec::new(),
            truncated: false,
        }
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn is_done(&self) -> bool {
        self.cursor >= self.sources.len()
    }

    /// More than [`MAX_HITS`] lines match; the oldest are not listed.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Reads `lines` (sequence and text) of `source` while `budget` lasts;
    /// returns whether the source was read to its end.
    fn read<'a>(
        &mut self,
        source: Source,
        lines: impl Iterator<Item = (u64, Cow<'a, str>)>,
        budget: &mut usize,
    ) -> bool {
        for (sequence, text) in lines {
            if *budget == 0 {
                self.next_sequence = sequence;
                return false;
            }
            *budget = budget.saturating_sub(text_cost(&text));
            if self.query.is_match(&text) {
                self.hits.push(Hit { source, sequence });
            }
        }
        true
    }

    /// Reads more lines until `budget` is spent: a line costs
    /// [`text_cost`], so long lines count for more than short ones. Returns
    /// whether the scan is done. A log that changed since the last step is
    /// read from where it left off by sequence, so nothing is read twice or
    /// skipped.
    pub fn step(&mut self, state: &AppState, mut budget: usize) -> bool {
        let before = self.hits.len();
        while self.cursor < self.sources.len() && budget > 0 {
            let source = self.sources[self.cursor];
            let finished_source = match source {
                Source::Diagnostics(network, view) => {
                    let lines = state.diagnostics(network, view);
                    let start = lines.partition_point(|line| line.sequence < self.next_sequence);
                    self.read(
                        source,
                        lines
                            .iter()
                            .skip(start)
                            .map(|line| (line.sequence, Cow::Borrowed(line.text.as_str()))),
                        &mut budget,
                    )
                }
                _ => {
                    let messages = messages(state, source);
                    let start =
                        messages.partition_point(|message| message.sequence < self.next_sequence);
                    self.read(
                        source,
                        messages[start..]
                            .iter()
                            .map(|message| (message.sequence, Cow::Borrowed(&*message.text))),
                        &mut budget,
                    )
                }
            };
            if finished_source {
                self.cursor += 1;
                self.next_sequence = 0;
            }
        }
        if self.sources.len() > 1 && self.hits.len() != before {
            self.hits.sort_unstable_by_key(|hit| hit.sequence);
        }
        // Keep the newest MAX_HITS: the oldest matches go, once per step.
        if self.hits.len() > MAX_HITS {
            self.hits.drain(..self.hits.len() - MAX_HITS);
            self.truncated = true;
        }
        self.is_done()
    }
}

/// Where `current` stands among `hits`, counting from 1.
pub fn position(hits: &[Hit], current: Hit) -> Option<usize> {
    hits.binary_search_by_key(&current.sequence, |hit| hit.sequence)
        .ok()
        .map(|index| index + 1)
}

/// The hit after `current` in log order, wrapping to the first. Without a
/// current hit, the first.
pub fn next_hit(hits: &[Hit], current: Option<Hit>) -> Option<Hit> {
    let after = match current {
        Some(current) => hits.partition_point(|hit| hit.sequence <= current.sequence),
        None => 0,
    };
    hits.get(after).or(hits.first()).copied()
}

/// The hit before `current`, wrapping to the last. Without a current hit,
/// the last.
pub fn previous_hit(hits: &[Hit], current: Option<Hit>) -> Option<Hit> {
    let before = match current {
        Some(current) => hits.partition_point(|hit| hit.sequence < current.sequence),
        None => hits.len(),
    };
    before
        .checked_sub(1)
        .and_then(|index| hits.get(index))
        .or(hits.last())
        .copied()
}

/// The hit to show when a new set of hits arrives: the nearest one at or
/// after where the previous search stood, else the newest.
pub fn initial_hit(hits: &[Hit], previous: Option<Hit>) -> Option<Hit> {
    let index =
        previous.map(|previous| hits.partition_point(|hit| hit.sequence < previous.sequence));
    index
        .and_then(|index| hits.get(index))
        .or(hits.last())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NetworkConfig, Selection};

    fn hit(sequence: u64) -> Hit {
        Hit {
            source: Source::Server(NetworkId(0)),
            sequence,
        }
    }

    #[test]
    fn matching_ignores_case_and_returns_every_occurrence_in_original_bytes() {
        let query = Query::new("ab").unwrap();
        assert_eq!(query.ranges("xAbaB ab"), vec![1..3, 3..5, 6..8]);
        assert!(!query.is_match("a b"));
        assert!(Query::new("").is_none());
    }

    #[test]
    fn matching_handles_japanese_and_multibyte_case_folding() {
        let query = Query::new("こんにちは").unwrap();
        assert_eq!(query.ranges("あ、こんにちは世界"), vec![6..21]);
        let query = Query::new("ÄÖ").unwrap();
        assert_eq!(query.ranges("xäöy"), vec![1..5]);
        // A character whose lowercase form is longer than the term's rest
        // is not split.
        let query = Query::new("i").unwrap();
        assert!(!query.is_match("\u{130}"));
        assert!(Query::new("i\u{307}").unwrap().is_match("\u{130}"));
    }

    #[test]
    fn matching_a_long_line_with_a_term_longer_than_the_line_is_safe() {
        let query = Query::new(&"a".repeat(50)).unwrap();
        assert!(!query.is_match(&"a".repeat(49)));
        assert!(query.is_match(&"a".repeat(50)));
        assert!(!query.is_match(""));
    }

    #[test]
    fn matching_is_linear_when_candidates_start_inside_a_folded_character() {
        // `İ` lowercases to `i` + U+0307. A term opening with U+0307 reaches
        // its end again and again but never begins on a character boundary.
        let text = "İ".repeat(8_000);
        let term = format!("\u{307}{}", "i\u{307}".repeat(512));
        let query = Query::new(&term).unwrap();
        let start = std::time::Instant::now();
        assert!(!query.is_match(&text));
        assert!(query.ranges(&text).is_empty());
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
        // Whole characters still match.
        let query = Query::new("i\u{307}i\u{307}").unwrap();
        assert_eq!(query.ranges("xİİx"), vec![1..5]);
        assert!(Query::new("\u{307}i").unwrap().ranges("İİ").is_empty());
    }

    #[test]
    fn matching_is_linear_on_repeated_text_and_finds_overlapping_prefixes() {
        // Restarting from every position would take minutes here.
        let text = "a".repeat(200_000);
        let query = Query::new(&format!("{}b", "a".repeat(2_000))).unwrap();
        let start = std::time::Instant::now();
        assert!(!query.is_match(&text));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let query = Query::new("aab").unwrap();
        assert_eq!(query.ranges("aaaab aab"), vec![2..5, 6..9]);
        let query = Query::new("abab").unwrap();
        assert_eq!(query.ranges("ababab"), vec![0..4]);
        assert_eq!(query.ranges("xababab abab"), vec![1..5, 8..12]);
    }

    #[test]
    fn a_scan_step_costs_more_for_long_lines() {
        let (mut state, a, _) = state_with_two_channels();
        for _ in 0..4 {
            state.append_conversation_activity(a, format!("{} needle", "x".repeat(640)));
        }
        let mut scan = Scan::new(&state, Scope::All, None, Query::new("needle").unwrap());
        // Each line costs 11 units, so a budget of 15 reads two per step.
        assert!(!scan.step(&state, 15));
        assert_eq!(scan.hits().len(), 2);
        while !scan.step(&state, 15) {}
        assert_eq!(scan.hits().len(), 4);
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let hits = [hit(2), hit(5), hit(9)];
        assert_eq!(next_hit(&hits, None), Some(hit(2)));
        assert_eq!(next_hit(&hits, Some(hit(2))), Some(hit(5)));
        assert_eq!(next_hit(&hits, Some(hit(9))), Some(hit(2)));
        // A current line that is no longer a hit still has a neighbour.
        assert_eq!(next_hit(&hits, Some(hit(6))), Some(hit(9)));
        assert_eq!(previous_hit(&hits, None), Some(hit(9)));
        assert_eq!(previous_hit(&hits, Some(hit(5))), Some(hit(2)));
        assert_eq!(previous_hit(&hits, Some(hit(2))), Some(hit(9)));
        assert_eq!(previous_hit(&hits, Some(hit(6))), Some(hit(5)));
        assert_eq!(next_hit(&[], None), None);
        assert_eq!(previous_hit(&[], Some(hit(1))), None);
        assert_eq!(position(&hits, hit(5)), Some(2));
        assert_eq!(position(&hits, hit(6)), None);
        assert_eq!(initial_hit(&hits, None), Some(hit(9)));
        assert_eq!(initial_hit(&hits, Some(hit(6))), Some(hit(9)));
        assert_eq!(initial_hit(&hits, Some(hit(5))), Some(hit(5)));
        assert_eq!(initial_hit(&hits, Some(hit(10))), Some(hit(9)));
    }

    fn state_with_two_channels() -> (AppState, ConversationId, ConversationId) {
        let state = AppState::with_networks(vec![
            NetworkConfig {
                id: NetworkId(1),
                name: "one".into(),
                channels: vec!["#a".into()],
            },
            NetworkConfig {
                id: NetworkId(2),
                name: "two".into(),
                channels: vec!["#b".into()],
            },
        ]);
        let networks: Vec<_> = state.networks().iter().map(|n| n.id).collect();
        let a = state.channel_id(networks[0], "#a").unwrap();
        let b = state.channel_id(networks[1], "#b").unwrap();
        (state, a, b)
    }

    fn run(state: &AppState, scope: Scope, term: &str, budget: usize) -> Vec<Hit> {
        let mut scan = Scan::new(state, scope, None, Query::new(term).unwrap());
        while !scan.step(state, budget) {}
        scan.hits().to_vec()
    }

    #[test]
    fn all_scope_spans_conversations_and_server_logs_in_log_order() {
        let (mut state, a, b) = state_with_two_channels();
        let network = state.networks()[0].id;
        state.append_conversation_activity(a, "needle one".into());
        state.append_conversation_activity(b, "Needle two".into());
        state.append_server_message(network, "a NEEDLE here".into());
        state.append_conversation_activity(a, "nothing".into());
        state.append_conversation_activity(a, "needle three".into());

        let hits = run(&state, Scope::All, "needle", 2);
        assert_eq!(hits.len(), 4);
        assert!(hits.windows(2).all(|w| w[0].sequence < w[1].sequence));
        assert_eq!(hits[0].source, Source::Conversation(a));
        assert_eq!(hits[1].source, Source::Conversation(b));
        assert_eq!(hits[2].source, Source::Server(network));
        assert_eq!(hits, run(&state, Scope::All, "needle", 1_000));
        for found in &hits {
            assert!(hit_message(&state, *found).is_some());
        }
    }

    #[test]
    fn selected_scope_covers_only_the_selected_log() {
        let (mut state, a, b) = state_with_two_channels();
        state.append_conversation_activity(a, "needle one".into());
        state.append_conversation_activity(b, "needle two".into());
        state.dispatch(crate::Command::SelectChannel(b));
        let hits = run(&state, Scope::Selected, "needle", 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, Source::Conversation(b));
        // The server log is a separate selection.
        let network = state.networks()[1].id;
        state.append_server_message(network, "needle on the server".into());
        state.dispatch(crate::Command::SelectServer(network));
        let hits = run(&state, Scope::Selected, "needle", 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, Source::Server(network));
        assert_eq!(state.selection(), Selection::Server(network));
    }

    #[test]
    fn a_scan_survives_lines_added_and_logs_removed_between_slices() {
        let (mut state, a, _) = state_with_two_channels();
        for n in 0..10 {
            state.append_conversation_activity(a, format!("needle {n}"));
        }
        let mut scan = Scan::new(&state, Scope::All, None, Query::new("needle").unwrap());
        assert!(!scan.step(&state, 4));
        assert_eq!(scan.hits().len(), 4);
        // New lines land after the cursor; nothing is read twice.
        state.append_conversation_activity(a, "needle new".into());
        while !scan.step(&state, 4) {}
        assert_eq!(scan.hits().len(), 11);
        let mut unique: Vec<_> = scan.hits().iter().map(|h| h.sequence).collect();
        unique.dedup();
        assert_eq!(unique.len(), 11);
        // A log that disappeared mid-scan is skipped, not a panic.
        let mut scan = Scan::new(&state, Scope::All, None, Query::new("needle").unwrap());
        let (mut other, _, _) = state_with_two_channels();
        other.append_conversation_activity(a, "x".into());
        while !scan.step(&other, 3) {}
    }

    #[test]
    fn the_hit_list_is_bounded() {
        let channels: Vec<String> = (0..7).map(|n| format!("#c{n}")).collect();
        let mut state = AppState::with_networks(vec![NetworkConfig {
            id: NetworkId(1),
            name: "one".into(),
            channels: channels.clone(),
        }]);
        for name in &channels {
            let id = state.channel_id(NetworkId(1), name).unwrap();
            // Each log keeps 2,000 lines, so the channels together pass the cap.
            for _ in 0..2_000 {
                state.append_conversation_activity(id, "needle".into());
            }
        }
        let all = {
            let mut scan = Scan::new(&state, Scope::All, None, Query::new("needle").unwrap());
            scan.step(&state, 1_000_000);
            scan
        };
        assert!(all.is_truncated());
        assert_eq!(all.hits().len(), MAX_HITS);
        // The newest MAX_HITS of all matches stay.
        let mut every: Vec<u64> = channels
            .iter()
            .flat_map(|name| {
                let id = state.channel_id(NetworkId(1), name).unwrap();
                messages(&state, Source::Conversation(id))
                    .iter()
                    .map(|m| m.sequence)
                    .collect::<Vec<_>>()
            })
            .collect();
        every.sort_unstable();
        let kept: Vec<u64> = all.hits().iter().map(|h| h.sequence).collect();
        assert_eq!(kept, every[every.len() - MAX_HITS..]);
        // Small slices give the same list.
        let mut sliced = Scan::new(&state, Scope::All, None, Query::new("needle").unwrap());
        while !sliced.step(&state, 777) {}
        assert!(sliced.is_truncated());
        assert_eq!(sliced.hits(), all.hits());
    }

    #[test]
    fn hits_under_the_cap_are_all_kept() {
        let mut state = AppState::with_networks(vec![NetworkConfig {
            id: NetworkId(1),
            name: "one".into(),
            channels: vec!["#c".into()],
        }]);
        let id = state.channel_id(NetworkId(1), "#c").unwrap();
        for _ in 0..2_000 {
            state.append_conversation_activity(id, "needle".into());
        }
        let mut scan = Scan::new(&state, Scope::All, None, Query::new("needle").unwrap());
        while !scan.step(&state, 100) {}
        assert!(!scan.is_truncated());
        assert_eq!(scan.hits().len(), 2_000);
    }

    #[test]
    fn revision_changes_with_added_lines() {
        let (mut state, a, _) = state_with_two_channels();
        state.dispatch(crate::Command::SelectChannel(a));
        let all = revision(&state, Scope::All, None);
        let selected = revision(&state, Scope::Selected, None);
        state.append_conversation_activity(a, "x".into());
        assert_ne!(revision(&state, Scope::All, None), all);
        assert_ne!(revision(&state, Scope::Selected, None), selected);
        // Another log's lines leave a search of the selected one alone.
        let (_, b) = (a, state.conversations()[1].id);
        let selected = revision(&state, Scope::Selected, None);
        state.append_conversation_activity(b, "y".into());
        assert_eq!(revision(&state, Scope::Selected, None), selected);
    }

    fn note(state: &mut AppState, network: NetworkId, view: DiagnosticsView, text: &str) {
        state.record_diagnostic(
            network,
            view,
            std::time::SystemTime::UNIX_EPOCH,
            None,
            text.into(),
        );
    }

    fn scan(
        state: &AppState,
        scope: Scope,
        shown: Option<DiagnosticsView>,
        term: &str,
    ) -> Vec<Hit> {
        let mut scan = Scan::new(state, scope, shown, Query::new(term).unwrap());
        while !scan.step(state, 7) {}
        scan.hits().to_vec()
    }

    #[test]
    fn diagnostics_are_searched_by_scope_and_only_while_shown() {
        use DiagnosticsView::{Overview, Protocol};
        let (mut state, _, _) = state_with_two_channels();
        let (one, two) = (state.networks()[0].id, state.networks()[1].id);
        state.append_server_message(one, "Sleep notice in the server log".into());
        note(&mut state, one, Overview, "Sleep notice: idle");
        note(&mut state, one, Protocol, "← :s NOTICE * :Sleep notice");
        note(&mut state, two, Overview, "Sleep notice on two");
        state.dispatch(crate::Command::SelectServer(one));

        // Off: no diagnostic is read.
        let hits = scan(&state, Scope::All, None, "sleep notice");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, Source::Server(one));

        // This log: the selected server's log and the view shown.
        let hits = scan(&state, Scope::Selected, Some(Overview), "sleep notice");
        let sources: Vec<_> = hits.iter().map(|hit| hit.source).collect();
        assert_eq!(
            sources,
            [Source::Diagnostics(one, Overview), Source::Server(one)]
        );
        // The diagnostics sort before the messages, so a search starts at the
        // newest message, not at a diagnostic line.
        assert_eq!(
            initial_hit(&hits, None).unwrap().source,
            Source::Server(one)
        );
        let hits = scan(&state, Scope::Selected, Some(Protocol), "sleep notice");
        assert_eq!(hits[0].source, Source::Diagnostics(one, Protocol));

        // All logs: every server, both views, in log order.
        let hits = scan(&state, Scope::All, Some(Overview), "sleep notice");
        assert_eq!(hits.len(), 4);
        assert!(
            hits.windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert!(hits.iter().all(|hit| hit_exists(&state, *hit)));

        // A channel's own search leaves the server's diagnostics out.
        let channel = state.conversations()[0].id;
        state.dispatch(crate::Command::SelectChannel(channel));
        assert!(scan(&state, Scope::Selected, Some(Overview), "sleep").is_empty());
    }

    #[test]
    fn diagnostics_sort_before_messages_regardless_of_record_order() {
        use DiagnosticsView::Overview;
        let (mut state, _, _) = state_with_two_channels();
        let one = state.networks()[0].id;
        state.dispatch(crate::Command::SelectServer(one));
        state.append_server_message(one, "needle early".into());
        note(&mut state, one, Overview, "needle diagnostic");
        state.append_server_message(one, "needle late".into());
        let hits = scan(&state, Scope::Selected, Some(Overview), "needle");
        let sources: Vec<_> = hits.iter().map(|hit| hit.source).collect();
        assert_eq!(
            sources,
            [
                Source::Diagnostics(one, Overview),
                Source::Server(one),
                Source::Server(one)
            ]
        );
        // The search starts at the newest message.
        assert_eq!(initial_hit(&hits, None), hits.last().copied());
    }

    #[test]
    fn a_diagnostic_hit_goes_stale_with_its_line_not_with_its_position() {
        use DiagnosticsView::Overview;
        let (mut state, _, _) = state_with_two_channels();
        let one = state.networks()[0].id;
        state.dispatch(crate::Command::SelectServer(one));
        note(&mut state, one, Overview, "needle");
        let hit = scan(&state, Scope::Selected, Some(Overview), "needle")[0];
        for n in 0..crate::DIAGNOSTIC_LIMIT {
            note(&mut state, one, Overview, &format!("line {n}"));
        }
        // Trimmed away: the hit names nothing, and no other line.
        assert!(!hit_exists(&state, hit));
        state.clear_diagnostics(one);
        note(&mut state, one, Overview, "needle");
        assert!(!hit_exists(&state, hit));
    }

    #[test]
    fn revision_follows_the_diagnostics_only_while_they_are_searched() {
        use DiagnosticsView::Overview;
        let (mut state, _, _) = state_with_two_channels();
        let one = state.networks()[0].id;
        state.dispatch(crate::Command::SelectServer(one));
        let off = revision(&state, Scope::Selected, None);
        let on = revision(&state, Scope::Selected, Some(Overview));
        note(&mut state, one, Overview, "x");
        assert_eq!(revision(&state, Scope::Selected, None), off);
        assert_ne!(revision(&state, Scope::Selected, Some(Overview)), on);
        let all = revision(&state, Scope::All, Some(Overview));
        let two = state.networks()[1].id;
        note(&mut state, two, DiagnosticsView::Protocol, "y");
        assert_ne!(revision(&state, Scope::All, Some(Overview)), all);
    }
}

#[cfg(test)]
mod measurement {
    use super::*;
    use crate::NetworkConfig;

    /// Prints how long a full scan of every retained line takes (50
    /// conversations of 2,000 lines): `cargo test -p cayenchat-app
    /// search_cost -- --ignored --nocapture` (add `--release` for the
    /// figure that matters). Documented in spec/performance.md.
    #[test]
    #[ignore = "measurement, not a check"]
    fn search_cost() {
        let channels: Vec<String> = (0..50).map(|n| format!("#c{n}")).collect();
        let mut state = AppState::with_networks(vec![NetworkConfig {
            id: NetworkId(1),
            name: "one".into(),
            channels: channels.clone(),
        }]);
        for name in &channels {
            let id = state.channel_id(NetworkId(1), name).unwrap();
            for n in 0..2_000 {
                state.append_conversation_activity(
                    id,
                    format!("line {n} of a fairly ordinary chat message with some words in it"),
                );
            }
        }
        // Long lines of one repeated character, the worst case for a naive
        // matcher: 100 lines of 16,000 characters against a 512-character term.
        let id = state.channel_id(NetworkId(1), "#c0").unwrap();
        for _ in 0..100 {
            state.append_conversation_activity(id, "a".repeat(16_000));
        }
        let repeated = format!("{}b", "a".repeat(512));
        for term in ["zzz", "Ordinary", "こんにちは", repeated.as_str()] {
            let start = std::time::Instant::now();
            let mut scan = Scan::new(&state, Scope::All, None, Query::new(term).unwrap());
            let mut slices = 0;
            let mut longest = std::time::Duration::ZERO;
            loop {
                let slice = std::time::Instant::now();
                let done = scan.step(&state, 20_000);
                longest = longest.max(slice.elapsed());
                slices += 1;
                if done {
                    break;
                }
            }
            println!(
                "{term:?}: {} hits, {slices} slices, total {:?}, longest slice {longest:?}",
                scan.hits().len(),
                start.elapsed()
            );
        }
    }
}
