// SPDX-License-Identifier: GPL-3.0-only
//! The unit a double or triple click selects, shared by the text inputs and
//! the logs so the rule lives in one place. Pure functions over text and a
//! byte offset; independent of the Auto Phrase setting, which only affects
//! wrapping.
//!
//! The character at the click decides the rule (the one after the offset,
//! or the last one at the end of a line):
//!
//! - Japanese (kana, kanji, CJK punctuation): the BudouX phrase containing
//!   it, kept inside the run of Japanese characters around it. A run
//!   without kana has no boundaries and is one unit.
//! - Letters and digits (and anything else UAX #29 calls a word): the word.
//! - White space: the whole run of white space.
//! - Anything else (symbols, punctuation): the run of such characters next
//!   to each other, never an empty range.
//!
//! A line break ends every unit. On an empty line the range is empty.

use std::ops::Range;

use cayenchat_model::phrase;
use unicode_segmentation::UnicodeSegmentation;

/// How far a click selects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SelectUnit {
    /// A phrase or word (double click).
    Word,
    /// The whole line (triple click).
    Line,
}

/// The unit a press with this click count selects: none for a single click.
pub(crate) fn of_click(click_count: usize) -> Option<SelectUnit> {
    match click_count {
        0 | 1 => None,
        2 => Some(SelectUnit::Word),
        _ => Some(SelectUnit::Line),
    }
}

/// The range `unit` selects around the byte `offset` of `text`.
pub(crate) fn range(text: &str, offset: usize, unit: SelectUnit) -> Range<usize> {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |at| offset + at);
    let line = &text[start..end];
    let inner = match unit {
        SelectUnit::Line => 0..line.len(),
        SelectUnit::Word => word_range(line, offset - start),
    };
    start + inner.start..start + inner.end
}

/// The word at `offset` of one line.
fn word_range(line: &str, offset: usize) -> Range<usize> {
    let hit = match line[offset..].chars().next() {
        Some(ch) => (offset, ch),
        None => match line.char_indices().next_back() {
            Some(last) => last,
            None => return offset..offset,
        },
    };
    let (at, ch) = hit;
    if ch.is_whitespace() {
        return run(line, at, |c| c.is_whitespace());
    }
    if phrase::is_japanese(ch) {
        return phrase_range(line, at);
    }
    // UAX #29 word segments; the segment holding the character.
    let (from, segment) = line
        .split_word_bound_indices()
        .find(|(from, segment)| (*from..from + segment.len()).contains(&at))
        .unwrap_or((at, ""));
    if segment.chars().any(char::is_alphanumeric) {
        return from..from + segment.len();
    }
    run(line, at, is_symbol)
}

fn is_symbol(c: char) -> bool {
    !c.is_whitespace() && !c.is_alphanumeric() && !phrase::is_japanese(c)
}

/// The maximal run of characters satisfying `member` around the one at `at`.
fn run(line: &str, at: usize, member: fn(char) -> bool) -> Range<usize> {
    let before = line[..at].trim_end_matches(member).len();
    let after = at + line[at..].find(|c| !member(c)).unwrap_or(line.len() - at);
    before..after
}

/// The BudouX phrase holding the Japanese character at `at`.
fn phrase_range(line: &str, at: usize) -> Range<usize> {
    let span = run(line, at, phrase::is_japanese);
    let local = at - span.start;
    let boundaries = phrase::boundaries(&line[span.clone()]);
    let from = boundaries
        .iter()
        .rev()
        .find(|&&b| b <= local)
        .copied()
        .unwrap_or(0);
    let to = boundaries
        .iter()
        .find(|&&b| b > local)
        .copied()
        .unwrap_or(span.len());
    span.start + from..span.start + to
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, offset: usize) -> &str {
        &text[range(text, offset, SelectUnit::Word)]
    }

    #[test]
    fn japanese_selects_the_budoux_phrase() {
        let text = "接続診断をオンにする";
        let parts = {
            let mut parts = Vec::new();
            let mut from = 0;
            for at in phrase::boundaries(text).into_iter().chain([text.len()]) {
                parts.push(&text[from..at]);
                from = at;
            }
            parts
        };
        assert!(parts.len() > 1, "{parts:?}");
        let mut from = 0;
        for part in &parts {
            for (i, _) in part.char_indices() {
                assert_eq!(word(text, from + i), *part);
            }
            from += part.len();
        }
        assert_eq!(parts[0], "接続診断を");
    }

    #[test]
    fn english_selects_the_word() {
        let text = "hello world, it's 3.14";
        assert_eq!(word(text, 0), "hello");
        assert_eq!(word(text, 3), "hello");
        assert_eq!(word(text, 7), "world");
        assert_eq!(word(text, text.find("it's").unwrap() + 3), "it's");
        assert_eq!(word(text, text.len() - 1), "3.14");
    }

    #[test]
    fn mixed_text_uses_the_rule_of_the_clicked_character() {
        let text = "abc接続診断をdef";
        assert_eq!(word(text, 1), "abc");
        assert_eq!(word(text, 3), "接続診断を");
        assert_eq!(word(text, text.len() - 1), "def");
    }

    #[test]
    fn white_space_selects_its_run() {
        let text = "a   b";
        assert_eq!(range(text, 2, SelectUnit::Word), 1..4);
        assert_eq!(range(text, 1, SelectUnit::Word), 1..4);
        assert_eq!(range("a b", 1, SelectUnit::Word), 1..2);
    }

    #[test]
    fn symbols_select_the_adjacent_symbols() {
        let text = "a://b !!x";
        assert_eq!(word(text, 1), "://");
        assert_eq!(word(text, 2), "://");
        assert_eq!(word(text, 6), "!!");
        assert_eq!(word("(ok)", 0), "(");
        assert_eq!(word("、。", 0), "、。");
    }

    #[test]
    fn ends_and_empty_text() {
        assert_eq!(range("", 0, SelectUnit::Word), 0..0);
        assert_eq!(range("", 5, SelectUnit::Line), 0..0);
        assert_eq!(word("hello", 5), "hello");
        assert_eq!(word("hello", 99), "hello");
        assert_eq!(word("hello world", 0), "hello");
        assert_eq!(word("ab ", 3), " ");
        assert_eq!(word("今日は", 9), "今日は");
    }

    #[test]
    fn offset_inside_a_character_is_floored() {
        assert_eq!(word("aあb", 2), "あ");
    }

    #[test]
    fn line_breaks_end_every_unit() {
        let text = "one two\nthree";
        assert_eq!(word(text, 7), "two");
        assert_eq!(word(text, 8), "three");
        assert_eq!(range(text, 2, SelectUnit::Line), 0..7);
        assert_eq!(range(text, 9, SelectUnit::Line), 8..13);
        assert_eq!(range("\n", 0, SelectUnit::Word), 0..0);
    }
}
