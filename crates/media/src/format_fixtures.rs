// SPDX-License-Identifier: MPL-2.0
//! Hand-built BMP and ICO inputs for tests, including hostile ones
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

/// An ICO of `count` entries that all point at the one `data`.
pub fn ico_many(data: &[u8], count: usize) -> Vec<u8> {
    let mut bytes = vec![0, 0, 1, 0];
    bytes.extend_from_slice(&(count as u16).to_le_bytes());
    let offset = 6 + 16 * count;
    for _ in 0..count {
        bytes.extend_from_slice(&[0, 0, 0, 0, 1, 0, 32, 0]);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    }
    bytes.extend_from_slice(data);
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
