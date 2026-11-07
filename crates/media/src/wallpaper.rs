// SPDX-License-Identifier: MPL-2.0
//! Channel log wallpapers (#928): a local image file made safe to lay behind
//! log text. Any photo must leave the text readable, so the image is blurred
//! (fine detail is lost) and pulled toward the log's background color
//! (contrast is lost). This is done once, when the file is loaded; the
//! caller keeps the result and the UI only scales and fades it.

use std::{fs::File, io::Read, path::Path};

use crate::{CancelFlag, Limits, LoadError, Thumbnail, decode};

/// Files larger than this are refused without reading further.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The wallpaper is kept at most this many pixels on a side; it is drawn
/// blurred and stretched, so more would only cost memory.
const MAX_SIDE: u32 = 640;
/// Blur radius (sigma) in pixels of the kept size.
const BLUR_SIGMA: f32 = 8.0;
/// How far every pixel moves toward the background color, in percent.
const TINT_PERCENT: u32 = 60;

/// Reads `path` and returns the filtered wallpaper, tinted toward `tint`
/// (`0xRRGGBB`, the log's background color). Blocking; run it on a
/// background thread.
pub fn load(path: &Path, tint: u32, cancel: &CancelFlag) -> Result<Thumbnail, LoadError> {
    let file = File::open(path).map_err(|_| LoadError::Network)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LoadError::Malformed)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge);
    }
    let limits = Limits {
        thumbnail_width: MAX_SIDE,
        thumbnail_height: MAX_SIDE,
        ..Limits::default()
    };
    let mut thumbnail = decode::thumbnail(&bytes, &limits, cancel)?;
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    filter(&mut thumbnail, tint);
    Ok(thumbnail)
}

/// Checks that `bytes` are a wallpaper image this app can draw (PNG, JPEG,
/// GIF or WebP within the decoder's size limits) and returns the file name
/// extension for its format. Decodes it, so run it off the UI thread.
pub fn validate(bytes: &[u8]) -> Result<&'static str, LoadError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge);
    }
    let extension = match decode::format(bytes)? {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Gif => "gif",
        _ => "webp",
    };
    let limits = Limits {
        thumbnail_width: MAX_SIDE,
        thumbnail_height: MAX_SIDE,
        ..Limits::default()
    };
    decode::thumbnail(bytes, &limits, &CancelFlag::default())?;
    Ok(extension)
}

/// Blurs `thumbnail` and moves its colors toward `tint`.
fn filter(thumbnail: &mut Thumbnail, tint: u32) {
    let pixels = std::mem::take(&mut thumbnail.bgra);
    let image = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, pixels)
        .expect("thumbnail buffer matches its size");
    // The channels are blurred alike, whatever their order.
    let mut pixels = image::imageops::blur(&image, BLUR_SIGMA).into_raw();
    let target = [tint & 0xFF, (tint >> 8) & 0xFF, (tint >> 16) & 0xFF];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        for (channel, target) in pixel.iter_mut().take(3).zip(target) {
            let mixed = u32::from(*channel) * (100 - TINT_PERCENT) + target * TINT_PERCENT;
            *channel = ((mixed + 50) / 100) as u8;
        }
        pixel[3] = 255;
    }
    thumbnail.bgra = pixels;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkerboard(side: u32) -> Thumbnail {
        let mut bgra = Vec::new();
        for y in 0..side {
            for x in 0..side {
                let value = if (x + y) % 2 == 0 { 255 } else { 0 };
                bgra.extend_from_slice(&[value, value, value, 255]);
            }
        }
        Thumbnail {
            width: side,
            height: side,
            source_width: side,
            source_height: side,
            bgra,
        }
    }

    #[test]
    fn filter_flattens_contrast_toward_the_tint() {
        let mut image = checkerboard(32);
        filter(&mut image, 0x102030);
        let (low, high) = image.bgra.chunks(4).fold((255u8, 0u8), |(low, high), p| {
            (low.min(p[0]), high.max(p[0]))
        });
        // The full black-to-white swing shrinks to under half, and the image
        // stays opaque.
        assert!(high - low < 128, "{low}..{high}");
        assert!(image.bgra.chunks(4).all(|p| p[3] == 255));
        let middle = &image.bgra[(16 * 32 + 16) * 4..][..3];
        assert!(middle[0] > 0x30 && middle[2] < 0x80, "{middle:?}");
    }

    #[test]
    fn validate_names_the_format_and_refuses_what_is_not_an_image() {
        let mut png = Vec::new();
        image::RgbaImage::new(4, 4)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(validate(&png), Ok("png"));
        assert_eq!(validate(b"not an image"), Err(LoadError::NotImage));
        // A cut-off file is malformed, not accepted on its header alone.
        assert!(validate(&png[..png.len() / 2]).is_err());
        let huge = vec![0u8; MAX_FILE_BYTES as usize + 1];
        assert_eq!(validate(&huge), Err(LoadError::TooLarge));
    }

    #[test]
    fn a_missing_or_foreign_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("cayenchat-wall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cancel = CancelFlag::default();
        assert!(load(&dir.join("absent.png"), 0, &cancel).is_err());
        let text = dir.join("text.png");
        std::fs::write(&text, b"not an image").unwrap();
        assert_eq!(load(&text, 0, &cancel).unwrap_err(), LoadError::NotImage);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
