// SPDX-License-Identifier: MPL-2.0
//! Turns downloaded bytes into a small static thumbnail.
//!
//! The format comes from the content (`ImageFormat::sniff`), never from the
//! URL or the declared type. Only PNG, JPEG, GIF and WebP are decoded; GIF
//! and animated WebP show their first frame. Dimensions are read from the
//! header and checked before any pixel buffer is allocated, the decoder's
//! allocation is capped, and one decode runs at a time in the whole process,
//! so the full-size image exists only briefly and only once.

use std::{borrow::Cow, io::Cursor, sync::Mutex};

use cayenchat_model::attachment::ImageFormat as Sniffed;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::{CancelFlag, Limits, LoadError};

/// Serializes decoding so full-size images never coexist.
pub(crate) static DECODE: Mutex<()> = Mutex::new(());

/// Construct a decoder with the per-image limits installed before it reads
/// image metadata. Callers hold `DECODE` for this entire operation and while
/// using the returned decoder.
pub(crate) fn limited_decoder<'a>(
    bytes: &'a [u8],
    format: ImageFormat,
    limits: &Limits,
) -> image::ImageResult<impl ImageDecoder + 'a> {
    // `image`'s PNG decoder always inflates iCCP and compressed text chunks
    // while it constructs the decoder. We do not use those values for display,
    // so remove them before handing PNG data to it. The source raster is also
    // charged up front: PNG's internal allocation limit cannot be changed once
    // its decoder has been constructed.
    let input = if format == ImageFormat::Png {
        strip_png_metadata(bytes)
    } else {
        Cow::Borrowed(bytes)
    };
    let filtered_input_bytes = if matches!(&input, Cow::Owned(_)) {
        input.len() as u64
    } else {
        0
    };
    let mut reader = ImageReader::with_format(Cursor::new(input), format);
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_source_side);
    decoder_limits.max_image_height = Some(limits.max_source_side);
    decoder_limits.max_alloc = Some(if format == ImageFormat::Png {
        // Let the PNG decoder classify a malformed or unsupported header.
        // Such inputs cannot supply a trustworthy raster estimate.
        let raster_bytes = png_raster_bytes(bytes).unwrap_or(0);
        limits
            .max_decode_bytes
            .saturating_sub(raster_bytes)
            .saturating_sub(filtered_input_bytes)
    } else {
        limits.max_decode_bytes
    });
    reader.limits(decoder_limits);
    reader.into_decoder()
}

/// Drops compressed and uncompressed PNG metadata chunks that the media
/// decoder does not consume. Malformed chunk framing is left for `png` to
/// reject, rather than interpreting it differently here.
fn strip_png_metadata(bytes: &[u8]) -> Cow<'_, [u8]> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) {
        return Cow::Borrowed(bytes);
    }

    let mut offset = SIGNATURE.len();
    let mut output: Option<Vec<u8>> = None;
    while offset < bytes.len() {
        let Some(header_end) = offset.checked_add(8) else {
            return finish_png_filter(bytes, offset, output);
        };
        let Some(header) = bytes.get(offset..header_end) else {
            return finish_png_filter(bytes, offset, output);
        };
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let Some(chunk_end) = header_end
            .checked_add(length)
            .and_then(|n| n.checked_add(4))
        else {
            return finish_png_filter(bytes, offset, output);
        };
        if chunk_end > bytes.len() {
            return finish_png_filter(bytes, offset, output);
        }
        let kind = &header[4..8];
        let is_metadata = matches!(kind, b"iCCP" | b"tEXt" | b"zTXt" | b"iTXt");
        if is_metadata && output.is_none() {
            let mut filtered = Vec::with_capacity(bytes.len());
            filtered.extend_from_slice(&bytes[..offset]);
            output = Some(filtered);
        } else if !is_metadata && let Some(filtered) = &mut output {
            filtered.extend_from_slice(&bytes[offset..chunk_end]);
        }
        offset = chunk_end;
    }
    output.map_or(Cow::Borrowed(bytes), Cow::Owned)
}

fn finish_png_filter<'a>(bytes: &'a [u8], offset: usize, output: Option<Vec<u8>>) -> Cow<'a, [u8]> {
    output.map_or(Cow::Borrowed(bytes), |mut filtered| {
        filtered.extend_from_slice(&bytes[offset..]);
        Cow::Owned(filtered)
    })
}

/// Upper bound for the transformed PNG source raster decoded by `image`.
/// `EXPAND` turns indexed and sub-byte samples into 8-bit channels.
fn png_raster_bytes(bytes: &[u8]) -> Option<u64> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u64::from(u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?));
    let height = u64::from(u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?));
    let bit_depth = *bytes.get(24)?;
    let color_type = *bytes.get(25)?;
    let has_transparency = png_has_chunk(bytes, b"tRNS");
    let bytes_per_pixel = match (color_type, bit_depth, has_transparency) {
        (0, 16, true) => 4,
        (0, 16, false) => 2,
        (0, _, true) => 2,
        (0, _, false) => 1,
        (2, 16, true) => 8,
        (2, 16, false) => 6,
        (2, _, true) => 4,
        (2, _, false) => 3,
        (3, _, _) => 4,
        (4, 16, _) => 4,
        (4, _, _) => 2,
        (6, 16, _) => 8,
        (6, _, _) => 4,
        _ => return None,
    };
    width.checked_mul(height)?.checked_mul(bytes_per_pixel)
}

fn png_has_chunk(bytes: &[u8], wanted: &[u8; 4]) -> bool {
    let mut offset = 8usize;
    while let Some(header) = bytes.get(offset..offset.saturating_add(8)) {
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let Some(end) = offset
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .and_then(|n| n.checked_add(4))
        else {
            return false;
        };
        if end > bytes.len() {
            return false;
        }
        if header[4..8] == wanted[..] {
            return true;
        }
        offset = end;
    }
    false
}

/// Leaves room for the decoded source image before decoder-owned intermediate
/// buffers are allowed to use the per-image budget.
pub(crate) fn reserve_source_image(
    decoder: &mut (impl ImageDecoder + ?Sized),
    limits: &Limits,
) -> image::ImageResult<()> {
    let remaining = limits
        .max_decode_bytes
        .saturating_sub(decoder.total_bytes());
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_source_side);
    decoder_limits.max_image_height = Some(limits.max_source_side);
    decoder_limits.max_alloc = Some(remaining);
    decoder.set_limits(decoder_limits)
}

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

/// Whether `bytes` are a supported image small enough, in bytes and in
/// header-declared pixels, to be kept as an avatar original.
pub fn is_small_original(bytes: &[u8]) -> bool {
    if bytes.len() > crate::ORIGINAL_MAX_BYTES {
        return false;
    }
    let Ok(format) = format(bytes) else {
        return false;
    };
    let limits = Limits::avatar(1);
    let _decoding = DECODE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    limited_decoder(bytes, format, &limits)
        .ok()
        .map(|decoder| decoder.dimensions())
        .is_some_and(|(width, height)| {
            width <= crate::ORIGINAL_MAX_SIDE && height <= crate::ORIGINAL_MAX_SIDE
        })
}

pub fn thumbnail(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<Thumbnail, LoadError> {
    let format = format(bytes)?;
    let _decoding = DECODE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    let mut decoder = limited_decoder(bytes, format, limits).map_err(|error| match error {
        image::ImageError::Limits(_) => LoadError::TooLarge,
        _ => LoadError::Malformed,
    })?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(LoadError::Malformed);
    }
    if width > limits.max_source_side
        || height > limits.max_source_side
        || u64::from(width) * u64::from(height) > limits.max_source_pixels
    {
        return Err(LoadError::TooLarge);
    }
    if decoder.total_bytes() > limits.max_decode_bytes {
        return Err(LoadError::TooLarge);
    }
    reserve_source_image(&mut decoder, limits).map_err(|error| match error {
        image::ImageError::Limits(_) => LoadError::TooLarge,
        _ => LoadError::Malformed,
    })?;

    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    let image = DynamicImage::from_decoder(decoder).map_err(|error| match error {
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
    } else if limits.smooth_resize {
        downscale(image, thumb_width, thumb_height)
    } else {
        image.thumbnail_exact(thumb_width, thumb_height)
    };
    Ok(Thumbnail {
        width: thumb_width,
        height: thumb_height,
        source_width: width,
        source_height: height,
        bgra: into_bgra(small),
    })
}

/// `image` scaled down to exactly `width` × `height` with Lanczos3, which
/// keeps outlines and fine detail sharper than the box-like filter of
/// `thumbnail_exact`. Images with transparency are filtered premultiplied, so
/// the color of fully transparent pixels does not bleed into the edge as a
/// dark or light fringe; the result is straight (not premultiplied) again.
fn downscale(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    let mut rgba = image.into_rgba8();
    let translucent = rgba.pixels().any(|pixel| pixel.0[3] != 255);
    if translucent {
        for pixel in rgba.pixels_mut() {
            let alpha = u32::from(pixel.0[3]);
            for channel in &mut pixel.0[..3] {
                *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
    }
    let mut small =
        image::imageops::resize(&rgba, width, height, image::imageops::FilterType::Lanczos3);
    drop(rgba);
    if translucent {
        for pixel in small.pixels_mut() {
            let alpha = u32::from(pixel.0[3]);
            if alpha == 0 {
                pixel.0 = [0; 4];
                continue;
            }
            for channel in &mut pixel.0[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    DynamicImage::ImageRgba8(small)
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
    fn compressed_png_icc_metadata_obeys_decode_allocation_limit() {
        use std::borrow::Cow;

        let profile = vec![0; 2 * 1024 * 1024];
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Rgb;
        info.bit_depth = png::BitDepth::Eight;
        info.icc_profile = Some(Cow::Owned(profile));
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::with_info(&mut bytes, info).unwrap();
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0]).unwrap();
        drop(writer);
        assert!(bytes.windows(4).any(|chunk| chunk == b"iCCP"));
        let filtered = strip_png_metadata(&bytes);
        assert!(!filtered.as_ref().windows(4).any(|chunk| chunk == b"iCCP"));

        let _decode = DECODE.lock().unwrap();
        let mut decoder = limited_decoder(&bytes, ImageFormat::Png, &Limits::default()).unwrap();
        assert!(decoder.icc_profile().unwrap().is_none());
        drop(_decode);

        let limits = Limits {
            max_decode_bytes: 1024 * 1024,
            ..Limits::default()
        };
        assert!(thumbnail(&bytes, &limits, &CancelFlag::default()).is_ok());
        assert!(is_small_original(&bytes));
        assert!(crate::avatar_edit::open(&bytes).is_ok());
    }

    #[test]
    fn gif_frame_intermediate_uses_budget_left_after_source_image() {
        let bytes = include_bytes!("testdata/partial-frame.gif");

        let limits = Limits {
            max_decode_bytes: 48 * 1024 * 1024,
            max_source_pixels: 3000 * 3000,
            ..Limits::default()
        };
        assert_eq!(
            thumbnail(bytes, &limits, &CancelFlag::default()),
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

    fn png_from(image: image::RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn transparent_pixels_do_not_tint_the_edge() {
        // Opaque white left half, fully transparent black right half: the
        // old straight-alpha filtering darkened the edge.
        let source = image::RgbaImage::from_fn(128, 128, |x, _| {
            if x < 64 {
                image::Rgba([255, 255, 255, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        });
        let limits = Limits::avatar(32);
        let thumb = thumbnail(&png_from(source), &limits, &CancelFlag::default()).unwrap();
        for pixel in thumb.bgra.as_chunks::<4>().0 {
            if pixel[3] > 0 {
                assert!(pixel[..3].iter().all(|&c| c >= 250), "{pixel:?}");
            }
        }
        assert!(thumb.bgra.as_chunks::<4>().0.iter().any(|p| p[3] == 0));
    }

    #[test]
    fn opaque_downscaling_keeps_a_sharp_edge_and_flat_areas() {
        let source = image::RgbaImage::from_fn(128, 128, |x, _| {
            if x < 64 {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        });
        let thumb = thumbnail(
            &png_from(source),
            &Limits::avatar(32),
            &CancelFlag::default(),
        )
        .unwrap();
        let row = &thumb.bgra[..32 * 4];
        assert!(row[0] <= 2 && row[4 * 31] >= 253);
        // The transition is at most a few pixels wide.
        let gray = (0..32).filter(|&x| (3..252).contains(&row[x * 4])).count();
        assert!(gray <= 3, "{gray}");
    }

    #[test]
    fn previews_keep_the_plain_thumbnail_filter() {
        let source = image::RgbaImage::from_fn(800, 400, |x, y| {
            image::Rgba([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 256) as u8, 255])
        });
        let expected =
            into_bgra(DynamicImage::ImageRgba8(source.clone()).thumbnail_exact(400, 200));
        let thumb = thumbnail(
            &png_from(source),
            &Limits::default(),
            &CancelFlag::default(),
        )
        .unwrap();
        assert_eq!(thumb.bgra, expected);
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
