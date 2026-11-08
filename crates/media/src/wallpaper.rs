// SPDX-License-Identifier: MPL-2.0
//! Channel log wallpapers (#928): a local image file made ready to lay behind
//! log text. The image is reduced and blurred (fine detail is lost); how much
//! of it shows is the UI's business (the log background color is laid over it
//! with an opacity, #950), so a change of color or opacity never loads again.
//! The blur is applied once, when the file is loaded; the caller keeps the
//! result and the UI only scales it.

use std::{fs::File, io::Read, path::Path};

use crate::{CancelFlag, Limits, LoadError, Thumbnail, decode};

/// Files larger than this are refused without reading further.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The wallpaper is kept at most this many pixels on a side; it is drawn
/// blurred and scaled, so more would only cost memory.
const MAX_SIDE: u32 = 640;

/// Reads `path` and returns the wallpaper reduced to at most 640 px a side
/// and blurred with `blur` (tenths of a sigma in pixels of that size; 0 is
/// no blur). Blocking; run it on a background thread.
pub fn load(path: &Path, blur: u8, cancel: &CancelFlag) -> Result<Thumbnail, LoadError> {
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
    apply(&mut thumbnail, blur);
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

/// Blurs `thumbnail` (`blur` in tenths of a sigma) and makes it opaque.
fn apply(thumbnail: &mut Thumbnail, blur: u8) {
    let pixels = std::mem::take(&mut thumbnail.bgra);
    let mut pixels = if blur == 0 {
        pixels
    } else {
        let image = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, pixels)
            .expect("thumbnail buffer matches its size");
        // The channels are blurred alike, whatever their order.
        image::imageops::blur(&image, f32::from(blur) / 10.0).into_raw()
    };
    for pixel in pixels.as_chunks_mut::<4>().0 {
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

    fn blurred(blur: u8) -> Vec<u8> {
        let mut image = checkerboard(32);
        apply(&mut image, blur);
        image.bgra
    }

    fn spread(bgra: &[u8]) -> u8 {
        let (low, high) = bgra.chunks(4).fold((255u8, 0u8), |(low, high), p| {
            (low.min(p[0]), high.max(p[0]))
        });
        high - low
    }

    #[test]
    fn no_blur_leaves_the_picture_as_it_was() {
        assert_eq!(blurred(0), checkerboard(32).bgra);
    }

    #[test]
    fn blur_smooths_the_picture_and_stays_opaque() {
        let result = blurred(30);
        assert_ne!(result, checkerboard(32).bgra);
        assert!(result.chunks(4).all(|p| p[3] == 255));
        // Black and white squares average out toward gray.
        let middle = result[(16 * 32 + 16) * 4];
        assert!((60..200).contains(&middle), "{middle}");
    }

    #[test]
    fn a_stronger_blur_changes_the_result_further() {
        assert!(spread(&blurred(30)) < spread(&blurred(10)));
        // A tenth steps are felt: a small blur still differs from none.
        assert_ne!(blurred(5), blurred(0));
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
        assert!(load(&dir.join("absent.png"), 3, &cancel).is_err());
        let text = dir.join("text.png");
        std::fs::write(&text, b"not an image").unwrap();
        assert_eq!(load(&text, 3, &cancel).unwrap_err(), LoadError::NotImage);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
