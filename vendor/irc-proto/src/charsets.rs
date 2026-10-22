//! Per-target character encodings for the text of `PRIVMSG` and `NOTICE`.
//!
//! A connection decodes and encodes whole lines with one encoding. When a
//! [`BodyCharsets`] is given to the codec, the text of a `PRIVMSG` or
//! `NOTICE` whose target it knows is converted with the encoding it names
//! instead, while everything in front of the text (tags, prefix, command and
//! the target itself) keeps the connection's encoding, so a target means the
//! same bytes whatever its text uses.

use std::{borrow::Cow, fmt, sync::Arc};

use encoding_rs::Encoding;

/// Chooses the encoding of message text by the message's target.
pub trait BodyCharsets: Send + Sync {
    /// Whether no target has its own encoding. Checked first on every line,
    /// so it must be cheap.
    fn is_empty(&self) -> bool;
    /// The encoding of text received in `target`, if it differs from the
    /// connection's.
    fn decoder(&self, target: &str) -> Option<&'static Encoding>;
    /// The encoding of text sent to `target`, if it differs from the
    /// connection's.
    fn encoder(&self, target: &str) -> Option<&'static Encoding>;
    /// Takes the encoding fixed for exactly this `line` when it was queued
    /// (`Some(None)`: the connection's), so a change made while the line
    /// waits does not alter its bytes. Asked once for each line the codec
    /// writes, before [`BodyCharsets::encoder`] is.
    fn take_pin(&self, _line: &str) -> Option<Pin> {
        None
    }
    /// Called when the codec drops a line holding CR, LF or NUL instead of
    /// writing it. The line itself is not passed on: it may hold secrets.
    fn refused_line(&self) {}
}

/// What was fixed for a queued line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pin {
    /// Write the text in this encoding (`None`: the connection's).
    Encoding(Option<&'static Encoding>),
    /// The connection no longer allows the encoding the line was checked
    /// for: write nothing rather than bytes that were never checked.
    Skip,
}

/// An encoding fixed in advance for the text of every message to any target;
/// `None` is the connection's.
pub struct Fixed(pub Option<&'static Encoding>);

impl BodyCharsets for Fixed {
    fn is_empty(&self) -> bool {
        false
    }
    fn decoder(&self, _target: &str) -> Option<&'static Encoding> {
        None
    }
    fn encoder(&self, _target: &str) -> Option<&'static Encoding> {
        self.0
    }
}

/// A shared [`BodyCharsets`], carried by a client configuration.
#[derive(Clone)]
pub struct BodyCharsetsHandle(pub Arc<dyn BodyCharsets>);

impl fmt::Debug for BodyCharsetsHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BodyCharsetsHandle").finish_non_exhaustive()
    }
}

impl PartialEq for BodyCharsetsHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Where the target and the text of a `PRIVMSG` or `NOTICE` lie in a line.
struct TextSpan {
    target: std::ops::Range<usize>,
    /// The first byte of the text, which is where the encoding changes.
    text: usize,
}

/// Finds the target and the text of a `PRIVMSG` or `NOTICE` line by its
/// ASCII structure alone, so it works on encoded bytes. `None` for any other
/// line and for a message with no text.
fn text_span(line: &[u8]) -> Option<TextSpan> {
    fn skip_spaces(line: &[u8], mut at: usize) -> usize {
        while line.get(at) == Some(&b' ') {
            at += 1;
        }
        at
    }
    fn word_end(line: &[u8], at: usize) -> usize {
        line[at..]
            .iter()
            .position(|byte| matches!(byte, b' ' | b'\r' | b'\n'))
            .map_or(line.len(), |offset| at + offset)
    }
    let mut at = 0;
    // Tags, then the prefix, each one word.
    for marker in [b'@', b':'] {
        if line.get(at) == Some(&marker) {
            at = skip_spaces(line, word_end(line, at));
        }
    }
    let command_end = word_end(line, at);
    let command = &line[at..command_end];
    if !(command.eq_ignore_ascii_case(b"PRIVMSG") || command.eq_ignore_ascii_case(b"NOTICE")) {
        return None;
    }
    let target_start = skip_spaces(line, command_end);
    let target_end = word_end(line, target_start);
    if target_start == target_end || line.get(target_end) != Some(&b' ') {
        return None;
    }
    Some(TextSpan {
        target: target_start..target_end,
        text: skip_spaces(line, target_end),
    })
}

/// Decodes a received line (without malformed bytes failing): the connection's
/// `encoding` for the whole of it, except the text of a `PRIVMSG` or
/// `NOTICE` to a target with its own decoder.
pub fn decode_line(
    line: &[u8],
    encoding: &'static Encoding,
    charsets: Option<&dyn BodyCharsets>,
) -> String {
    if let Some(charsets) = charsets.filter(|charsets| !charsets.is_empty()) {
        if let Some(span) = text_span(line) {
            let (target, _) = encoding.decode_without_bom_handling(&line[span.target.clone()]);
            if let Some(text_encoding) = charsets.decoder(&target) {
                let (head, _) = encoding.decode_without_bom_handling(&line[..span.text]);
                let (text, _) = text_encoding.decode_without_bom_handling(&line[span.text..]);
                return format!("{head}{text}");
            }
        }
    }
    encoding.decode_without_bom_handling(line).0.into_owned()
}

/// Encodes a line to send, in the same split as [`decode_line`]. The second
/// value is the encoding that could not represent a character, if any (it
/// wrote a numeric character reference instead).
pub fn encode_line<'a>(
    line: &'a str,
    encoding: &'static Encoding,
    charsets: Option<&dyn BodyCharsets>,
) -> (Cow<'a, [u8]>, Option<&'static Encoding>) {
    if let Some(charsets) = charsets.filter(|charsets| !charsets.is_empty()) {
        if let Some(span) = text_span(line.as_bytes()) {
            if let Some(text_encoding) = charsets.encoder(&line[span.target.clone()]) {
                let (head, _, head_lost) = encoding.encode(&line[..span.text]);
                let (text, _, text_lost) = text_encoding.encode(&line[span.text..]);
                let mut bytes = head.into_owned();
                bytes.extend_from_slice(&text);
                let failed = if head_lost {
                    Some(encoding)
                } else {
                    text_lost.then_some(text_encoding)
                };
                return (Cow::Owned(bytes), failed);
            }
        }
    }
    let (bytes, _, lost) = encoding.encode(line);
    (bytes, lost.then_some(encoding))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;

    impl BodyCharsets for Fixed {
        fn is_empty(&self) -> bool {
            false
        }
        fn decoder(&self, target: &str) -> Option<&'static Encoding> {
            (target == "#jp").then_some(encoding_rs::ISO_2022_JP)
        }
        fn encoder(&self, target: &str) -> Option<&'static Encoding> {
            (target == "#jp").then_some(encoding_rs::ISO_2022_JP)
        }
    }

    const JIS: &[u8] = b"\x1b$B$3$s$K$A$O\x1b(B";

    #[test]
    fn only_the_text_of_a_message_to_a_known_target_changes_encoding() {
        let mut wire = b":nick!u@h PRIVMSG #jp :".to_vec();
        wire.extend_from_slice(JIS);
        wire.extend_from_slice(b"\r\n");
        let decoded = decode_line(&wire, encoding_rs::UTF_8, Some(&Fixed));
        assert_eq!(decoded, ":nick!u@h PRIVMSG #jp :こんにちは\r\n");
        // Another target, or no override at all: the connection's encoding.
        let other = wire.clone();
        assert_ne!(
            decode_line(&other, encoding_rs::UTF_8, None),
            ":nick!u@h PRIVMSG #jp :こんにちは\r\n"
        );
        let (encoded, lost) = encode_line(&decoded, encoding_rs::UTF_8, Some(&Fixed));
        assert_eq!(lost, None);
        assert_eq!(&*encoded, &wire[..]);
    }

    #[test]
    fn the_target_keeps_the_connection_encoding_and_tags_are_left_alone() {
        let wire = "@time=2026-01-01T00:00:00.000Z;+x=\u{3053} :n!u@h NOTICE #jp :\u{3053}\r\n";
        let (encoded, lost) = encode_line(wire, encoding_rs::UTF_8, Some(&Fixed));
        assert_eq!(lost, None);
        let tag_end = wire.find(" :n!").unwrap();
        assert_eq!(&encoded[..tag_end], wire[..tag_end].as_bytes());
        assert!(encoded.ends_with(b"\x1b$B$3\x1b(B\r\n"));
        assert_eq!(
            decode_line(&encoded, encoding_rs::UTF_8, Some(&Fixed)),
            wire
        );
    }

    #[test]
    fn other_lines_and_commands_are_not_split() {
        for line in [
            "PING :#jp\r\n",
            "JOIN #jp\r\n",
            "PRIVMSG #jp\r\n",
            "PRIVMSG\r\n",
            ":s 332 me #jp :\u{3053}\r\n",
            "PRIVMSG #other :\u{3053}\r\n",
        ] {
            let (encoded, _) = encode_line(line, encoding_rs::UTF_8, Some(&Fixed));
            assert_eq!(&*encoded, line.as_bytes(), "{line:?}");
        }
    }

    #[test]
    fn unrepresentable_text_is_reported() {
        let (_, lost) = encode_line("PRIVMSG #jp :\u{1f352}\r\n", encoding_rs::UTF_8, Some(&Fixed));
        assert_eq!(lost, Some(encoding_rs::ISO_2022_JP));
        let (_, lost) = encode_line("PRIVMSG #other :\u{1f352}\r\n", encoding_rs::SHIFT_JIS, Some(&Fixed));
        assert_eq!(lost, Some(encoding_rs::SHIFT_JIS));
    }
}
