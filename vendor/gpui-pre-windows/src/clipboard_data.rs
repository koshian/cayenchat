//! Decoding of clipboard text. Kept free of the `windows` crate so CayenChat's
//! workspace tests can include this file and run its regressions on any OS.

/// The longest clipboard string accepted, in bytes (CayenChat #684).
const MAX_CLIPBOARD_STRING_BYTES: usize = 16 * 1024 * 1024;

/// Decodes a `CF_UNICODETEXT`-style buffer. Returns `None` for an empty or
/// odd-sized buffer, one over the size limit, or one without a UTF-16 NUL
/// inside it, rather than reading text the owner never terminated.
pub(crate) fn decode_clipboard_utf16(buffer: &[u8]) -> Option<String> {
    if buffer.is_empty()
        || buffer.len() > MAX_CLIPBOARD_STRING_BYTES
        || !buffer.len().is_multiple_of(2)
    {
        return None;
    }

    let units = buffer
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_ne_bytes(*bytes));
    let end = units.clone().position(|unit| unit == 0)?;
    Some(String::from_utf16_lossy(
        &units.take(end).collect::<Vec<_>>(),
    ))
}

#[cfg(test)]
mod tests {
    use super::decode_clipboard_utf16;

    fn utf16_bytes(value: &[u16]) -> Vec<u8> {
        value.iter().flat_map(|unit| unit.to_ne_bytes()).collect()
    }

    #[test]
    fn decodes_only_through_a_bounded_terminator() {
        assert_eq!(
            decode_clipboard_utf16(&utf16_bytes(&[b'h' as u16, 0])),
            Some("h".into())
        );
        assert_eq!(
            decode_clipboard_utf16(&utf16_bytes(&[0])),
            Some(String::new())
        );
        assert_eq!(
            decode_clipboard_utf16(&utf16_bytes(&[0xD83D, 0xDE00, 0])),
            Some("😀".into())
        );
    }

    #[test]
    fn rejects_empty_odd_unterminated_and_oversized_buffers() {
        assert_eq!(decode_clipboard_utf16(&[]), None);
        assert_eq!(decode_clipboard_utf16(b"a"), None);
        assert_eq!(decode_clipboard_utf16(&utf16_bytes(&[b'a' as u16])), None);
        assert_eq!(decode_clipboard_utf16(&vec![0; 16 * 1024 * 1024 + 2]), None);
    }
}
