// SPDX-License-Identifier: GPL-3.0-only
//! Phrase-aware wrapping for Japanese chat text (Experimental "Auto Phrase").
//! The text is never changed: BudouX's phrase boundaries become the offsets
//! GPUI's wrapper is told not to break at (see `StyledText::with_wrap_joins`).

use std::{collections::HashMap, sync::Arc};

use cayenchat_model::phrase;

/// Rows kept before the cache starts over. A screen shows a few dozen rows,
/// so this covers scrolling back and forth without growing with the log.
const CAPACITY: usize = 512;

#[derive(Default)]
pub(crate) struct PhraseJoins {
    cache: HashMap<Box<str>, Option<Arc<[u32]>>>,
}

impl PhraseJoins {
    /// Offsets into `prefix` + `text` where a wrap should be avoided, or
    /// `None` when there is nothing to avoid (text without Japanese). Nothing
    /// is analysed while `enabled` is false.
    pub(crate) fn joins(&mut self, enabled: bool, prefix: &str, text: &str) -> Option<Arc<[u32]>> {
        if !enabled {
            self.cache.clear();
            return None;
        }
        let full = format!("{prefix}{text}");
        if let Some(found) = self.cache.get(full.as_str()) {
            return found.clone();
        }
        let joins = phrase::joins(&full);
        let joins = (!joins.is_empty()).then(|| Arc::from(joins));
        if self.cache.len() >= CAPACITY {
            self.cache.clear();
        }
        self.cache.insert(full.into(), joins.clone());
        joins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_and_non_japanese_text_have_no_joins() {
        let mut joins = PhraseJoins::default();
        assert!(joins.joins(false, "<a> ", "今日は天気です。").is_none());
        assert!(
            joins
                .joins(true, "<a> ", "hello https://example.com/")
                .is_none()
        );
    }

    #[test]
    fn offsets_include_the_prefix_and_are_cached() {
        let mut joins = PhraseJoins::default();
        let first = joins.joins(true, "<a> ", "今日は天気です。").unwrap();
        // "<a> " is 4 bytes; 今日は|天気 breaks at 4 + 9, so 13 is not joined.
        assert!(first.contains(&7) && !first.contains(&13));
        let again = joins.joins(true, "<a> ", "今日は天気です。").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }
}
