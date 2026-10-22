// SPDX-License-Identifier: MPL-2.0
//! Text from servers and other users made safe to show.

use std::borrow::Cow;

/// Bidirectional embedding, override and isolate controls (U+202A–U+202E,
/// U+2066–U+2069). They force the order in which following characters are
/// drawn, so a line could show `exe.pdf` for `fdp.exe` or make one sender's
/// text look like another's. Right-to-left scripts need none of them: the
/// Unicode bidirectional algorithm orders Arabic or Hebrew by itself. The
/// marks LRM, RLM and ALM (U+200E, U+200F, U+061C), which only influence the
/// neutral characters next to them, are kept.
pub fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `text` with every bidirectional control replaced by a zero width space
/// (U+200B), which has no effect on direction. Both are three bytes in
/// UTF-8, so byte offsets computed on the result (links, highlights,
/// selections) are the same as on the original.
pub fn neutralize_bidi(text: &str) -> Cow<'_, str> {
    if text.chars().any(is_bidi_control) {
        Cow::Owned(
            text.chars()
                .map(|ch| if is_bidi_control(ch) { '\u{200B}' } else { ch })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// [`neutralize_bidi`] for an owned string, without copying it when there is
/// nothing to replace.
pub fn neutralize_bidi_owned(text: String) -> String {
    match neutralize_bidi(&text) {
        Cow::Borrowed(_) => text,
        Cow::Owned(neutral) => neutral,
    }
}

/// Whether `ch` starts a mIRC formatting code (a color code also takes the
/// digits after it).
pub fn is_formatting_code(ch: char) -> bool {
    matches!(
        ch,
        '\u{1}'
            | '\u{2}'
            | '\u{3}'
            | '\u{f}'
            | '\u{11}'
            | '\u{16}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{1f}'
    )
}

/// Calls `keep` with the offset and character of everything in `text` but
/// formatting codes.
pub fn each_plain_char(text: &str, mut keep: impl FnMut(usize, char)) {
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
            ch if is_formatting_code(ch) => {}
            ch => keep(offset, ch),
        }
    }
}

/// Removes mIRC formatting: bold, italics, underline, strikethrough,
/// monospace, reverse, reset and color codes with their `fg[,bg]` digits.
/// CTCP delimiters are dropped as well.
pub fn strip_formatting(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    each_plain_char(text, |_, ch| plain.push(ch));
    plain
}

/// The longest title or subject handed to the operating system, in characters.
pub const OS_TITLE_LIMIT: usize = 200;
/// The longest dialog or notification body handed to the operating system,
/// in characters.
pub const OS_BODY_LIMIT: usize = 2000;

/// A window title, notification subject or dialog title made safe for the
/// operating system: one line, see [`os_text`]. Applying it again changes
/// nothing.
pub fn os_title(text: &str) -> String {
    os_text(text, false, OS_TITLE_LIMIT)
}

/// A dialog or notification body made safe for the operating system. Line
/// breaks stay (dialogs use them), see [`os_text`]. Applying it again
/// changes nothing.
pub fn os_body(text: &str) -> String {
    os_text(text, true, OS_BODY_LIMIT)
}

/// The one gate in front of every operating system surface (dialogs,
/// desktop notifications, window titles), whoever built the text:
/// 1. bidirectional controls become U+200B ([`neutralize_bidi`]);
/// 2. mIRC formatting codes are removed ([`strip_formatting`]);
/// 3. other control characters are removed, tabs, U+2028 and U+2029 become
///    spaces, and line breaks stay when `multiline` and become spaces
///    otherwise;
/// 4. more than `limit` characters are cut and end in `…` (whole characters
///    only, the result has `limit` at most).
fn os_text(text: &str, multiline: bool, limit: usize) -> String {
    let mut plain = String::with_capacity(text.len());
    each_plain_char(text, |_, ch| match ch {
        '\n' if multiline => plain.push('\n'),
        '\n' | '\t' | '\u{2028}' | '\u{2029}' => plain.push(' '),
        ch if ch.is_control() => {}
        ch if is_bidi_control(ch) => plain.push('\u{200B}'),
        ch => plain.push(ch),
    });
    match plain.char_indices().nth(limit) {
        Some(_) => {
            let end = plain.char_indices().nth(limit - 1).map_or(0, |(i, _)| i);
            plain.truncate(end);
            plain.push('…');
            plain
        }
        None => plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_text_is_cleaned_and_cut() {
        let dirty = "a\u{202E}b \u{2}bold\u{3}04,01red\u{f}\u{7}\u{9b}\tx\u{2028}y\r\nz";
        assert_eq!(os_title(dirty), "a\u{200B}b boldred x y z");
        assert_eq!(os_body(dirty), "a\u{200B}b boldred x y\nz");
        let long = "あ".repeat(5000);
        assert_eq!(os_title(&long).chars().count(), OS_TITLE_LIMIT);
        assert!(os_title(&long).ends_with('…'));
        assert_eq!(os_body(&long).chars().count(), OS_BODY_LIMIT);
        let exact = "x".repeat(OS_TITLE_LIMIT);
        assert_eq!(os_title(&exact), exact);
    }

    #[test]
    fn os_text_is_idempotent_and_keeps_ordinary_text() {
        let long = format!("\u{202E}\u{3}4{}", "é".repeat(3000));
        for text in [
            long.as_str(),
            "a\r\n\u{1}b\u{2029}",
            "日本語 hello",
            "שלום עולם",
            "مرحبا \u{200F}(1)",
        ] {
            assert_eq!(os_title(&os_title(text)), os_title(text));
            assert_eq!(os_body(&os_body(text)), os_body(text));
        }
        for text in [
            "日本語 hello",
            "שלום עולם",
            "مرحبا \u{200F}(1)",
            "a\u{200E}b",
        ] {
            assert_eq!(os_title(text), text);
            assert_eq!(os_body(text), text);
        }
        assert_eq!(os_body("one\ntwo"), "one\ntwo");
    }

    #[test]
    fn overrides_and_isolates_become_zero_width_spaces_of_the_same_length() {
        let spoofed = "invoice\u{202E}fdp.exe \u{2067}x\u{2069}";
        let shown = neutralize_bidi(spoofed);
        assert_eq!(shown, "invoice\u{200B}fdp.exe \u{200B}x\u{200B}");
        assert_eq!(shown.len(), spoofed.len());
        assert!(!shown.chars().any(is_bidi_control));
    }

    #[test]
    fn every_embedding_override_and_isolate_control_is_replaced() {
        let controls = "\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}";
        assert_eq!(controls.chars().count(), 9);
        assert_eq!(neutralize_bidi(controls), "\u{200B}".repeat(9));
    }

    #[test]
    fn right_to_left_text_and_marks_are_untouched() {
        for text in ["שלום עולם", "مرحبا \u{200F}(1)", "abc\u{200E}", "日本語"] {
            assert!(matches!(neutralize_bidi(text), Cow::Borrowed(t) if t == text));
        }
    }
}
