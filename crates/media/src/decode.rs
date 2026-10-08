//! Turns downloaded bytes into a small static thumbnail.
//!
//! The format comes from the content (`ImageFormat::sniff`), never from the
//! URL or the declared type. Only PNG, JPEG, GIF and WebP are decoded; GIF
//! and animated WebP show their first frame. Dimensions are read from the
//! header and checked before any pixel buffer is allocated, the decoder's
//! allocation is capped, and one decode runs at a time in the whole process,
//! so the full-size image exists only briefly and only once.

use std::{io::Cursor, sync::Mutex};

use cayenchat_model::attachment::ImageFormat as Sniffed;
use image::{DynamicImage, ImageFormat, ImageReader};

use crate::{CancelFlag, Limits, LoadError};

/// Serializes decoding so full-size images never coexist.
pub(crate) static DECODE: Mutex<()> = Mutex::new(());

/// A decoded thumbnail in GPUI's BGRA byte order, not premultiplied.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Size of the original image.
    pub source_width: u32,
    pub source_height: u32,
    pub bgra: Vec<u8>,
}

impl Thumbnail {
    /// Bytes of pixel data held by this thumbnail.
    pub fn byte_len(&self) -> usize {
        self.bgra.len()
    }
}

impl std::fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source_width", &self.source_width)
            .field("source_height", &self.source_height)
            .finish()
    }
}

/// The supported format of `bytes`, from its leading bytes.
pub fn format(bytes: &[u8]) -> Result<ImageFormat, LoadError> {
    match Sniffed::sniff(bytes) {
        Some(Sniffed::Png) => Ok(ImageFormat::Png),
        Some(Sniffed::Jpeg) => Ok(ImageFormat::Jpeg),
        Some(Sniffed::Gif) => Ok(ImageFormat::Gif),
        Some(Sniffed::Webp) => Ok(ImageFormat::WebP),
        Some(_) => Err(LoadError::Unsupported),
        None => Err(LoadError::NotImage),
    }
}

/// The largest size within `max_width` × `max_height` with the aspect ratio
/// of `width` × `height`, never enlarged.
pub fn fit(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width <= max_width && height <= max_height {
        return (width.max(1), height.max(1));
    }
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    (
        ((f64::from(width) * scale).round() as u32).clamp(1, max_width),
        ((f64::from(height) * scale).round() as u32).clamp(1, max_height),
    )
}

pub fn thumbnail(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<Thumbnail, LoadError> {
    let format = format(bytes)?;
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| LoadError::Malformed)?;
    if width == 0 || height == 0 {
        return Err(LoadError::Malformed);
    }
    if width > limits.max_source_side
        || height > limits.max_source_side
        || u64::from(width) * u64::from(height) > limits.max_source_pixels
    {
        return Err(LoadError::TooLarge);
    }

    let _decoding = DECODE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_source_side);
    decoder_limits.max_image_height = Some(limits.max_source_side);
    decoder_limits.max_alloc = Some(limits.max_decode_bytes);
    reader.limits(decoder_limits);
    let image = reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => LoadError::TooLarge,
        _ => LoadError::Malformed,
    })?;
    // Avatars keep the centered square, so the slot is always filled.
    let (image, crop_width, crop_height) = if limits.crop_square && width != height {
        let side = width.min(height);
        let square = image.crop_imm((width - side) / 2, (height - side) / 2, side, side);
        drop(image);
        (square, side, side)
    } else {
        (image, width, height)
    };
    let (thumb_width, thumb_height) = fit(
        crop_width,
        crop_height,
        limits.thumbnail_width,
        limits.thumbnail_height,
    );
    let small = if (thumb_width, thumb_height) == (crop_width, crop_height) {
        image
    } else {
        let small = image.thumbnail_exact(thumb_width, thumb_height);
        drop(image);
        small
    };
    Ok(Thumbnail {
        width: thumb_width,
        height: thumb_height,
        source_width: width,
        source_height: height,
        bgra: into_bgra(small),
    })
}

/// The pixels of `image` in the BGRA order GPUI draws.
pub(crate) fn into_bgra(image: DynamicImage) -> Vec<u8> {
    let mut pixels = image.into_rgba8().into_raw();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{encode, gif_two_frames};

    #[test]
    fn decodes_supported_formats_into_small_bgra_thumbnails() {
        let limits = Limits::default();
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::WebP,
        ] {
            let bytes = encode(format, 1200, 600);
            let thumb = thumbnail(&bytes, &limits, &CancelFlag::default()).unwrap();
            assert_eq!((thumb.width, thumb.height), (400, 200), "{format:?}");
            assert_eq!((thumb.source_width, thumb.source_height), (1200, 600));
            assert_eq!(thumb.byte_len(), 400 * 200 * 4);
            // The fixture is red: BGRA puts red third.
            let first = &thumb.bgra[..4];
            assert!(first[2] > 200 && first[0] < 60, "{format:?}: {first:?}");
        }
    }

    #[test]
    fn small_images_are_not_enlarged_and_gif_shows_its_first_frame() {
        let limits = Limits::default();
        let thumb = thumbnail(
            &encode(ImageFormat::Png, 16, 8),
            &limits,
            &CancelFlag::default(),
        )
        .unwrap();
        assert_eq!((thumb.width, thumb.height), (16, 8));
        let thumb = thumbnail(&gif_two_frames(), &limits, &CancelFlag::default()).unwrap();
        assert_eq!((thumb.width, thumb.height), (4, 4));
        assert!(thumb.bgra[2] > 200, "first frame is red");
    }

    #[test]
    fn rejects_non_images_unsupported_formats_malformed_and_oversized_content() {
        let limits = Limits::default();
        let cancel = CancelFlag::default();
        assert_eq!(
            thumbnail(b"<html>hi</html>", &limits, &cancel),
            Err(LoadError::NotImage)
        );
        let bmp = encode(ImageFormat::Bmp, 4, 4);
        assert_eq!(
            thumbnail(&bmp, &limits, &cancel),
            Err(LoadError::Unsupported)
        );
        let mut truncated = encode(ImageFormat::Png, 64, 64);
        truncated.truncate(60);
        assert!(matches!(
            thumbnail(&truncated, &limits, &cancel),
            Err(LoadError::Malformed)
        ));
        assert_eq!(
            thumbnail(b"\x89PNG\r\n\x1a\ngarbage", &limits, &cancel),
            Err(LoadError::Malformed)
        );

        // Dimensions are checked from the header, before decoding.
        let tight = Limits {
            max_source_pixels: 100 * 100,
            ..limits
        };
        let large = encode(ImageFormat::Png, 200, 100);
        assert_eq!(thumbnail(&large, &tight, &cancel), Err(LoadError::TooLarge));
        let narrow = Limits {
            max_source_side: 150,
            ..limits
        };
        assert_eq!(
            thumbnail(&large, &narrow, &cancel),
            Err(LoadError::TooLarge)
        );
        // The decoder's own allocation cap.
        let frugal = Limits {
            max_decode_bytes: 1024,
            ..limits
        };
        assert_eq!(
            thumbnail(&large, &frugal, &cancel),
            Err(LoadError::TooLarge)
        );
    }

    #[test]
    fn cancelled_work_is_not_decoded() {
        let cancel = CancelFlag::default();
        cancel.cancel();
        assert_eq!(
            thumbnail(&encode(ImageFormat::Png, 8, 8), &Limits::default(), &cancel),
            Err(LoadError::Cancelled)
        );
    }

    #[test]
    fn fits_within_the_box_keeping_the_aspect_ratio() {
        assert_eq!(fit(4000, 3000, 400, 200), (267, 200));
        assert_eq!(fit(3000, 300, 400, 200), (400, 40));
        assert_eq!(fit(100, 50, 400, 200), (100, 50));
        assert_eq!(fit(10_000, 1, 400, 200), (400, 1));
    }

    #[test]
    fn avatars_are_small_centered_squares() {
        let limits = Limits::avatar(32);
        let cancel = CancelFlag::default();
        for (width, height) in [(300, 100), (100, 300), (64, 64), (20, 10)] {
            let thumb =
                thumbnail(&encode(ImageFormat::Png, width, height), &limits, &cancel).unwrap();
            let side = width.min(height).min(32);
            assert_eq!(
                (thumb.width, thumb.height),
                (side, side),
                "{width}x{height}"
            );
            assert_eq!((thumb.source_width, thumb.source_height), (width, height));
            assert_eq!(thumb.byte_len(), (side * side * 4) as usize);
        }
        // Animated GIFs show their first frame here too.
        let thumb = thumbnail(&gif_two_frames(), &limits, &cancel).unwrap();
        assert!(thumb.bgra[2] > 200);
        // Sources above the avatar limits are refused before decoding.
        assert_eq!(
            thumbnail(&encode(ImageFormat::Png, 4097, 8), &limits, &cancel),
            Err(LoadError::TooLarge)
        );
        assert_eq!(
            thumbnail(&encode(ImageFormat::Png, 2100, 2100), &limits, &cancel),
            Err(LoadError::TooLarge)
        );
    }
}
