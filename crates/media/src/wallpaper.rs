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

/// How strongly the picture is filtered (#950).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Filter {
    /// Blur radius (sigma) in pixels of the kept size; 0 is no blur.
    pub blur: u8,
    /// How far every pixel moves toward the background color, in percent
    /// (capped at 100); 0 leaves the colors alone.
    pub tint_percent: u8,
}

/// Reads `path` and returns the filtered wallpaper, tinted toward `tint`
/// (`0xRRGGBB`, the log's background color). Blocking; run it on a
/// background thread.
pub fn load(
    path: &Path,
    tint: u32,
    filter: Filter,
    cancel: &CancelFlag,
) -> Result<Thumbnail, LoadError> {
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
    apply(&mut thumbnail, tint, filter);
    Ok(thumbnail)
}

/// Blurs `thumbnail` and moves its colors toward `tint`.
fn apply(thumbnail: &mut Thumbnail, tint: u32, filter: Filter) {
    let pixels = std::mem::take(&mut thumbnail.bgra);
    let mut pixels = if filter.blur == 0 {
        pixels
    } else {
        let image = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, pixels)
            .expect("thumbnail buffer matches its size");
        // The channels are blurred alike, whatever their order.
        image::imageops::blur(&image, f32::from(filter.blur)).into_raw()
    };
    let percent = u32::from(filter.tint_percent.min(100));
    let target = [tint & 0xFF, (tint >> 8) & 0xFF, (tint >> 16) & 0xFF];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        for (channel, target) in pixel.iter_mut().take(3).zip(target) {
            let mixed = u32::from(*channel) * (100 - percent) + target * percent;
            *channel = ((mixed + 50) / 100) as u8;
        }
        pixel[3] = 255;
    }
    thumbnail.bgra = pixels;
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILTER: Filter = Filter {
        blur: 3,
        tint_percent: 30,
    };

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
        let filter = Filter {
            blur: 8,
            tint_percent: 60,
        };
        apply(&mut image, 0x102030, filter);
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

    fn filtered(blur: u8, tint_percent: u8) -> Vec<u8> {
        let mut image = checkerboard(32);
        apply(&mut image, 0x102030, Filter { blur, tint_percent });
        image.bgra
    }

    #[test]
    fn no_blur_and_no_tint_leave_the_picture_as_it_was() {
        assert_eq!(filtered(0, 0), checkerboard(32).bgra);
    }

    #[test]
    fn blur_alone_smooths_and_tint_alone_shifts_toward_the_color() {
        let blurred = filtered(3, 0);
        assert_ne!(blurred, checkerboard(32).bgra);
        // Black and white squares average out toward gray.
        let middle = blurred[(16 * 32 + 16) * 4];
        assert!((60..200).contains(&middle), "{middle}");
        // Without blur a white pixel moves 30 % of the way to the tint
        // (blue 0x30): 255 * 0.7 + 48 * 0.3 = 193.
        let tinted = filtered(0, 30);
        assert_eq!(tinted[0], 193);
        assert_eq!(filtered(0, 100)[0], 0x30);
    }

    #[test]
    fn stronger_settings_change_the_result_further() {
        let spread = |bgra: &[u8]| {
            let (low, high) = bgra.chunks(4).fold((255u8, 0u8), |(low, high), p| {
                (low.min(p[0]), high.max(p[0]))
            });
            high - low
        };
        assert!(spread(&filtered(6, 0)) < spread(&filtered(1, 0)));
        assert!(spread(&filtered(0, 60)) < spread(&filtered(0, 30)));
    }

    #[test]
    fn a_missing_or_foreign_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("cayenchat-wall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cancel = CancelFlag::default();
        assert!(load(&dir.join("absent.png"), 0, FILTER, &cancel).is_err());
        let text = dir.join("text.png");
        std::fs::write(&text, b"not an image").unwrap();
        assert_eq!(
            load(&text, 0, FILTER, &cancel).unwrap_err(),
            LoadError::NotImage
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
