#[cfg(any(target_os = "windows", test))]
const MAX_CLIPBOARD_STRING_BYTES: usize = 16 * 1024 * 1024;

#[cfg(any(target_os = "windows", test))]
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

#[cfg(any(target_os = "windows", test))]
pub(crate) fn with_locked_clipboard_data<T>(
    data_ptr: *mut std::ffi::c_void,
    size: usize,
    read: impl FnOnce(*mut std::ffi::c_void, usize) -> T,
) -> Option<T> {
    if data_ptr.is_null() || size == 0 {
        return None;
    }

    Some(read(data_ptr, size))
}

#[cfg(test)]
mod tests {
    use super::{decode_clipboard_utf16, with_locked_clipboard_data};
    use std::{cell::Cell, ffi::c_void};

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

    #[test]
    fn failed_global_lock_returns_none_without_reading_data() {
        let read_called = Cell::new(false);
        let result = with_locked_clipboard_data(std::ptr::null_mut(), 2, |_, _| {
            read_called.set(true);
        });

        assert_eq!(result, None);
        assert!(!read_called.get());
    }

    #[test]
    fn rejects_zero_sized_global_data_without_reading_it() {
        let read_called = Cell::new(false);
        let result = with_locked_clipboard_data(
            std::ptr::NonNull::<c_void>::dangling().as_ptr(),
            0,
            |_, _| {
                read_called.set(true);
            },
        );

        assert_eq!(result, None);
        assert!(!read_called.get());
    }
}
