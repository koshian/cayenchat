//! Lenient COMPOUND_TEXT decoding for text received from an input method.
//!
//! `xim_ctext::compound_text_to_utf8` only accepts a single UTF-8 or JIS segment and
//! the client used to `expect` it to succeed. IBus (Mozc) sends mixed strings such
//! as ASCII followed by a UTF-8 or JIS X 0208 segment, which made the whole
//! application panic. This decoder never fails: unsupported segments are dropped.

use alloc::string::String;
use alloc::vec::Vec;

const ESC: u8 = 0x1B;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Single,
    Utf8,
    Jis,
    Skip,
}

pub(crate) fn decode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut mode = Mode::Single;
    let mut latin1_right = false;
    let mut segment: Vec<u8> = Vec::new();
    let mut i = 0;

    while i <= bytes.len() {
        let at_escape = i < bytes.len() && bytes[i] == ESC;
        if i == bytes.len() || at_escape {
            flush(&mut out, mode, &mut segment);
            if !at_escape {
                break;
            }
            // ESC, intermediate bytes (0x20..=0x2F), final byte (0x30..=0x7E).
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (0x20..=0x2F).contains(&bytes[end]) {
                end += 1;
            }
            let intermediates = &bytes[start..end];
            let final_byte = bytes.get(end).copied();
            i = (end + 1).min(bytes.len());
            match (intermediates, final_byte) {
                (b"%", Some(b'G')) => mode = Mode::Utf8,
                (b"%", Some(b'@')) => mode = Mode::Single,
                (b"(", Some(b'B' | b'J')) => mode = Mode::Single,
                (b"$(" | b"$", Some(b'B')) => mode = Mode::Jis,
                (b"-", Some(b'A')) => latin1_right = true,
                (b"$(" | b"$)" | b"$", _) => mode = Mode::Skip,
                _ => {}
            }
            continue;
        }

        let byte = bytes[i];
        i += 1;
        match mode {
            Mode::Skip => {}
            Mode::Utf8 | Mode::Jis => segment.push(byte),
            Mode::Single => {
                if byte < 0x80 || latin1_right {
                    out.push(char::from(byte));
                }
            }
        }
    }
    out
}

fn flush(out: &mut String, mode: Mode, segment: &mut Vec<u8>) {
    if segment.is_empty() {
        return;
    }
    match mode {
        Mode::Utf8 => out.push_str(&String::from_utf8_lossy(segment)),
        Mode::Jis => {
            let mut jis = Vec::with_capacity(segment.len() + 4);
            jis.extend_from_slice(&[ESC, b'$', b'(', b'B']);
            jis.extend_from_slice(segment);
            if let Ok(text) = xim_ctext::compound_text_to_utf8(&jis) {
                out.push_str(&text);
            }
        }
        Mode::Single | Mode::Skip => {}
    }
    segment.clear();
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn plain_and_utf8() {
        assert_eq!(decode(b"abc"), "abc");
        assert_eq!(decode(b""), "");
        assert_eq!(decode(b"\x1b%G\xe3\x81\xa6\x1b%@"), "て");
    }

    #[test]
    fn mixed_ascii_and_utf8() {
        assert_eq!(decode(b"\x1b%G\xe3\x81\xa6\x1b%@t"), "てt");
        assert_eq!(decode(b"t\x1b%G\xe3\x81\xa6"), "tて");
        assert_eq!(decode(b"\x1b(B\x1b%G\xe3\x81\xa6\x1b%@\x1b(Bt"), "てt");
    }

    #[test]
    fn jis_x0208() {
        // "あ" is 0x2422 in JIS X 0208.
        assert_eq!(decode(b"\x1b$(B\x24\x22\x1b(Bt"), "あt");
    }

    #[test]
    fn malformed_input_does_not_panic() {
        decode(b"\x1b");
        decode(b"\x1b%");
        decode(b"\x1b$(");
        decode(b"\x1b%G\xff\xfe");
        decode(b"\x1b$(A\x30\x30abc");
    }
}
