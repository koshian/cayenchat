//! Virtualized log list state.
//!
//! Log panes render through GPUI's `list`, which lays out only the rows near
//! the viewport. Without it every redraw, including each keystroke or IME
//! preedit update in the draft input, rebuilt and shaped every retained log
//! line, which made typing and channel switching slow on Linux.

use gpui::{ListAlignment, ListState, px};

/// Extra height rendered beyond the viewport so scrolling does not pop in.
const OVERDRAW: f32 = 400.;

/// A bottom-anchored list of `prefix` fixed rows followed by messages whose
/// arrival sequences ascend. Bottom alignment keeps the newest line visible
/// while the user is at the end and preserves the position after scrolling up.
pub struct LogList {
    pub state: ListState,
    prefix: usize,
    len: usize,
    last_sequence: Option<u64>,
}

impl LogList {
    pub fn new() -> Self {
        Self {
            state: ListState::new(0, ListAlignment::Bottom, px(OVERDRAW)),
            prefix: 0,
            len: 0,
            last_sequence: None,
        }
    }

    pub fn clear(&mut self) {
        self.state.reset(0);
        self.prefix = 0;
        self.len = 0;
        self.last_sequence = None;
    }

    /// Tells the list which rows changed since the previous frame. Bounded logs
    /// only drop old lines from the front and append new ones at the end, so the
    /// previous last sequence locates the retained rows.
    pub fn sync(&mut self, prefix: usize, sequences: &[u64]) {
        if prefix != self.prefix {
            self.state.splice(0..self.prefix, prefix);
            self.prefix = prefix;
        }
        let retained = match self.last_sequence {
            Some(last) => sequences.binary_search(&last).ok().map(|index| index + 1),
            None => Some(0),
        };
        match retained {
            Some(retained) if retained <= self.len => {
                let removed = self.len - retained;
                if removed > 0 {
                    self.state.splice(prefix..prefix + removed, 0);
                }
                let added = sequences.len() - retained;
                if added > 0 {
                    let end = prefix + retained;
                    self.state.splice(end..end, added);
                }
            }
            _ => self.state.reset(prefix + sequences.len()),
        }
        self.len = sequences.len();
        self.last_sequence = sequences.last().copied();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_tracks_appends_front_trims_prefix_changes_and_resets() {
        let mut log = LogList::new();
        log.sync(0, &[1, 2, 3]);
        assert_eq!(log.state.item_count(), 3);

        log.sync(0, &[1, 2, 3, 7, 9]);
        assert_eq!(log.state.item_count(), 5);

        // A bounded log dropped its oldest lines and gained one more.
        log.sync(0, &[3, 7, 9, 10]);
        assert_eq!(log.state.item_count(), 4);

        // Status and diagnostic rows ahead of the messages.
        log.sync(2, &[3, 7, 9, 10]);
        assert_eq!(log.state.item_count(), 6);
        log.sync(1, &[3, 7, 9, 10, 11]);
        assert_eq!(log.state.item_count(), 6);

        // A replaced log (for example after reconnecting) starts over.
        log.sync(1, &[1]);
        assert_eq!(log.state.item_count(), 2);

        log.clear();
        assert_eq!(log.state.item_count(), 0);
        log.sync(0, &[]);
        assert_eq!(log.state.item_count(), 0);
    }
}
