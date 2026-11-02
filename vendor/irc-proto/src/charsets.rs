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

/// The label of ISO-2022-JP that keeps half-width katakana (Perl's `7bit-jis`).
/// It is not a WHATWG label; see [`Charset::for_label`].
pub const KEEP_KANA_LABEL: &str = "7bit-jis";

/// An encoding to write in, and how: `keep_kana` writes half-width katakana
/// (U+FF61..=U+FF9F) of ISO-2022-JP text as they are, with `ESC ( I`, where
/// `encoding_rs` turns them into full-width ones. It has no effect on any
/// other encoding. Decoding is the encoding's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charset {
    /// The encoding the text is converted with.
    pub encoding: &'static Encoding,
    /// Whether half-width katakana are written as they are.
    pub keep_kana: bool,
}

impl Charset {
    /// A plain encoding.
    pub const fn plain(encoding: &'static Encoding) -> Self {
        Self {
            encoding,
            keep_kana: false,
        }
    }

    /// The charset a label stands for: a WHATWG label whose encoder writes
    /// the same encoding (not UTF-16 or "replacement"), or `7bit-jis`.
    pub fn for_label(label: &str) -> Option<Self> {
        if label.trim().eq_ignore_ascii_case(KEEP_KANA_LABEL) {
            return Some(Self {
                encoding: encoding_rs::ISO_2022_JP,
                keep_kana: true,
            });
        }
        Encoding::for_label(label.as_bytes())
            .filter(|encoding| encoding.output_encoding() == *encoding)
            .map(Self::plain)
    }

    /// Whether this writes ISO-2022-JP, in either policy.
    pub fn is_iso_2022_jp(self) -> bool {
        self.encoding == encoding_rs::ISO_2022_JP
    }
}

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
    fn encoder(&self, target: &str) -> Option<Charset>;
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
    Encoding(Option<Charset>),
    /// The connection no longer allows the encoding the line was checked
    /// for: write nothing rather than bytes that were never checked.
    Skip,
}

/// An encoding fixed in advance for the text of every message to any target;
/// `None` is the connection's.
pub struct Fixed(pub Option<Charset>);

impl BodyCharsets for Fixed {
    fn is_empty(&self) -> bool {
        false
    }
    fn decoder(&self, _target: &str) -> Option<&'static Encoding> {
        None
    }
    fn encoder(&self, _target: &str) -> Option<Charset> {
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

/// Encodes `text` in `charset`. With `keep_kana` in ISO-2022-JP, each run of
/// half-width katakana is written as `ESC ( I`, one byte per character
/// (`c - 0xFF61 + 0x21`), and `ESC ( B`; the rest goes through `encoding_rs`.
fn encode_text(text: &str, charset: Charset) -> (Cow<'_, [u8]>, bool) {
    fn is_kana(ch: char) -> bool {
        ('\u{ff61}'..='\u{ff9f}').contains(&ch)
    }
    if !(charset.keep_kana && charset.is_iso_2022_jp() && text.chars().any(is_kana)) {
        let (bytes, _, lost) = charset.encoding.encode(text);
        return (bytes, lost);
    }
    let mut bytes = Vec::with_capacity(text.len());
    let mut lost = false;
    let mut rest = text;
    while !rest.is_empty() {
        let kana = is_kana(rest.chars().next().unwrap_or_default());
        let end = rest.find(|ch| is_kana(ch) != kana).unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        let mut chunk = Vec::new();
        if kana {
            chunk.extend_from_slice(b"\x1b(I");
            chunk.extend(run.chars().map(|ch| (u32::from(ch) - 0xff61 + 0x21) as u8));
            chunk.extend_from_slice(b"\x1b(B");
        } else {
            let (encoded, _, run_lost) = charset.encoding.encode(run);
            chunk.extend_from_slice(&encoded);
            lost |= run_lost;
        }
        // Two escape sequences with nothing between them are an error in the
        // WHATWG decoder, so the switch back to ASCII is left out where the
        // next run switches again.
        if chunk.first() == Some(&0x1b) && bytes.ends_with(b"\x1b(B") {
            bytes.truncate(bytes.len() - 3);
        }
        bytes.extend_from_slice(&chunk);
        rest = tail;
    }
    (Cow::Owned(bytes), lost)
}

/// Encodes a line to send, in the same split as [`decode_line`]. The second
/// value is the encoding that could not represent a character, if any (it
/// wrote a numeric character reference instead). Half-width katakana are kept
/// only in the text of a `PRIVMSG` or `NOTICE`.
pub fn encode_line<'a>(
    line: &'a str,
    connection: Charset,
    charsets: Option<&dyn BodyCharsets>,
) -> (Cow<'a, [u8]>, Option<&'static Encoding>) {
    let charsets = charsets.filter(|charsets| !charsets.is_empty());
    if connection.keep_kana || charsets.is_some() {
        if let Some(span) = text_span(line.as_bytes()) {
            let own = charsets.and_then(|charsets| charsets.encoder(&line[span.target.clone()]));
            if own.is_some() || connection.keep_kana {
                let text_charset = own.unwrap_or(connection);
                let (head, _, head_lost) = connection.encoding.encode(&line[..span.text]);
                let (text, text_lost) = encode_text(&line[span.text..], text_charset);
                let mut bytes = head.into_owned();
                bytes.extend_from_slice(&text);
                let failed = if head_lost {
                    Some(connection.encoding)
                } else {
                    text_lost.then_some(text_charset.encoding)
                };
                return (Cow::Owned(bytes), failed);
            }
        }
    }
    let (bytes, _, lost) = connection.encoding.encode(line);
    (bytes, lost.then_some(connection.encoding))
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
        fn encoder(&self, target: &str) -> Option<Charset> {
            (target == "#jp").then_some(Charset::plain(encoding_rs::ISO_2022_JP))
        }
    }

    const UTF8: Charset = Charset::plain(encoding_rs::UTF_8);
    const SJIS: Charset = Charset::plain(encoding_rs::SHIFT_JIS);
    const KEEP: Charset = Charset {
        encoding: encoding_rs::ISO_2022_JP,
        keep_kana: true,
    };

    struct Keep;

    impl BodyCharsets for Keep {
        fn is_empty(&self) -> bool {
            false
        }
        fn decoder(&self, _target: &str) -> Option<&'static Encoding> {
            None
        }
        fn encoder(&self, target: &str) -> Option<Charset> {
            (target == "#keep").then_some(KEEP)
        }
    }

    fn keep(text: &str) -> Vec<u8> {
        let line = format!("PRIVMSG #a :{text}\r\n");
        let (bytes, lost) = encode_line(&line, KEEP, None);
        assert_eq!(lost, None);
        bytes.into_owned()
    }

    #[test]
    fn labels_resolve_to_one_charset() {
        assert_eq!(Charset::for_label("7bit-jis"), Some(KEEP));
        assert_eq!(Charset::for_label(" 7BIT-JIS "), Some(KEEP));
        assert_eq!(
            Charset::for_label("ISO-2022-JP"),
            Some(Charset::plain(encoding_rs::ISO_2022_JP))
        );
        assert_eq!(Charset::for_label("utf-8"), Some(UTF8));
        assert_eq!(Charset::for_label("utf-16le"), None);
        assert_eq!(Charset::for_label("nope"), None);
    }

    #[test]
    fn half_width_katakana_are_kept_in_runs() {
        // ﾉｼ: ESC ( I, 0x49 0x3C, ESC ( B.
        assert_eq!(keep("ﾉｼ"), b"PRIVMSG #a :\x1b(I\x49\x3c\x1b(B\r\n");
        // Mixed: the full-width part is the usual X0208 sequence. The switch
        // back to ASCII between two runs is left out (two escapes in a row
        // are a decoding error).
        assert_eq!(keep("ノｼ"), b"PRIVMSG #a :\x1b$B%N\x1b(I\x3c\x1b(B\r\n");
        assert_eq!(keep("ｼノ"), b"PRIVMSG #a :\x1b(I\x3c\x1b$B%N\x1b(B\r\n");
        // A run at the start and at the end, with ASCII between.
        assert_eq!(
            keep("ｱ b ｲ"),
            b"PRIVMSG #a :\x1b(I\x31\x1b(B b \x1b(I\x32\x1b(B\r\n"
        );
        // The voiced sound mark U+FF9E is 0x5E, and the range ends are kept.
        assert_eq!(keep("ｶﾞ"), b"PRIVMSG #a :\x1b(I\x36\x5e\x1b(B\r\n");
        assert_eq!(keep("\u{ff61}\u{ff9f}"), b"PRIVMSG #a :\x1b(I\x21\x5f\x1b(B\r\n");
    }

    #[test]
    fn text_without_half_width_katakana_matches_iso_2022_jp() {
        let plain = Charset::plain(encoding_rs::ISO_2022_JP);
        for text in ["", "abc", "こんにちは、世界", "ABC あいう DEF"] {
            let line = format!("PRIVMSG #a :{text}\r\n");
            assert_eq!(encode_line(&line, KEEP, None), encode_line(&line, plain, None));
        }
    }

    #[test]
    fn kept_katakana_round_trip_through_the_iso_2022_jp_decoder() {
        for text in ["ﾉｼ", "ノｼ", "ｼノ", "ｱ b ｲ", "ｶﾞｷﾞ です", "あｱいｲう"] {
            let wire = keep(text);
            let decoded = decode_line(&wire, encoding_rs::ISO_2022_JP, None);
            assert_eq!(decoded, format!("PRIVMSG #a :{text}\r\n"));
        }
    }

    #[test]
    fn katakana_are_kept_only_in_the_text_and_only_for_that_target() {
        // The target and the command keep the connection's policy.
        let line = "PRIVMSG #keep :ｱ\r\n";
        let (bytes, lost) = encode_line(line, UTF8, Some(&Keep));
        assert_eq!(lost, None);
        assert_eq!(&*bytes, b"PRIVMSG #keep :\x1b(I\x31\x1b(B\r\n");
        // Another target, and a line that is no message: unchanged.
        for line in ["PRIVMSG #other :ｱ\r\n", "TOPIC #keep :ｱ\r\n"] {
            let (bytes, _) = encode_line(line, UTF8, Some(&Keep));
            assert_eq!(&*bytes, line.as_bytes());
        }
        // Half-width katakana in a connection's non-message line are folded.
        let (bytes, _) = encode_line("TOPIC #a :ｱ\r\n", KEEP, None);
        assert_eq!(&*bytes, b"TOPIC #a :\x1b$B%\x22\x1b(B\r\n");
        // A channel can go back to folding on a keeping connection.
        let (bytes, _) = encode_line("PRIVMSG #jp :ｱ\r\n", KEEP, Some(&Fixed));
        assert_eq!(&*bytes, b"PRIVMSG #jp :\x1b$B%\x22\x1b(B\r\n");
    }

    #[test]
    fn unrepresentable_text_is_reported_with_kept_katakana() {
        let (_, lost) = encode_line("PRIVMSG #a :ｱ\u{1f352}\r\n", KEEP, None);
        assert_eq!(lost, Some(encoding_rs::ISO_2022_JP));
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
        let (encoded, lost) = encode_line(&decoded, UTF8, Some(&Fixed));
        assert_eq!(lost, None);
        assert_eq!(&*encoded, &wire[..]);
    }

    #[test]
    fn the_target_keeps_the_connection_encoding_and_tags_are_left_alone() {
        let wire = "@time=2026-01-01T00:00:00.000Z;+x=\u{3053} :n!u@h NOTICE #jp :\u{3053}\r\n";
        let (encoded, lost) = encode_line(wire, UTF8, Some(&Fixed));
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
            let (encoded, _) = encode_line(line, UTF8, Some(&Fixed));
            assert_eq!(&*encoded, line.as_bytes(), "{line:?}");
        }
    }

    #[test]
    fn unrepresentable_text_is_reported() {
        let (_, lost) = encode_line("PRIVMSG #jp :\u{1f352}\r\n", UTF8, Some(&Fixed));
        assert_eq!(lost, Some(encoding_rs::ISO_2022_JP));
        let (_, lost) = encode_line("PRIVMSG #other :\u{1f352}\r\n", SJIS, Some(&Fixed));
        assert_eq!(lost, Some(encoding_rs::SHIFT_JIS));
    }
}
