// SPDX-License-Identifier: MPL-2.0
//! Hand-built BMP, TIFF and ICO inputs for tests, including hostile ones
//! whose headers declare far more than the data holds. Shared by the unit
//! tests and `tests/decode_allocations.rs` (through `#[path]`).

#![allow(dead_code)]

/// A 24-bit BMP whose header declares `width` × `height` and whose pixel
/// data is only `data_len` bytes.
pub fn bmp_declaring(width: i32, height: i32, data_len: usize) -> Vec<u8> {
    let mut bytes = b"BM".to_vec();
    bytes.extend_from_slice(&(54 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&54u32.to_le_bytes());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&24u16.to_le_bytes());
    bytes.extend_from_slice(&[0; 24]);
    bytes.resize(54 + data_len, 0x80);
    bytes
}

/// A BMP with 8-bit run-length compression (`compression` 1) declaring
/// `width` × `height`, with an all-zero palette and a short run stream.
pub fn bmp_rle8(width: i32, height: i32) -> Vec<u8> {
    let mut bytes = b"BM".to_vec();
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&(54u32 + 1024).to_le_bytes());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8u16.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 20]);
    bytes.resize(54 + 1024, 0);
    // One absolute run, then end of bitmap.
    bytes.extend_from_slice(&[0, 3, 1, 2, 3, 0, 0, 1]);
    bytes
}

/// One IFD entry: tag, field type (3 SHORT, 4 LONG), count and the value or
/// offset, little-endian.
fn tiff_entry(tag: u16, kind: u16, count: u32, value: u32) -> [u8; 12] {
    let mut entry = [0; 12];
    entry[..2].copy_from_slice(&tag.to_le_bytes());
    entry[2..4].copy_from_slice(&kind.to_le_bytes());
    entry[4..8].copy_from_slice(&count.to_le_bytes());
    entry[8..12].copy_from_slice(&value.to_le_bytes());
    entry
}

/// A single-strip 8-bit grayscale TIFF declaring `width` × `height` with
/// `data_len` bytes of uncompressed strip data (`compression` 1 is none, 5 is
/// LZW). `strip_bytes` is the byte count the strip tag claims.
pub fn tiff_gray(
    width: u32,
    height: u32,
    compression: u16,
    data_len: usize,
    strip_bytes: u32,
) -> Vec<u8> {
    let entries = [
        tiff_entry(256, 4, 1, width),
        tiff_entry(257, 4, 1, height),
        tiff_entry(258, 3, 1, 8),
        tiff_entry(259, 3, 1, u32::from(compression)),
        tiff_entry(262, 3, 1, 1),
        tiff_entry(273, 4, 1, 8 + 2 + 9 * 12 + 4),
        tiff_entry(277, 3, 1, 1),
        tiff_entry(278, 4, 1, height),
        tiff_entry(279, 4, 1, strip_bytes),
    ];
    let mut bytes = b"II*\0\x08\0\0\0".to_vec();
    bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in entries {
        bytes.extend_from_slice(&entry);
    }
    bytes.extend_from_slice(&[0; 4]);
    bytes.resize(bytes.len() + data_len, 0x40);
    bytes
}

/// A TIFF whose first entry is a tag of `count` values stored out of line at
/// an offset past the end of the file, to see how metadata is bounded.
pub fn tiff_with_huge_tag(count: u32) -> Vec<u8> {
    let mut bytes = tiff_gray(4, 4, 1, 16, 16);
    // Replace the BitsPerSample entry (third) with `count` LONGs elsewhere.
    let at = 8 + 2 + 2 * 12;
    bytes[at..at + 12].copy_from_slice(&tiff_entry(258, 4, count, 0x00ff_0000));
    bytes
}

/// An ICO with one entry per `(width byte, height byte, data)`; 0 means 256.
pub fn ico_of(entries: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 1, 0];
    bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len();
    for (width, height, data) in entries {
        bytes.extend_from_slice(&[*width, *height, 0, 0, 1, 0, 32, 0]);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, _, data) in entries {
        bytes.extend_from_slice(data);
    }
    bytes
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A well-formed PNG (so it gets past chunk checks) whose IHDR is rewritten,
/// with a valid CRC, to declare `width` × `height`; the data stays tiny.
pub fn png_declaring(mut png: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    png
}
