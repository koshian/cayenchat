// SPDX-License-Identifier: GPL-3.0-only
//! The wrapped layout of a multi-line `TextInput` (#616): the rows the text is
//! drawn in, and the conversions between byte offsets and positions that
//! drawing, the caret, the mouse, Up/Down and the input method need.
//!
//! A hard line (ended by `\n`) is one shaped paragraph that wraps into one or
//! more rows. Positions are relative to the top left of the text, before the
//! input's vertical scroll. An offset on the break between two wrapped rows
//! belongs to the lower row, so Down and a click at the start of a row land
//! where they look.

use gpui::{App, Bounds, Pixels, Point, TextAlign, Window, WrappedLine, point, px, size};

/// Width of the mark that stands for a selected line break.
const BREAK_MARK: Pixels = px(6.);

#[derive(Clone, Copy, Debug)]
struct Row {
    /// Index of the paragraph the row belongs to.
    line: usize,
    /// Byte offset of the paragraph in the text.
    base: usize,
    /// Byte offsets of the row's first character and the end of its text.
    start: usize,
    end: usize,
    /// Whether the row ends in a soft wrap (else at a line break or the end).
    soft: bool,
    /// The x of the row's start in the unwrapped paragraph.
    x0: Pixels,
}

#[derive(Clone)]
pub(crate) struct WrappedText {
    lines: Vec<WrappedLine>,
    rows: Vec<Row>,
    line_height: Pixels,
}

impl WrappedText {
    pub(crate) fn new(lines: impl IntoIterator<Item = WrappedLine>, line_height: Pixels) -> Self {
        let lines: Vec<WrappedLine> = lines.into_iter().collect();
        let mut rows = Vec::new();
        let mut base = 0;
        for (line_ix, line) in lines.iter().enumerate() {
            let (mut start, mut x0) = (0, px(0.));
            for boundary in line.wrap_boundaries.iter() {
                let Some(glyph) = line
                    .unwrapped_layout
                    .runs
                    .get(boundary.run_ix)
                    .and_then(|run| run.glyphs.get(boundary.glyph_ix))
                else {
                    break;
                };
                rows.push(Row {
                    line: line_ix,
                    base,
                    start: base + start,
                    end: base + glyph.index,
                    soft: true,
                    x0,
                });
                (start, x0) = (glyph.index, glyph.position.x);
            }
            rows.push(Row {
                line: line_ix,
                base,
                start: base + start,
                end: base + line.len(),
                soft: false,
                x0,
            });
            base += line.len() + 1;
        }
        Self {
            lines,
            rows,
            line_height,
        }
    }

    pub(crate) fn line_height(&self) -> Pixels {
        self.line_height
    }

    pub(crate) fn height(&self) -> Pixels {
        self.line_height * self.rows.len()
    }

    fn row_for_offset(&self, offset: usize) -> usize {
        self.rows
            .partition_point(|row| row.start <= offset)
            .saturating_sub(1)
    }

    fn x_in_row(&self, row: &Row, offset: usize) -> Pixels {
        let offset = offset.clamp(row.start, row.end) - row.base;
        self.lines[row.line].unwrapped_layout.x_for_index(offset) - row.x0
    }

    /// Where the caret at `offset` is drawn: the top left of its cell.
    pub(crate) fn point_for_offset(&self, offset: usize) -> Point<Pixels> {
        let ix = self.row_for_offset(offset);
        point(self.x_in_row(&self.rows[ix], offset), self.line_height * ix)
    }

    /// The offset closest to `position`; outside the text it is the nearest
    /// row's start or end.
    pub(crate) fn offset_for_point(&self, position: Point<Pixels>) -> usize {
        self.offset_at(position, false)
    }

    /// Like `offset_for_point`, but the offset stays on the row at `position`:
    /// on a wrapped row it is at most the row's last character, since its end
    /// belongs to the next row. Up and Down use it to really change rows.
    pub(crate) fn offset_in_row(&self, position: Point<Pixels>) -> usize {
        self.offset_at(position, true)
    }

    fn offset_at(&self, position: Point<Pixels>, stay_in_row: bool) -> usize {
        let ix = if position.y <= px(0.) {
            0
        } else {
            ((position.y / self.line_height) as usize).min(self.rows.len() - 1)
        };
        let row = &self.rows[ix];
        if position.x <= px(0.) {
            return row.start;
        }
        let at = self.lines[row.line]
            .unwrapped_layout
            .closest_index_for_x(position.x + row.x0)
            + row.base;
        let at = at.clamp(row.start, row.end);
        if stay_in_row && row.soft && at == row.end {
            return self.lines[row.line]
                .unwrapped_layout
                .runs
                .iter()
                .flat_map(|run| run.glyphs.iter())
                .map(|glyph| row.base + glyph.index)
                .filter(|&index| index < row.end)
                .max()
                .map_or(row.start, |last| last.max(row.start));
        }
        at
    }

    /// One rectangle per row the range covers, with a small mark where it
    /// includes a line break.
    pub(crate) fn rects_for_range(&self, range: &std::ops::Range<usize>) -> Vec<Bounds<Pixels>> {
        let mut rects = Vec::new();
        for (ix, row) in self.rows.iter().enumerate() {
            let (start, end) = (range.start.max(row.start), range.end.min(row.end));
            let break_selected = !row.soft && range.start <= row.end && range.end > row.end;
            if start > end || (start == end && !break_selected) {
                continue;
            }
            let left = self.x_in_row(row, start);
            let mut right = self.x_in_row(row, end);
            if break_selected {
                right += BREAK_MARK;
            }
            rects.push(Bounds::new(
                point(left, self.line_height * ix),
                size(right - left, self.line_height),
            ));
        }
        rects
    }

    /// Draws the text with its top left at `origin`.
    pub(crate) fn paint(&self, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let mut row = 0;
        for line in &self.lines {
            let at = point(origin.x, origin.y + self.line_height * row);
            // A failed paint only loses this line.
            let _ = line.paint(at, self.line_height, TextAlign::Left, None, window, cx);
            row += line.wrap_boundaries.len() + 1;
        }
    }
}
