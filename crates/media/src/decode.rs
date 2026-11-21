// SPDX-License-Identifier: MPL-2.0
//! Turns downloaded bytes into a small static thumbnail.
//!
//! The format comes from the content (`ImageFormat::sniff`), never from the
//! URL or the declared type. Only PNG, JPEG, GIF, WebP, BMP and ICO are
//! decoded (TIFF and AVIF are recognized but refused); GIF and animated WebP
//! show their first frame, and an ICO shows its largest entry, an embedded
//! PNG going through the PNG path below. Dimensions are read from the
//! header and checked before any pixel buffer is allocated, the decoder's
//! allocation is capped, and one decode runs at a time in the whole process,
//! so the full-size image exists only briefly and only once.
//!
//! # Why PNG has its own budget handling
//!
//! `image`'s PNG decoder always inflates iCCP and compressed text chunks while
//! it is constructed, and its allocation limit is fixed then
//! (`png::Decoder::new_with_limits`; `ImageDecoder::set_limits` does not reach
//! it). `png` could skip those chunks (`set_ignore_text_chunk`,
//! `set_ignore_iccp_chunk`), but `image` does not expose that, and using `png`
//! directly would mean our own `ImageDecoder` for every PNG colour type and
//! Adam7. So for PNG we strip those chunks (`strip_png_metadata`) and take the
//! copy, twice any eXIf (avatar orientation needs it, and `png` holds it twice),
//! the IHDR raster and `PNG_UNRESERVED_HEADROOM` off the limit up front
//! (`png_reservations`). Both walks share the one chunk reader, `PngChunks`.
//! The measurements behind the headroom are in `spec/performance.md`.

use std::{borrow::Cow, io::Cursor, sync::Mutex};

use cayenchat_model::attachment::ImageFormat as Sniffed;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::{CancelFlag, Limits, LoadError};

/// Serializes decoding so full-size images never coexist.
pub(crate) static DECODE: Mutex<()> = Mutex::new(());

/// Construct a decoder with the per-image limits installed before it reads
/// image metadata. Callers hold `DECODE` for this entire operation and while
/// using the returned decoder. The returned reservation covers the filtered
/// PNG copy for as long as the decoder retains it.
pub(crate) fn limited_decoder<'a>(
    bytes: &'a [u8],
    format: ImageFormat,
    limits: &Limits,
) -> image::ImageResult<(impl ImageDecoder + 'a, u64)> {
    // `image`'s PNG decoder always inflates iCCP and compressed text chunks
    // while it constructs the decoder. We do not use those values for display,
    // so remove them before handing PNG data to it. The input copy and source
    // raster are charged afterwards by `reserve_source_image`, from the
    // constructed decoder's `total_bytes`. Constructing a decoder does not
    // allocate raster-sized memory, but `png`'s own allocation limit cannot
    // change after construction (`PngDecoder::set_limits` does not reach it),
    // so for PNG the input copy, the retained eXIf data and the raster
    // declared by IHDR are all taken off that limit here.
    //
    // ICO is reduced to the one entry that will be shown before anything else:
    // an embedded PNG goes through the PNG path above (same limits, same
    // stripping), and an embedded DIB is repacked as a one-entry ICO, whose
    // copy is charged like the PNG copy.
    let (mut input, mut format) = (Cow::Borrowed(bytes), format);
    let mut ico_copy_bytes = 0;
    if format == ImageFormat::Ico {
        let (head, entry) = ico_best_entry(bytes)?;
        if entry.starts_with(PNG_SIGNATURE) {
            (input, format) = (Cow::Borrowed(entry), ImageFormat::Png);
        } else {
            ico_copy_bytes = entry.len() as u64 + ICO_SINGLE_HEADER_LEN as u64;
            if ico_copy_bytes > limits.max_decode_bytes {
                return Err(insufficient_memory());
            }
            input = Cow::Owned(single_entry_ico(head, entry));
        }
    }
    let (metadata_copy_bytes, raster_bytes) = png_reservations(&input, format)?;
    let metadata_copy_bytes = metadata_copy_bytes.saturating_add(ico_copy_bytes);
    if metadata_copy_bytes.saturating_add(raster_bytes) > limits.max_decode_bytes {
        return Err(insufficient_memory());
    }

    let stripped = match format {
        ImageFormat::Png => match strip_png_metadata(&input) {
            Cow::Owned(stripped) => Some(stripped),
            Cow::Borrowed(_) => None,
        },
        _ => None,
    };
    let input = stripped.map_or(input, Cow::Owned);
    let mut reader = ImageReader::with_format(Cursor::new(input), format);
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_source_side);
    decoder_limits.max_image_height = Some(limits.max_source_side);
    decoder_limits.max_alloc = Some(limits.max_decode_bytes - metadata_copy_bytes - raster_bytes);
    reader.limits(decoder_limits);
    reader
        .into_decoder()
        .map(|decoder| (decoder, metadata_copy_bytes))
}

fn insufficient_memory() -> image::ImageError {
    image::ImageError::Limits(image::error::LimitError::from_kind(
        image::error::LimitErrorKind::InsufficientMemory,
    ))
}

/// Reserves the full source length before filtering allocates its output.
/// This is conservative because metadata chunks are removed, but guarantees
/// the copy's capacity is budgeted even when most of the input is discarded.
///
/// It also reserves twice the length of every eXIf chunk. `png` keeps eXIf
/// (which we do not strip: avatar orientation uses it) twice while the decoder
/// lives, in its raw buffer and in the parsed info, and its internal
/// allocation limit is fixed when the decoder is constructed, so that memory
/// cannot be left to the remaining-budget check done later.
///
/// Returns that reservation and the raster the decoded image will need
/// (see `png_raster_bytes`), which `reserve_source_image` charges later from
/// `total_bytes`; here it only lowers `png`'s fixed allocation limit.
///
/// A PNG whose first chunk is not a well-formed IHDR is rejected here, before
/// any metadata is stripped: otherwise a chunk placed ahead of IHDR would make
/// the estimate unavailable and then be removed, leaving a valid PNG whose
/// raster was never reserved.
fn png_reservations(bytes: &[u8], format: ImageFormat) -> image::ImageResult<(u64, u64)> {
    if format != ImageFormat::Png {
        return Ok((0, 0));
    }
    let scan = scan_png_chunks(bytes);
    let Some(raster) = png_raster_bytes(bytes, scan.has_trns) else {
        return Err(image::ImageError::Decoding(
            image::error::DecodingError::new(
                image::error::ImageFormatHint::Exact(ImageFormat::Png),
                "PNG does not start with a valid IHDR",
            ),
        ));
    };
    let copy = if scan.has_metadata {
        bytes.len() as u64
    } else {
        0
    };
    Ok((
        copy.saturating_add(scan.exif_bytes.saturating_mul(2))
            .saturating_add(PNG_UNRESERVED_HEADROOM),
        raster,
    ))
}

/// Memory `png` allocates without going through its allocation limit
/// (`UnfilteringBuffer`, inflate state, row buffers), so lowering `max_alloc`
/// cannot account for it. Measured at the default 48 MiB budget with a
/// 4096 × 3070 RGBA PNG, plain and Adam7: the decode peak exceeded the budget
/// by about 250–266 KiB with the raster filling it; this keeps twice that
/// aside for PNG. See `spec/performance.md`.
const PNG_UNRESERVED_HEADROOM: u64 = 512 * 1024;

/// Upper bound of the decoded raster `image` produces for a PNG, from its
/// IHDR (and whether a tRNS chunk adds an alpha channel). `None` when the
/// header is not a well-formed IHDR, which `png` then rejects itself. A raster
/// too large for `u64` saturates, so it never fits any budget.
fn png_raster_bytes(bytes: &[u8], has_trns: bool) -> Option<u64> {
    let ihdr = bytes.get(8..29)?;
    if &ihdr[4..8] != b"IHDR" || ihdr[..4] != 13u32.to_be_bytes() {
        return None;
    }
    let width = u64::from(u32::from_be_bytes(ihdr[8..12].try_into().ok()?));
    let height = u64::from(u32::from_be_bytes(ihdr[12..16].try_into().ok()?));
    let (depth, color_type) = (ihdr[16], ihdr[17]);
    let channels: u64 = match color_type {
        0 => 1,
        2 | 3 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    let channels = if has_trns && color_type != 4 && color_type != 6 {
        channels + 1
    } else {
        channels
    };
    let sample_bytes = if depth == 16 { 2 } else { 1 };
    Some(
        width
            .saturating_mul(height)
            .saturating_mul(channels * sample_bytes),
    )
}

#[derive(Default)]
struct PngScan {
    /// Some chunk is metadata that `strip_png_metadata` removes.
    has_metadata: bool,
    /// Total length of eXIf chunks.
    exif_bytes: u64,
    has_trns: bool,
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Whether `kind` is a chunk `strip_png_metadata` removes.
fn is_png_metadata(kind: &[u8; 4]) -> bool {
    matches!(kind, b"iCCP" | b"tEXt" | b"zTXt" | b"iTXt")
}

/// The one place PNG chunk framing is read. Yields each well-framed chunk as
/// (type, range of the whole chunk including length, type and CRC) and stops
/// at the first malformed or truncated one; `rest` then gives where it stopped.
/// Empty if `bytes` lacks the PNG signature.
struct PngChunks<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> PngChunks<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let offset = if bytes.starts_with(PNG_SIGNATURE) {
            PNG_SIGNATURE.len()
        } else {
            bytes.len()
        };
        Self { bytes, offset }
    }

    /// Offset of the first byte not yielded as a well-framed chunk.
    fn rest(&self) -> usize {
        self.offset
    }
}

impl Iterator for PngChunks<'_> {
    type Item = ([u8; 4], std::ops::Range<usize>);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.offset;
        let header = self.bytes.get(start..start.checked_add(8)?)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = start.checked_add(8)?.checked_add(length)?.checked_add(4)?;
        if end > self.bytes.len() {
            return None;
        }
        self.offset = end;
        Some((header[4..8].try_into().unwrap(), start..end))
    }
}

/// Walks well-framed PNG chunks.
fn scan_png_chunks(bytes: &[u8]) -> PngScan {
    let mut scan = PngScan::default();
    for (kind, range) in PngChunks::new(bytes) {
        match &kind {
            b"eXIf" => scan.exif_bytes += (range.len() - 12) as u64,
            b"tRNS" => scan.has_trns = true,
            _ if is_png_metadata(&kind) => scan.has_metadata = true,
            _ => {}
        }
    }
    scan
}

/// Drops compressed and uncompressed PNG metadata chunks that the media
/// decoder does not consume. Malformed chunk framing is left for `png` to
/// reject, rather than interpreting it differently here: everything from the
/// first malformed chunk on is passed through unchanged.
fn strip_png_metadata(bytes: &[u8]) -> Cow<'_, [u8]> {
    let mut chunks = PngChunks::new(bytes);
    let mut output: Option<Vec<u8>> = None;
    for (kind, range) in chunks.by_ref() {
        let is_metadata = is_png_metadata(&kind);
        if is_metadata && output.is_none() {
            let mut filtered = Vec::with_capacity(bytes.len());
            filtered.extend_from_slice(&bytes[..range.start]);
            output = Some(filtered);
        } else if !is_metadata && let Some(filtered) = &mut output {
            filtered.extend_from_slice(&bytes[range]);
        }
    }
    output.map_or(Cow::Borrowed(bytes), |mut filtered| {
        filtered.extend_from_slice(&bytes[chunks.rest()..]);
        Cow::Owned(filtered)
    })
}

/// Leaves room for the decoded source image before decoder-owned intermediate
/// buffers are allowed to use the per-image budget.
pub(crate) fn reserve_source_image(
    decoder: &mut (impl ImageDecoder + ?Sized),
    limits: &Limits,
    reserved_copy_bytes: u64,
) -> image::ImageResult<()> {
    if decoder
        .total_bytes()
        .checked_add(reserved_copy_bytes)
        .is_none_or(|bytes| bytes > limits.max_decode_bytes)
    {
        return Err(insufficient_memory());
    }
    let remaining = limits
        .max_decode_bytes
        .saturating_sub(decoder.total_bytes())
        .saturating_sub(reserved_copy_bytes);
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
        Some(Sniffed::Bmp) => Ok(ImageFormat::Bmp),
        // TIFF is not previewed: its JPEG-compressed strips are expanded by a
        // nested decoder outside our limits (D018).
        Some(_) => Err(LoadError::Unsupported),
        // ICO is not an upload format, so `ImageFormat::sniff` does not know
        // it; it is recognized here only.
        None if is_ico(bytes) => Ok(ImageFormat::Ico),
        None => Err(LoadError::NotImage),
    }
}

/// ICO directory header: reserved 0, type 1 (icons; 2 is cursors), and at
/// least one entry.
fn is_ico(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0, 0, 1, 0]) && bytes.get(4..6).is_some_and(|count| count != [0, 0])
}

fn ico_error(message: &'static str) -> image::ImageError {
    image::ImageError::Decoding(image::error::DecodingError::new(
        image::error::ImageFormatHint::Exact(ImageFormat::Ico),
        message,
    ))
}

/// Length of the header of an ICO with one entry: 6 + 16.
const ICO_SINGLE_HEADER_LEN: usize = 22;

/// The data of the ICO entry to show: the one declaring the most pixels (the
/// first of equals; 0 means 256). `image` would hand an embedded PNG to `png`
/// outside our limits and without `strip_png_metadata`, and does not bound the
/// read by the entry's size, so the caller decodes this one entry itself.
/// The directory (16 bytes per entry) is read once and must lie within the
/// input, and the chosen entry must too: the work is bounded by the input and
/// does not depend on what the entries point at.
fn ico_best_entry(bytes: &[u8]) -> image::ImageResult<([u8; 8], &[u8])> {
    let count = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    let directory = bytes
        .get(6..6 + 16 * count)
        .ok_or_else(|| ico_error("ICO directory is cut off"))?;
    let side = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
    let best = directory
        .as_chunks::<16>()
        .0
        .iter()
        .rev()
        .max_by_key(|entry| side(entry[0]) * side(entry[1]))
        .ok_or_else(|| ico_error("ICO has no entries"))?;
    let size = u32::from_le_bytes(best[8..12].try_into().unwrap()) as usize;
    let offset = u32::from_le_bytes(best[12..16].try_into().unwrap()) as usize;
    let data = offset
        .checked_add(size)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| ico_error("ICO entry is cut off"))?;
    // The first 8 bytes of the entry: size, colours, planes and depth.
    Ok((best[..8].try_into().unwrap(), data))
}

/// An ICO holding only `dib`, the bitmap data of the chosen entry, under the
/// entry's own `head` (`image` checks it against the bitmap header).
fn single_entry_ico(head: [u8; 8], dib: &[u8]) -> Vec<u8> {
    let mut ico = Vec::with_capacity(ICO_SINGLE_HEADER_LEN + dib.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&head);
    ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
    ico.extend_from_slice(&(ICO_SINGLE_HEADER_LEN as u32).to_le_bytes());
    ico.extend_from_slice(dib);
    ico
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
        .map(|(decoder, _)| decoder.dimensions())
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
    let (mut decoder, reserved_copy_bytes) =
        limited_decoder(bytes, format, limits).map_err(|error| match error {
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
    reserve_source_image(&mut decoder, limits, reserved_copy_bytes).map_err(
        |error| match error {
            image::ImageError::Limits(_) => LoadError::TooLarge,
            _ => LoadError::Malformed,
        },
    )?;

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
    use crate::format_fixtures::*;
    use crate::testing::{encode, gif_two_frames};

    fn ihdr_png(width: u32, height: u32, depth: u8, color_type: u8) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[depth, color_type, 0, 0, 0]);
        bytes
    }

    #[test]
    fn png_raster_bytes_follow_ihdr_without_overflowing() {
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 8, 6), false), Some(400));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 16, 6), false), Some(800));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 8, 2), false), Some(300));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 8, 2), true), Some(400));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 1, 0), false), Some(100));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 8, 3), true), Some(400));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 8, 4), true), Some(200));
        assert_eq!(png_raster_bytes(&ihdr_png(10, 10, 8, 5), false), None);
        assert_eq!(png_raster_bytes(b"\x89PNG\r\n\x1a\n", false), None);
        assert_eq!(
            png_raster_bytes(&ihdr_png(u32::MAX, u32::MAX, 16, 6), false),
            Some(u64::MAX)
        );
    }

    #[test]
    fn decodes_supported_formats_into_small_bgra_thumbnails() {
        let limits = Limits::default();
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::WebP,
            ImageFormat::Bmp,
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
        // AVIF and HEIC are recognized but not decoded.
        let mut avif = b"\0\0\0\x18ftypavif\0\0\0\0avifmif1".to_vec();
        avif.resize(64, 0);
        assert_eq!(
            thumbnail(&avif, &limits, &cancel),
            Err(LoadError::Unsupported)
        );
        // TIFF is recognized, not decoded (nested JPEG strips escape the limits).
        assert_eq!(
            thumbnail(&encode(ImageFormat::Tiff, 16, 16), &limits, &cancel),
            Err(LoadError::Unsupported)
        );
        // Cursors (ICO type 2) are not icons.
        assert_eq!(
            thumbnail(b"\0\0\x02\0\x01\0 rest of a cursor", &limits, &cancel),
            Err(LoadError::NotImage)
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
        let (mut decoder, _) =
            limited_decoder(&bytes, ImageFormat::Png, &Limits::default()).unwrap();
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
    fn oversized_declared_png_dimensions_are_rejected_before_decoding() {
        // 4096 × 2800 RGBA declares about 44 MiB of raster, over a 16 MiB
        // budget. The pixel data is cut off, so starting to decode it would
        // fail as `Malformed`; `TooLarge` shows the header was judged first.
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 4096, 2800);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![0; 4096 * 2800 * 4]).unwrap();
        drop(writer);
        bytes.truncate(bytes.len() / 2);

        let limits = Limits {
            max_decode_bytes: 16 * 1024 * 1024,
            ..Limits::default()
        };
        assert_eq!(
            thumbnail(&bytes, &limits, &CancelFlag::default()),
            Err(LoadError::TooLarge)
        );
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

    fn red_ico(embedded: Vec<u8>, side: u8) -> Vec<u8> {
        ico_of(&[(side, side, embedded)])
    }

    #[test]
    fn ico_with_png_or_bmp_entries_decodes_the_best_entry() {
        let limits = Limits::default();
        let cancel = CancelFlag::default();
        // Several candidates: the larger one at the same depth wins.
        let ico = ico_of(&[
            (16, 16, encode(ImageFormat::Png, 16, 16)),
            (64, 64, encode(ImageFormat::Png, 64, 64)),
        ]);
        let thumb = thumbnail(&ico, &limits, &cancel).unwrap();
        assert_eq!((thumb.width, thumb.height), (64, 64));
        assert!(thumb.bgra[2] > 200 && thumb.bgra[0] < 60);

        // Through the encoder, which writes PNG entries.
        let mut encoded = Vec::new();
        image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut encoded), ImageFormat::Ico)
            .unwrap();
        let thumb = thumbnail(&encoded, &limits, &cancel).unwrap();
        assert_eq!((thumb.width, thumb.height), (32, 32));

        // A BMP (DIB) entry: a bottom-up 24-bit DIB with a mask, no file header.
        let mut dib = bmp_declaring(4, 8, 4 * 4 * 3 + 4 * 4)[14..].to_vec();
        dib[8..12].copy_from_slice(&8i32.to_le_bytes());
        let thumb = thumbnail(&red_ico(dib, 4), &limits, &cancel);
        assert!(thumb.is_ok(), "{thumb:?}");
    }

    #[test]
    fn hostile_ico_is_refused_within_the_budget() {
        let limits = Limits::default();
        let cancel = CancelFlag::default();
        let png = encode(ImageFormat::Png, 16, 16);
        // Entries pointing past the end, and a truncated entry.
        let mut ico = red_ico(png.clone(), 16);
        ico.truncate(ico.len() - 20);
        assert_eq!(thumbnail(&ico, &limits, &cancel), Err(LoadError::Malformed));
        let mut count = red_ico(png.clone(), 16);
        count[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            thumbnail(&count, &limits, &cancel),
            Err(LoadError::Malformed)
        );
        // An embedded PNG declaring a huge raster never reaches pixel decoding.
        let huge = png_declaring(png.clone(), 60_000, 60_000);
        assert_eq!(
            thumbnail(&red_ico(huge, 16), &limits, &cancel),
            Err(LoadError::TooLarge)
        );
        // The embedded PNG takes the PNG path: metadata that `png` would
        // inflate outside the budget is stripped, wherever the directory says
        // the entry ends.
        let text = |kind: &[u8; 4]| {
            let mut chunk = 4u32.to_be_bytes().to_vec();
            chunk.extend_from_slice(kind);
            chunk.extend_from_slice(b"k\0v\0");
            chunk.extend_from_slice(&[0; 4]);
            let mut with = png[..33].to_vec();
            with.extend_from_slice(&chunk);
            with.extend_from_slice(&png[33..]);
            with
        };
        for kind in [b"tEXt", b"zTXt", b"iTXt", b"iCCP", b"eXIf"] {
            let bytes = red_ico(text(kind), 16);
            let thumb = thumbnail(&bytes, &limits, &cancel);
            // A malformed chunk body may fail in `png`; it must not get further.
            assert!(
                thumb.is_ok() || thumb == Err(LoadError::Malformed),
                "{thumb:?}"
            );
        }
        // A 16×16 entry whose PNG really is 4096 × 3070 (the directory's size
        // is not trusted): the PNG budget refuses it.
        let big = png_declaring(png.clone(), 4096, 3070);
        assert_eq!(
            thumbnail(&ico_of(&[(0, 0, big)]), &limits, &cancel),
            Err(LoadError::TooLarge)
        );
        // Many entries pointing at one PNG cost one directory read.
        let many = ico_many(&png, 4000);
        assert!(thumbnail(&many, &limits, &cancel).is_ok());
    }

    #[test]
    fn ico_with_a_huge_metadata_png_is_decoded_within_the_budget() {
        // 20 MiB of compressed iCCP in a 16×16 PNG: stripped, so only the
        // pixels are decoded. A directory claiming less or more than the PNG
        // never widens what is read.
        let mut info = png::Info::with_size(16, 16);
        info.color_type = png::ColorType::Rgb;
        info.bit_depth = png::BitDepth::Eight;
        info.icc_profile = Some(std::borrow::Cow::Owned(vec![0; 20 * 1024 * 1024]));
        let mut png = Vec::new();
        let mut encoder = png::Encoder::with_info(&mut png, info).unwrap();
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0].repeat(256)).unwrap();
        drop(writer);
        let honest = red_ico(png, 16);
        let result = thumbnail(&honest, &Limits::default(), &CancelFlag::default());
        assert!(result.is_ok(), "{result:?}");
        for size in [33u32, u32::MAX] {
            let mut ico = honest.clone();
            ico[14..18].copy_from_slice(&size.to_le_bytes());
            let result = thumbnail(&ico, &Limits::default(), &CancelFlag::default());
            assert_eq!(result, Err(LoadError::Malformed), "{size}");
        }
    }

    #[test]
    fn bmp_with_hostile_headers_fails_within_the_limits() {
        let limits = Limits::default();
        let cancel = CancelFlag::default();
        let large = |bytes: Vec<u8>| thumbnail(&bytes, &limits, &cancel);
        // Dimensions beyond the side and pixel limits, from the header alone.
        assert_eq!(
            large(bmp_declaring(60_000, 60_000, 64)),
            Err(LoadError::TooLarge)
        );
        assert_eq!(
            large(bmp_declaring(8192, 8192, 64)),
            Err(LoadError::TooLarge)
        );
        // Within the limits but with far less data than declared.
        assert_eq!(
            large(bmp_declaring(4000, 4000, 64)),
            Err(LoadError::Malformed)
        );
        // Run-length BMP with an absurd declared height, and a short one.
        assert!(large(bmp_rle8(100, -60_000)).is_err());
        let _ = large(bmp_rle8(100, 100));
        // Truncated files and a bare signature.
        let mut bmp = encode(ImageFormat::Bmp, 64, 64);
        bmp.truncate(bmp.len() / 2);
        assert_eq!(large(bmp), Err(LoadError::Malformed));
        assert_eq!(
            large(b"BM\0\0\0\0\0\0\0\0\0\0\0\0\0".to_vec()),
            Err(LoadError::Malformed)
        );
        assert_eq!(large(vec![0, 0, 1, 0, 1, 0]), Err(LoadError::Malformed));
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
    fn avatars_use_the_area_average_of_previews_with_transparency() {
        // Opaque white left half, fully transparent black right half. The
        // avatar thumbnail is exactly what `thumbnail_exact` makes (no
        // premultiplying), so a preview of the same image looks the same.
        let source = image::RgbaImage::from_fn(128, 128, |x, _| {
            if x < 64 {
                image::Rgba([255, 255, 255, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        });
        let expected = into_bgra(DynamicImage::ImageRgba8(source.clone()).thumbnail_exact(32, 32));
        let thumb = thumbnail(
            &png_from(source),
            &Limits::avatar(32),
            &CancelFlag::default(),
        )
        .unwrap();
        assert_eq!(thumb.bgra, expected);
        assert!(thumb.bgra.as_chunks::<4>().0.iter().any(|p| p[3] == 0));
    }

    #[test]
    fn avatars_use_the_area_average_of_previews_for_opaque_images() {
        let source = image::RgbaImage::from_fn(128, 128, |x, y| {
            image::Rgba([(x * 2) as u8, (y * 2) as u8, ((x ^ y) % 256) as u8, 255])
        });
        let expected = into_bgra(DynamicImage::ImageRgba8(source.clone()).thumbnail_exact(32, 32));
        let thumb = thumbnail(
            &png_from(source),
            &Limits::avatar(32),
            &CancelFlag::default(),
        )
        .unwrap();
        assert_eq!(thumb.bgra, expected);
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
