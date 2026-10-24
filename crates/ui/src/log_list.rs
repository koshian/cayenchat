// SPDX-License-Identifier: GPL-3.0-only
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
    shapes: Vec<u8>,
    scroll_handler: bool,
}

impl LogList {
    /// Scrolls so that row `at` is fully visible, moving as little as
    /// possible. The list only knows the height of rows it has laid out, and
    /// reveals a row after unmeasured ones wrongly. Then this scrolls part of
    /// the way instead, so the next layout measures the rows up to `at`, and
    /// returns true: call it again after that layout until it returns false.
    pub fn reveal(&self, at: usize) -> bool {
        let top = self.state.logical_scroll_top().item_ix;
        if at <= top || (top..=at).all(|ix| self.state.bounds_for_item(ix).is_some()) {
            self.state.scroll_to_reveal_item(at);
            return false;
        }
        // Rows are not shorter than this, so about a viewport of them is
        // laid out from the new top; half the distance left is the fallback
        // for taller rows, which makes the steps converge.
        const MIN_ROW_HEIGHT: f32 = 16.0;
        let viewport = (self.state.viewport_bounds().size.height / px(MIN_ROW_HEIGHT)).ceil();
        let rows = (viewport as usize).min((at - top).div_ceil(2));
        let mut target = at - rows;
        if target == top {
            // A step to the current top makes no progress when that row fills
            // the overdraw; start at `at` itself, which is then measured.
            target = at;
        }
        self.state.scroll_to(ListOffset {
            item_ix: target,
            offset_in_item: px(0.),
        });
        true
    }

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
            shapes: Vec::new(),
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
        self.sync_shaped(prefix, sequences, &[]);
    }

    /// [`sync`](Self::sync) for rows whose look depends on their neighbours.
    /// `shapes` holds one value per row (empty: all rows have shape 0); a row
    /// kept from the previous sync whose shape changed has its measured
    /// height forgotten, as [`invalidate`](Self::invalidate) does.
    pub fn sync_shaped(&mut self, prefix: usize, sequences: &[u64], shapes: &[u8]) {
        let anchor = self.anchor();
        if prefix != self.prefix {
            self.state.splice(0..self.prefix, prefix);
            self.prefix = prefix;
        }
        if sequences == self.sequences.as_slice() && shapes == self.shapes.as_slice() {
            return;
        }
        let reshaped = self.reshaped(sequences, shapes);
        self.shapes.clear();
        self.shapes.extend_from_slice(shapes);
        if sequences == self.sequences.as_slice() {
            for sequence in reshaped {
                self.invalidate(sequence);
            }
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
        for sequence in reshaped {
            self.invalidate(sequence);
        }
    }

    /// The sequences present before and after whose shape differs. One
    /// merge walk over both ascending lists, only when shapes changed.
    fn reshaped(&self, sequences: &[u64], shapes: &[u8]) -> Vec<u64> {
        if shapes == self.shapes.as_slice() {
            return Vec::new();
        }
        let shape = |shapes: &[u8], index: usize| shapes.get(index).copied().unwrap_or(0);
        let (old, mut changed) = (&self.sequences, Vec::new());
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < sequences.len() {
            match old[i].cmp(&sequences[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    if shape(&self.shapes, i) != shape(shapes, j) {
                        changed.push(old[i]);
                    }
                    (i, j) = (i + 1, j + 1);
                }
            }
        }
        changed
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
        self.shapes.clear();
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

    /// Scrolls so the row with `sequence` is at the top; a line the list
    /// does not hold is ignored.
    pub fn scroll_to_sequence(&mut self, sequence: u64) {
        if let Ok(index) = self.sequences.binary_search(&sequence) {
            self.scroll_to_row(self.prefix + index, px(0.));
        }
    }

    /// Scrolls to the newest line and follows the bottom again, the same
    /// state as a list that was never scrolled.
    pub fn scroll_to_end(&self) {
        self.scroll_to_row(self.state.item_count(), px(0.));
    }

    /// Whether the end of the last row is out of view. Reads the layout of
    /// the previous frame, so it is cheap, and a list whose rows all fit has
    /// nothing behind.
    pub fn is_behind_latest(&self) -> bool {
        /// Less than this is rounding of the measured heights.
        const TOLERANCE: f32 = 1.;
        let max = f32::from(self.state.max_offset_for_scrollbar().height);
        let offset = f32::from(-self.state.scroll_px_offset_for_scrollbar().y);
        offset < max - TOLERANCE
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
    fn a_search_hit_goes_to_the_top_whatever_the_rows_above_it_are() {
        let mut log = LogList::new();
        log.sync(1, &[3, 7, 9, 10, 11]);
        log.scroll_to_sequence(10);
        let top = log.state.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (1 + 3, px(0.)));
        // A line the list no longer holds leaves the position alone.
        log.scroll_to_sequence(8);
        assert_eq!(log.state.logical_scroll_top().item_ix, 4);
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

    /// A row kept across a sync whose shape changed is measured again; the
    /// row count and the other rows stay as they are.
    #[gpui::test]
    fn a_kept_row_whose_shape_changed_is_measured_again(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, list};
        use std::{cell::RefCell, rc::Rc};

        struct Log {
            list: LogList,
            heights: Rc<RefCell<Vec<f32>>>,
        }
        impl Render for Log {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let heights = self.heights.clone();
                div().w(px(300.)).h(px(300.)).child(
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

        let heights = Rc::new(RefCell::new(vec![40.; 10]));
        let (view, cx) = cx.add_window_view(|_, _| {
            let mut list = LogList::new();
            list.sync_shaped(0, &(1..=10).collect::<Vec<_>>(), &[0; 10]);
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
                item_ix: 0,
                offset_in_item: px(0.),
            })
        });
        draw(cx);
        let height_of = |cx: &mut gpui::VisualTestContext, index| {
            view.read_with(cx, |log, _| {
                log.list.state.bounds_for_item(index).map(|b| b.size.height)
            })
        };
        assert_eq!(height_of(cx, 1), Some(px(40.)));
        // Older line 0 joins in front of line 1, which loses its header.
        view.update(cx, |log, _| {
            *log.heights.borrow_mut() = [vec![40., 20.], vec![40.; 9]].concat();
            log.list.sync_shaped(
                0,
                &(0..=10).collect::<Vec<_>>(),
                &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        });
        draw(cx);
        assert_eq!(height_of(cx, 1), Some(px(20.)));
        assert_eq!(height_of(cx, 2), Some(px(40.)));
        assert_eq!(view.read_with(cx, |log, _| log.list.state.item_count()), 11);
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
