//! IRC message text: formatting codes, CTCP ACTION and nickname mentions.

use cayenchat_model::names;
use std::ops::Range;

/// Removes mIRC formatting: bold, italics, underline, strikethrough,
/// monospace, reverse, reset and color codes with their `fg[,bg]` digits.
/// CTCP delimiters are dropped as well.
pub fn strip_formatting(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    each_plain_char(text, |_, ch| plain.push(ch));
    plain
}

/// Runs `find` on `text` without formatting codes (as [`strip_formatting`]
/// removes them) and returns the ranges it found as byte ranges of `text`.
pub fn plain_ranges(text: &str, find: impl FnOnce(&str) -> Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut plain = String::with_capacity(text.len());
    // The offset in `text` of each byte of `plain`.
    let mut offsets = Vec::with_capacity(text.len());
    each_plain_char(text, |offset, ch| {
        plain.push(ch);
        offsets.extend(offset..offset + ch.len_utf8());
    });
    find(&plain)
        .into_iter()
        .filter(|range| !range.is_empty())
        .map(|range| offsets[range.start]..offsets[range.end - 1] + 1)
        .collect()
}

/// Calls `keep` with the offset and character of everything in `text` but
/// formatting codes.
fn each_plain_char(text: &str, mut keep: impl FnMut(usize, char)) {
    fn digits(chars: &mut std::iter::Peekable<std::str::CharIndices>) -> usize {
        let mut count = 0;
        while count < 2 && chars.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) {
            chars.next();
            count += 1;
        }
        count
    }
    let mut chars = text.char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '\u{1}' | '\u{2}' | '\u{f}' | '\u{11}' | '\u{16}' | '\u{1d}' | '\u{1e}' | '\u{1f}' => {}
            '\u{3}' => {
                if digits(&mut chars) > 0 && chars.peek().is_some_and(|(_, ch)| *ch == ',') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) {
                        chars.next();
                        digits(&mut chars);
                    }
                }
            }
            ch => keep(offset, ch),
        }
    }
}

/// The action text of a CTCP ACTION (`/me`) message.
pub fn action_text(text: &str) -> Option<&str> {
    text.strip_prefix("\u{1}ACTION ")
        .map(|action| action.strip_suffix('\u{1}').unwrap_or(action))
}

/// Byte ranges of the whole-word occurrences of `nickname` in `text`.
/// Neighbouring nickname characters (letters, digits and `-_[]\`^{}|`) make
/// it part of another word, so `bob` does not match `bobby`, while `@bob` and
/// `bob:` match.
pub fn mention_ranges(text: &str, nickname: &str) -> Vec<Range<usize>> {
    if nickname.is_empty() {
        return Vec::new();
    }
    let lower = names::fold(text);
    let nickname = names::fold(nickname);
    lower
        .match_indices(&nickname)
        .map(|(start, found)| start..start + found.len())
        .filter(|range| {
            let before = lower[..range.start].chars().next_back();
            let after = lower[range.end..].chars().next();
            !before.is_some_and(is_nick_char) && !after.is_some_and(is_nick_char)
        })
        .collect()
}

/// Nickname equality under RFC 1459 case mapping ([`names::same`]).
pub fn same_nickname(left: &str, right: &str) -> bool {
    names::same(left, right)
}

fn is_nick_char(ch: char) -> bool {
    ch.is_alphanumeric() || "-_[]\\`^{}|".contains(ch)
}

/// A nickname folded for comparison (RFC 1459 case mapping), usable as a
/// map key.
pub fn nickname_key(nickname: &str) -> String {
    names::fold(nickname)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nickname_mentions_need_word_boundaries() {
        let mentions = |text, nickname| !mention_ranges(text, nickname).is_empty();
        assert!(mentions("bob: hi", "Bob"));
        assert!(mentions("hi, @bob!", "bob"));
        assert!(mentions("おはよう bob さん", "bob"));
        assert!(mentions("{away}", "[away]"));
        assert!(!mentions("bobby: hi", "bob"));
        assert!(!mentions("bob_: hi", "bob"));
        assert!(!mentions("anything", ""));
        assert!(same_nickname("Nick[a]", "nick{A}"));
        assert_eq!(mention_ranges("Bob, bobby and @bob", "bob"), [0..3, 16..19]);
    }

    #[test]
    fn formatting_and_actions_are_removed() {
        assert_eq!(
            strip_formatting("\u{3}04,01red\u{3} \u{2}bold\u{f}"),
            "red bold"
        );
        assert_eq!(strip_formatting("\u{3}12,x"), ",x");
        assert_eq!(action_text("\u{1}ACTION waves\u{1}"), Some("waves"));
        assert_eq!(action_text("waves"), None);
    }

    #[test]
    fn plain_ranges_map_back_past_formatting() {
        // A colored nickname is a mention, and the range covers it in the
        // original text.
        let text = "\u{3}04,01bob\u{3}: héllo \u{2}bob\u{2}";
        let ranges = plain_ranges(text, |plain| mention_ranges(plain, "bob"));
        assert_eq!(ranges, [6..9, 20..23]);
        assert!(ranges.iter().all(|range| &text[range.clone()] == "bob"));
        // Empty ranges are dropped; a multibyte character maps whole.
        let found = plain_ranges("a\u{2}éb", |_| vec![1..3, 2..2]);
        assert_eq!(
            found.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(),
            [(2, 4)]
        );
    }
}
