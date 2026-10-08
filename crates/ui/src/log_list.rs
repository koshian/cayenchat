//! Virtualized log list state.
//!
//! Log panes render through GPUI's `list`, which lays out only the rows near
//! the viewport. Without it every redraw, including each keystroke or IME
//! preedit update in the draft input, rebuilt and shaped every retained log
//! line, which made typing and channel switching slow on Linux.

use gpui::{ListAlignment, ListOffset, ListState, Pixels, px};

/// Extra height rendered beyond the viewport so scrolling does not pop in.
const OVERDRAW: f32 = 400.;

/// A list of `prefix` fixed rows followed by rows identified by ascending keys
/// (message arrival sequences for logs). Logs are bottom-anchored, which keeps
/// the newest line visible while the user is at the end and preserves the
/// position after scrolling up.
pub struct LogList {
    pub state: ListState,
    prefix: usize,
    sequences: Vec<u64>,
    scroll_handler: bool,
}

impl LogList {
    pub fn new() -> Self {
        Self::with_alignment(ListAlignment::Bottom)
    }

    /// A list anchored at the top, such as the channel tree.
    pub fn new_top() -> Self {
        Self::with_alignment(ListAlignment::Top)
    }

    fn with_alignment(alignment: ListAlignment) -> Self {
        Self {
            state: ListState::new(0, alignment, px(OVERDRAW)),
            prefix: 0,
            sequences: Vec::new(),
            scroll_handler: false,
        }
    }

    /// Calls `handler` with the first visible row whenever the user scrolls
    /// the list; set once, later calls are ignored. The list's state is
    /// borrowed while it runs, so the handler must defer anything that
    /// reads the list.
    pub fn on_scroll(&mut self, handler: impl Fn(usize, &mut gpui::App) + 'static) {
        if self.scroll_handler {
            return;
        }
        self.scroll_handler = true;
        self.state
            .set_scroll_handler(move |event, _, cx| handler(event.visible_range.start, cx));
    }

    /// Tells the list which rows changed since the previous sync. Only rows
    /// whose sequence appeared or disappeared are replaced, so the others keep
    /// their measured heights and the scroll position. This covers appends,
    /// bounded logs dropping old lines, older history inserted above
    /// everything, and the combined log swapping one channel's lines for
    /// another's when the selection changes.
    ///
    /// A list scrolled away from the bottom keeps the same message at its
    /// top, at the same pixel offset within that row, whatever was inserted
    /// or removed above it: the anchor is the row's sequence, not its index,
    /// and rows above it need no measured height. A list following the
    /// bottom keeps following it.
    pub fn sync(&mut self, prefix: usize, sequences: &[u64]) {
        let anchor = self.anchor();
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
        // GPUI shifts the scroll top by the rows spliced in above it; this
        // restores it from identity in any case.
        if let Some((sequence, offset_in_item)) = anchor
            && let Ok(index) = self.sequences.binary_search(&sequence)
        {
            self.scroll_to_row(prefix + index, offset_in_item);
        }
    }

    /// Like [`sync`](Self::sync) for keys in any order, such as the channel
    /// tree after the user reordered it. Rows equal at the start and at the
    /// end of both lists are kept; the span between them is replaced. The row
    /// at the top stays there when it still exists.
    pub fn sync_unordered(&mut self, prefix: usize, keys: &[u64]) {
        let anchor = self.anchor();
        if prefix != self.prefix {
            self.state.splice(0..self.prefix, prefix);
            self.prefix = prefix;
        }
        if keys == self.sequences.as_slice() {
            return;
        }
        let old = &self.sequences;
        let head = old.iter().zip(keys).take_while(|(a, b)| a == b).count();
        let tail = old[head..]
            .iter()
            .rev()
            .zip(keys[head..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.state.splice(
            prefix + head..prefix + old.len() - tail,
            keys.len() - head - tail,
        );
        self.sequences.clear();
        self.sequences.extend_from_slice(keys);
        if let Some((key, offset_in_item)) = anchor
            && let Some(index) = self.sequences.iter().position(|k| *k == key)
        {
            self.scroll_to_row(prefix + index, offset_in_item);
        }
    }

    fn scroll_to_row(&self, item_ix: usize, offset_in_item: Pixels) {
        let current = self.state.logical_scroll_top();
        if (current.item_ix, current.offset_in_item) != (item_ix, offset_in_item) {
            self.state.scroll_to(ListOffset {
                item_ix,
                offset_in_item,
            });
        }
    }

    /// The message row at the top of a scrolled list and the offset into
    /// it. `None` while following the bottom or with a fixed row on top.
    fn anchor(&self) -> Option<(u64, Pixels)> {
        let top = self.state.logical_scroll_top();
        let index = top.item_ix.checked_sub(self.prefix)?;
        self.sequences
            .get(index)
            .map(|sequence| (*sequence, top.offset_in_item))
    }

    /// How many message rows lie above row `visible_start` (the first row a
    /// scroll event reports as visible).
    pub fn message_rows_above(&self, visible_start: usize) -> usize {
        visible_start.saturating_sub(self.prefix)
    }

    /// Forgets the measured height of the row with `sequence`, whose content
    /// changed size while it may have been off screen (an image preview
    /// finished). The row at the scroll top is left alone: it is on screen,
    /// so it is measured again anyway, and replacing it would reset the
    /// scroll offset within it.
    pub fn invalidate(&mut self, sequence: u64) {
        let Ok(index) = self.sequences.binary_search(&sequence) else {
            return;
        };
        let index = self.prefix + index;
        if self.state.logical_scroll_top().item_ix != index {
            self.state.splice(index..index + 1, 1);
        }
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

        log.sync(0, &[]);
        assert_eq!(log.state.item_count(), 0);
    }

    #[test]
    fn unordered_keys_keep_the_row_count_and_the_row_at_the_top() {
        let mut log = LogList::new();
        log.sync_unordered(0, &[1, 2, 3, 4, 5, 6]);
        log.state.scroll_to(ListOffset {
            item_ix: 4,
            offset_in_item: px(3.),
        });
        // Key 5 moves from index 4 to index 1; the top row follows it.
        log.sync_unordered(0, &[1, 5, 2, 3, 4, 6]);
        assert_eq!(log.state.item_count(), 6);
        assert_eq!(log.sequences, [1, 5, 2, 3, 4, 6]);
        assert_eq!(log.state.logical_scroll_top().item_ix, 1);
        log.sync_unordered(0, &[6, 1, 5, 2, 3]);
        assert_eq!(log.state.item_count(), 5);
        log.sync_unordered(0, &[]);
        assert_eq!(log.state.item_count(), 0);
    }

    #[test]
    fn prepending_keeps_the_top_message_and_its_offset() {
        let mut log = LogList::new();
        log.sync(1, &[100, 101, 102, 103, 104]);
        log.state.scroll_to(ListOffset {
            item_ix: 1 + 2,
            offset_in_item: px(7.),
        });
        // An older page above everything, and a live line below.
        log.sync(1, &[40, 41, 42, 100, 101, 102, 103, 104, 105]);
        let top = log.state.logical_scroll_top();
        assert_eq!(top.item_ix, 1 + 5, "still message 102");
        assert_eq!(top.offset_in_item, px(7.));
        assert_eq!(log.anchor(), Some((102, px(7.))));
        // Trimming lines above it does not move it either.
        log.sync(1, &[101, 102, 103, 104, 105]);
        assert_eq!(log.anchor(), Some((102, px(7.))));
        assert_eq!(log.state.logical_scroll_top().item_ix, 1 + 1);
        // A prefix row appearing above it is another row above it.
        log.sync(3, &[101, 102, 103, 104, 105]);
        assert_eq!(log.anchor(), Some((102, px(7.))));
    }

    #[test]
    fn a_list_following_the_bottom_keeps_following_it() {
        let mut log = LogList::new();
        log.sync(0, &[10, 11]);
        assert!(log.anchor().is_none());
        log.sync(0, &[1, 2, 3, 10, 11, 12]);
        assert!(log.anchor().is_none());
        assert_eq!(log.state.logical_scroll_top().item_ix, 6);
    }

    #[test]
    fn invalidating_a_row_keeps_the_count_and_ignores_unknown_rows() {
        let mut log = LogList::new();
        log.sync(1, &[3, 5, 8]);
        log.invalidate(5);
        log.invalidate(4);
        assert_eq!(log.state.item_count(), 4);
        assert_eq!(log.sequences, [3, 5, 8]);
    }

    /// Rows of different heights (wrapped text, image previews) are drawn,
    /// the list is scrolled into the middle, and an older page of rows of
    /// other heights is inserted above: the rows on screen stay where they
    /// were, before and after the new rows are measured.
    #[gpui::test]
    fn prepended_rows_of_any_height_do_not_move_the_viewport(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, list};
        use std::{cell::RefCell, rc::Rc};

        struct Log {
            list: LogList,
            heights: Rc<RefCell<Vec<f32>>>,
        }
        impl Render for Log {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let heights = self.heights.clone();
                div().w(px(300.)).h(px(200.)).child(
                    list(self.list.state.clone(), move |index, _, _| {
                        div()
                            .w_full()
                            .h(px(heights.borrow()[index]))
                            .into_any_element()
                    })
                    .size_full(),
                )
            }
        }

        let height = |n: u64| [20., 64., 20., 36., 120., 20.][n as usize % 6];
        let old: Vec<u64> = (100..140).collect();
        let heights = Rc::new(RefCell::new(old.iter().map(|n| height(*n)).collect()));
        let (view, cx) = cx.add_window_view(|_, _| {
            let mut list = LogList::new();
            list.sync(0, &old);
            Log {
                list,
                heights: heights.clone(),
            }
        });
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        };
        draw(cx);
        view.update(cx, |log, _| {
            log.list.state.scroll_to(ListOffset {
                item_ix: 20,
                offset_in_item: px(13.),
            })
        });
        draw(cx);
        let positions = |log: &Log| {
            (20..24)
                .map(|index| log.list.state.bounds_for_item(index).map(|b| b.origin.y))
                .collect::<Vec<_>>()
        };
        let before = view.read_with(cx, |log, _| positions(log));
        assert!(before.iter().all(Option::is_some), "{before:?}");

        let page: Vec<u64> = (60..75).collect();
        view.update(cx, |log, _| {
            let mut all = page.clone();
            all.extend(&old);
            let mut rows: Vec<f32> = page.iter().map(|n| height(*n) + 3.).collect();
            rows.extend(log.heights.borrow().iter());
            *log.heights.borrow_mut() = rows;
            log.list.sync(0, &all);
        });
        for _ in 0..2 {
            draw(cx);
            let after = view.read_with(cx, |log, _| {
                let top = log.list.state.logical_scroll_top();
                assert_eq!(top.item_ix, 20 + page.len());
                assert_eq!(top.offset_in_item, px(13.));
                (20..24)
                    .map(|index| {
                        log.list
                            .state
                            .bounds_for_item(index + page.len())
                            .map(|b| b.origin.y)
                    })
                    .collect::<Vec<_>>()
            });
            assert_eq!(after, before);
        }
    }
}
