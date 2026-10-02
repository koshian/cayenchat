//! Sent-draft history for the chat input: in memory only and bounded.

/// Most recent sent drafts kept.
const LIMIT: usize = 20;

#[derive(Default)]
pub struct InputHistory {
    /// Oldest first.
    entries: Vec<String>,
    browsing: Option<Browsing>,
}

struct Browsing {
    index: usize,
    /// The text this history last put into the input; anything else in the
    /// input means the user edited it or moved elsewhere.
    shown: String,
    /// The unsent text that was in the input before browsing started.
    draft: String,
}

impl InputHistory {
    /// Remembers a sent draft and ends any browsing. Repeating the latest
    /// entry does not add another.
    pub fn record(&mut self, text: &str) {
        self.browsing = None;
        if text.trim().is_empty() || self.entries.last().is_some_and(|last| last == text) {
            return;
        }
        self.entries.push(text.to_owned());
        if self.entries.len() > LIMIT {
            self.entries.remove(0);
        }
    }

    /// The next older entry, given the input's current text. `None` leaves the
    /// input alone (no history, or already at the oldest).
    pub fn previous(&mut self, current: &str) -> Option<String> {
        self.drop_if_stale(current);
        let index = match &self.browsing {
            Some(browsing) => browsing.index.checked_sub(1)?,
            None => self.entries.len().checked_sub(1)?,
        };
        let draft = self
            .browsing
            .take()
            .map_or_else(|| current.to_owned(), |browsing| browsing.draft);
        Some(self.show(index, draft))
    }

    /// The next newer entry, or the unsent text once past the newest. `None`
    /// when not browsing.
    pub fn next(&mut self, current: &str) -> Option<String> {
        self.drop_if_stale(current);
        let browsing = self.browsing.take()?;
        if browsing.index + 1 < self.entries.len() {
            Some(self.show(browsing.index + 1, browsing.draft))
        } else {
            Some(browsing.draft)
        }
    }

    fn show(&mut self, index: usize, draft: String) -> String {
        let shown = self.entries[index].clone();
        self.browsing = Some(Browsing {
            index,
            shown: shown.clone(),
            draft,
        });
        shown
    }

    fn drop_if_stale(&mut self, current: &str) {
        if self
            .browsing
            .as_ref()
            .is_some_and(|browsing| browsing.shown != current)
        {
            self.browsing = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(entries: &[&str]) -> InputHistory {
        let mut history = InputHistory::default();
        for entry in entries {
            history.record(entry);
        }
        history
    }

    #[test]
    fn walks_back_and_forth_and_restores_the_unsent_draft() {
        let mut history = history(&["one", "/join #two", "three"]);
        assert_eq!(history.previous("draft").as_deref(), Some("three"));
        assert_eq!(history.previous("three").as_deref(), Some("/join #two"));
        assert_eq!(history.previous("/join #two").as_deref(), Some("one"));
        assert_eq!(history.previous("one"), None, "stops at the oldest");
        assert_eq!(history.next("one").as_deref(), Some("/join #two"));
        assert_eq!(history.next("/join #two").as_deref(), Some("three"));
        assert_eq!(history.next("three").as_deref(), Some("draft"));
        assert_eq!(history.next("draft"), None, "no longer browsing");
    }

    #[test]
    fn is_bounded_and_skips_blanks_and_immediate_repeats() {
        let mut history = InputHistory::default();
        history.record("   ");
        assert_eq!(history.previous(""), None);
        for n in 0..LIMIT + 5 {
            history.record(&n.to_string());
            history.record(&n.to_string());
        }
        assert_eq!(history.entries.len(), LIMIT);
        assert_eq!(history.entries[0], "5");
    }

    #[test]
    fn editing_the_recalled_text_starts_over() {
        let mut history = history(&["one", "two"]);
        assert_eq!(history.previous("").as_deref(), Some("two"));
        // Edited, so the draft is now "two!" and browsing restarts from the newest.
        assert_eq!(history.previous("two!").as_deref(), Some("two"));
        assert_eq!(history.next("two").as_deref(), Some("two!"));
    }

    #[test]
    fn sending_ends_browsing() {
        let mut history = history(&["one", "two"]);
        assert_eq!(history.previous("").as_deref(), Some("two"));
        history.record("two");
        assert_eq!(history.next("two"), None);
    }
}
