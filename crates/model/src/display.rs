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

#[cfg(test)]
mod tests {
    use super::*;

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
