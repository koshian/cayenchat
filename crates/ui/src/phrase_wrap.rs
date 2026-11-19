// SPDX-License-Identifier: GPL-3.0-only
//! Phrase-aware wrapping for Japanese chat text (Experimental "Auto Phrase").
//! The text is never changed: BudouX's phrase boundaries become the offsets
//! GPUI's wrapper is told not to break at (see `StyledText::with_wrap_joins`).
//! Only the message's natural-language stretches are analysed: the sender
//! prefix and protected spans (links, nick highlights) are left alone.

use std::{collections::HashMap, ops::Range, sync::Arc};

use cayenchat_model::phrase;

/// Rows kept before the cache starts over. A screen shows a few dozen rows,
/// so this covers scrolling back and forth without growing with the log.
const CAPACITY: usize = 512;

#[derive(Default)]
pub(crate) struct PhraseJoins {
    cache: HashMap<Box<str>, Option<Arc<[u32]>>>,
}

impl PhraseJoins {
    /// Offsets (shifted by `shift`, the prefix length) where a wrap should be
    /// avoided, or `None` when there is nothing to avoid. `protected` are
    /// byte ranges of `text` that are not analysed. Nothing is analysed
    /// while `enabled` is false.
    pub(crate) fn joins(
        &mut self,
        enabled: bool,
        shift: usize,
        text: &str,
        protected: &[Range<usize>],
    ) -> Option<Arc<[u32]>> {
        if !enabled {
            self.cache.clear();
            return None;
        }
        let protected = merged(protected, text.len());
        let key = key(shift, &protected, text);
        if let Some(found) = self.cache.get(key.as_str()) {
            return found.clone();
        }
        let mut joins = Vec::new();
        for (from, to) in segments(&protected, text.len()) {
            joins.extend(
                phrase::joins(&text[from..to])
                    .into_iter()
                    .filter_map(|at| u32::try_from(at as usize + from + shift).ok()),
            );
        }
        let joins = (!joins.is_empty()).then(|| Arc::from(joins));
        if self.cache.len() >= CAPACITY {
            self.cache.clear();
        }
        self.cache.insert(key.into(), joins.clone());
        joins
    }
}

/// Sorted, non-overlapping ranges clamped to `len`.
fn merged(ranges: &[Range<usize>], len: usize) -> Vec<Range<usize>> {
    let mut ranges: Vec<_> = ranges
        .iter()
        .map(|range| range.start.min(len)..range.end.min(len))
        .filter(|range| range.start < range.end)
        .collect();
    ranges.sort_by_key(|range| range.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// The stretches of `0..len` outside `protected` (sorted, disjoint).
fn segments(protected: &[Range<usize>], len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(protected.len() + 1);
    let mut at = 0;
    for range in protected {
        if at < range.start {
            out.push((at, range.start));
        }
        at = range.end;
    }
    if at < len {
        out.push((at, len));
    }
    out
}

/// Cache key: the shift and protected ranges as digits up to the first `|`,
/// then the text, so different splits of the same text never collide.
fn key(shift: usize, protected: &[Range<usize>], text: &str) -> String {
    use std::fmt::Write;
    let mut key = format!("{shift};");
    for range in protected {
        let _ = write!(key, "{}-{},", range.start, range.end);
    }
    key.push('|');
    key.push_str(text);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    const JA: &str = "今日は天気です。";

    #[test]
    fn disabled_and_non_japanese_text_have_no_joins() {
        let mut joins = PhraseJoins::default();
        assert!(joins.joins(false, 4, JA, &[]).is_none());
        assert!(
            joins
                .joins(true, 4, "hello https://example.com/", &[])
                .is_none()
        );
        assert!(
            joins
                .joins(true, 0, "这是一个中文消息测试。", &[])
                .is_none()
        );
    }

    #[test]
    fn offsets_include_the_prefix_and_are_cached() {
        let mut joins = PhraseJoins::default();
        let first = joins.joins(true, 4, JA, &[]).unwrap();
        // 今日は|天気 breaks at 9 in the text, 13 with the 4-byte prefix.
        assert!(first.contains(&7) && !first.contains(&13));
        let again = joins.joins(true, 4, JA, &[]).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        // A different protected range or shift is a different entry.
        let other = joins.joins(true, 5, JA, &[]).unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(
            joins
                .joins(true, 4, JA, std::slice::from_ref(&(0..JA.len())))
                .is_none()
        );
    }

    #[test]
    fn protected_spans_are_never_joined() {
        let mut joins = PhraseJoins::default();
        let text = format!("{JA}https://例.jp/美味しいラーメンを{JA}");
        let url = JA.len()..JA.len() + "https://例.jp/美味しいラーメンを".len();
        let found = joins
            .joins(true, 0, &text, std::slice::from_ref(&url))
            .unwrap();
        assert!(found.iter().all(|&at| {
            let at = at as usize;
            at <= url.start || at >= url.end
        }));
        // Both natural-language sides are still analysed.
        assert!(found.iter().any(|&at| (at as usize) < url.start));
        assert!(found.iter().any(|&at| (at as usize) > url.end));
    }

    #[test]
    fn segments_cover_the_gaps_and_overlaps_merge() {
        let protected = merged(&[5..8, 2..6, 20..30, 9..9], 12);
        assert_eq!(protected, vec![2..8]);
        assert_eq!(segments(&protected, 12), [(0, 2), (8, 12)]);
        assert_eq!(segments(&[], 3), [(0, 3)]);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use gpui::{Font, FontFeatures, FontStyle, FontWeight, SharedString, TestAppContext, TextRun};

    /// Byte offsets where `text` starts a new visual line.
    fn breaks(
        cx: &mut gpui::VisualTestContext,
        text: &str,
        width: f32,
        joins: Option<&Arc<[u32]>>,
    ) -> Vec<usize> {
        let run = TextRun {
            len: text.len(),
            font: Font {
                family: ".ZedMono".into(),
                features: FontFeatures::default(),
                fallbacks: None,
                weight: FontWeight::default(),
                style: FontStyle::Normal,
            },
            color: Default::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let lines = cx.update(|window, _| {
            window
                .text_system()
                .shape_text_joined(
                    SharedString::from(text.to_owned()),
                    gpui::px(16.),
                    &[run],
                    Some(gpui::px(width)),
                    None,
                    joins,
                )
                .unwrap()
        });
        let layout = &lines[0];
        layout
            .wrap_boundaries()
            .iter()
            .map(|boundary| {
                let glyph =
                    &layout.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
                glyph.index
            })
            .collect()
    }

    #[gpui::test]
    fn joins_move_breaks_to_phrase_boundaries_without_losing_text(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let text = "私は遠くから来た友達と一緒に美味しいラーメンを食べに行きました。";
        let mut cache = PhraseJoins::default();
        let joins = cache.joins(true, 0, text, &[]).unwrap();
        let phrase_starts: Vec<usize> = cayenchat_model::phrase::boundaries(text);
        let mut moved = false;
        for width in (60..400).step_by(7) {
            let off = breaks(cx, text, width as f32, None);
            let on = breaks(cx, text, width as f32, Some(&joins));
            // Same text, only the break positions differ; lines still wrap.
            assert_eq!(off.is_empty(), on.is_empty(), "width {width}");
            // Every ON break is a phrase boundary unless a phrase alone is
            // wider than the line (then the fallback breaks anywhere).
            let widest = phrase_starts
                .iter()
                .chain([&text.len()])
                .scan(0, |prev, &at| {
                    let len = text[*prev..at].chars().count();
                    *prev = at;
                    Some(len)
                })
                .max()
                .unwrap();
            if widest as f32 * 16. <= width as f32 {
                assert!(
                    on.iter().all(|at| phrase_starts.contains(at)),
                    "{width}: {on:?}"
                );
            }
            moved |= off != on;
        }
        assert!(moved, "joins never changed a break");
    }

    #[gpui::test]
    fn a_phrase_wider_than_the_line_still_wraps(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let text = "ラーメンを食べに行きました。";
        let joins = PhraseJoins::default().joins(true, 0, text, &[]).unwrap();
        let on = breaks(cx, text, 40., Some(&joins));
        assert!(!on.is_empty());
        assert!(on.iter().all(|&at| text.is_char_boundary(at) && at > 0));
    }
}
