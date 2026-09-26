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
    sequences: Vec<u64>,
}

impl LogList {
    pub fn new() -> Self {
        Self {
            state: ListState::new(0, ListAlignment::Bottom, px(OVERDRAW)),
            prefix: 0,
            sequences: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.state.reset(0);
        self.prefix = 0;
        self.sequences.clear();
    }

    /// Tells the list which rows changed since the previous sync. Only rows
    /// whose sequence appeared or disappeared are replaced, so the others keep
    /// their measured heights and the scroll position. This covers appends,
    /// bounded logs dropping old lines, and the combined log swapping one
    /// channel's lines for another's when the selection changes.
    pub fn sync(&mut self, prefix: usize, sequences: &[u64]) {
        if prefix != self.prefix {
            self.state.splice(0..self.prefix, prefix);
            self.prefix = prefix;
        }
        if sequences == self.sequences.as_slice() {
            return;
        }
        // Both lists ascend. Walk from the end so splicing a run of changes
        // leaves the indices of earlier rows valid.
        let old = &self.sequences;
        let (mut i, mut j) = (old.len(), sequences.len());
        while i > 0 || j > 0 {
            if i > 0 && j > 0 && old[i - 1] == sequences[j - 1] {
                i -= 1;
                j -= 1;
                continue;
            }
            let (old_end, new_end) = (i, j);
            while (i > 0 || j > 0) && !(i > 0 && j > 0 && old[i - 1] == sequences[j - 1]) {
                if j == 0 || (i > 0 && old[i - 1] > sequences[j - 1]) {
                    i -= 1;
                } else {
                    j -= 1;
                }
            }
            self.state.splice(prefix + i..prefix + old_end, new_end - j);
        }
        self.sequences.clear();
        self.sequences.extend_from_slice(sequences);
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

        // A replaced log (for example after reconnecting).
        log.sync(1, &[1]);
        assert_eq!(log.state.item_count(), 2);

        // The combined log swaps lines in the middle when the selection moves.
        log.sync(1, &[1, 4, 5, 8]);
        assert_eq!(log.state.item_count(), 5);
        log.sync(1, &[1, 5, 6, 7, 8]);
        assert_eq!(log.state.item_count(), 6);
        assert_eq!(log.sequences, [1, 5, 6, 7, 8]);

        log.clear();
        assert_eq!(log.state.item_count(), 0);
        log.sync(0, &[]);
        assert_eq!(log.state.item_count(), 0);
    }
}
