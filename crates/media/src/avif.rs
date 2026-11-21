// SPDX-License-Identifier: MPL-2.0
//! AVIF previews (D018): the first still image of the file, scaled down.
//!
//! `image`'s AVIF decoder needs libdav1d, a C library to build, distribute and
//! audit on every platform, and it decodes in its constructor without looking
//! at the limits. So AVIF does not go through `image`: `mp4parse` (pure Rust)
//! finds the AV1 data in the container and `rav1d` (the Rust port of dav1d,
//! built without assembly, so no C code and no system library) decodes it.
//!
//! What is shown: the primary item, with its separate alpha item when there is
//! one, rotated by `irot`. Grid (tiled) images, image sequences without a
//! primary item and unusual colour matrices are refused, and the link stays
//! text. ICC profiles, HDR transfer functions and mirroring are ignored.
//!
//! # Limits
//!
//! The sequence header is parsed first and gives the frame size. It is checked
//! against the pixel limits and the decode budget *before* any frame memory
//! exists (`budget_bytes`), and `rav1d` itself refuses larger frames
//! (`frame_size_limit`). Only the decoded planes are kept, and the picture is
//! reduced straight to the thumbnail, so there is never a full RGBA copy. One
//! thread and one frame in flight keep the decoder's own memory at its smallest.
//! The factors in `budget_bytes` come from measurements in
//! `tests/decode_allocations.rs` and `spec/performance.md`.
//!
//! `rav1d` exposes only its C ABI, so it is called through a small `unsafe`
//! wrapper here. A panic inside it cannot unwind through that ABI and aborts the
//! process; the mutation test in `tests/avif_mutations.rs` is the evidence that
//! damaged files do not reach one.

use std::{
    ffi::c_void,
    io::Cursor,
    mem::MaybeUninit,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
};

use image::{RgbaImage, imageops};
use mp4parse::{ImageRotation, ParseStrictness};
use rav1d::{
    Dav1dResult,
    include::dav1d::{
        data::Dav1dData,
        dav1d::{Dav1dContext, Dav1dSettings},
        headers::Dav1dSequenceHeader,
        picture::Dav1dPicture,
    },
    src::lib::{
        dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings,
        dav1d_get_picture, dav1d_open, dav1d_parse_sequence_header, dav1d_picture_unref,
        dav1d_send_data,
    },
};

use crate::{CancelFlag, Limits, LoadError, Thumbnail, decode};

/// Whether `bytes` is an AVIF file (`ftyp` with an `avif` or `avis` brand).
pub(crate) fn is_avif(bytes: &[u8]) -> bool {
    cayenchat_model::attachment::ImageFormat::sniff(bytes)
        == Some(cayenchat_model::attachment::ImageFormat::Avif)
}

/// Layouts, as `Dav1dPixelLayout` numbers them.
const I400: u32 = 0;
const I420: u32 = 1;
const I422: u32 = 2;
const I444: u32 = 3;

/// Header facts that decide the memory a decode needs.
struct Sequence {
    width: u32,
    height: u32,
    layout: u32,
    /// Bytes per stored sample: 1 for 8 bit, 2 for 10 and 12 bit.
    sample_bytes: u64,
    matrix: u32,
    full_range: bool,
}

fn sequence_header(coded: &[u8]) -> Option<Sequence> {
    if coded.is_empty() {
        return None;
    }
    let mut header = MaybeUninit::<Dav1dSequenceHeader>::uninit();
    // SAFETY: `coded` is a valid slice and `header` is valid to write to; the
    // function initializes it when it returns success.
    let result = unsafe {
        dav1d_parse_sequence_header(
            NonNull::new(header.as_mut_ptr()),
            NonNull::new(coded.as_ptr().cast_mut()),
            coded.len(),
        )
    };
    if result.0 != 0 {
        return None;
    }
    // SAFETY: success means the header was written.
    let header = unsafe { header.assume_init() };
    Some(Sequence {
        width: u32::try_from(header.max_width).ok()?,
        height: u32::try_from(header.max_height).ok()?,
        layout: header.layout,
        sample_bytes: if header.hbd == 0 { 1 } else { 2 },
        matrix: header.mtrx,
        full_range: header.color_range != 0,
    })
}

/// Samples per pixel stored for `layout`, in quarters.
fn quarter_samples(layout: u32) -> u64 {
    match layout {
        I400 => 4,
        I420 => 6,
        I422 => 8,
        _ => 12,
    }
}

/// Upper bound of the memory decoding `sequence` takes, from its header, plus
/// `alpha` for the second plane of an alpha item. The planes are the picture
/// itself; `rav1d` adds working memory proportional to the frame (metadata,
/// loop filter and reference state) which `OVERHEAD_PER_PIXEL` covers.
fn budget_bytes(sequence: &Sequence, alpha: bool, input_bytes: usize) -> u64 {
    // dav1d pads frame sizes to whole 128 pixel superblocks.
    let pad = |side: u32| u64::from(side).div_ceil(128) * 128;
    let pixels = pad(sequence.width) * pad(sequence.height);
    let planes = pixels * quarter_samples(sequence.layout) / 4 * sequence.sample_bytes;
    let alpha_plane = if alpha {
        pixels * sequence.sample_bytes
    } else {
        0
    };
    // The container parse and the copy handed to the decoder.
    let copies = 2 * input_bytes as u64;
    (planes + alpha_plane + pixels * OVERHEAD_PER_PIXEL).saturating_add(copies)
}

/// Bytes of decoder working memory per (padded) pixel, on top of the planes.
/// Measured at 0.6–0.7 (2048², 3000² and 4096² frames, 8 to 12 bit, 4:2:0 and
/// 4:4:4; see `tests/decode_allocations.rs`), so this keeps a margin of about 1.5.
const OVERHEAD_PER_PIXEL: u64 = 1;

/// A decoder context closed on drop.
struct Decoder(Option<Dav1dContext>);

impl Decoder {
    fn new(frame_size_limit: u32) -> Option<Self> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: `settings` is valid to write to.
        unsafe { dav1d_default_settings(NonNull::new(settings.as_mut_ptr())?) };
        // SAFETY: initialized by `dav1d_default_settings`.
        let mut settings = unsafe { settings.assume_init() };
        settings.n_threads = 1;
        // Film grain would copy the whole picture; a thumbnail does not need it.
        settings.apply_grain = 0;
        settings.max_frame_delay = 1;
        settings.frame_size_limit = frame_size_limit;
        settings.all_layers = 0;
        settings.output_invisible_frames = 0;
        // No logging: untrusted files must not write to the terminal.
        // SAFETY: the logger is two `Option`s (cookie and callback), for which
        // all zeroes is `None`, and a logger without a callback is never called.
        settings.logger = unsafe { std::mem::zeroed() };
        let mut context: Option<Dav1dContext> = None;
        // SAFETY: both pointers are valid for the call.
        let result = unsafe { dav1d_open(NonNull::new(&mut context), NonNull::new(&mut settings)) };
        (result.0 == 0).then_some(())?;
        context.map(|context| Self(Some(context)))
    }

    /// Decodes the first picture of `coded`.
    fn decode(&self, coded: &[u8]) -> Option<Picture> {
        let mut data = Dav1dData::default();
        // SAFETY: `data` is valid to write to.
        let buffer = unsafe { dav1d_data_create(NonNull::new(&mut data), coded.len()) };
        if buffer.is_null() {
            return None;
        }
        // SAFETY: the buffer has `coded.len()` bytes and does not overlap `coded`.
        unsafe { std::ptr::copy_nonoverlapping(coded.as_ptr(), buffer, coded.len()) };
        // A still image is one temporal unit; a few rounds cover the decoder
        // consuming it in pieces. The data left over after an error is dropped
        // with `data`.
        for _ in 0..16 {
            if data.sz > 0 {
                // SAFETY: the context is open and `data` is valid.
                let _: Dav1dResult = unsafe { dav1d_send_data(self.0, NonNull::new(&mut data)) };
            }
            let mut picture = Picture(Dav1dPicture::default());
            // SAFETY: the context is open and the picture is valid to write to.
            let result = unsafe { dav1d_get_picture(self.0, NonNull::new(&mut picture.0)) };
            if result.0 == 0 {
                // SAFETY: the data was created by `dav1d_data_create`.
                unsafe { dav1d_data_unref(NonNull::new(&mut data)) };
                return Some(picture);
            }
            // `picture` holds nothing after a failure; dropping it is a no-op.
            std::mem::forget(picture);
        }
        // SAFETY: as above.
        unsafe { dav1d_data_unref(NonNull::new(&mut data)) };
        None
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context came from `dav1d_open`; `dav1d_close` empties
        // the option, so it is closed once.
        unsafe { dav1d_close(NonNull::new(&mut self.0)) };
    }
}

struct Picture(Dav1dPicture);

impl Drop for Picture {
    fn drop(&mut self) {
        if self.0.data[0].is_some() {
            // SAFETY: the picture was returned by `dav1d_get_picture`.
            unsafe { dav1d_picture_unref(NonNull::new(&mut self.0)) };
        }
    }
}

/// A decoded plane, read through safe slices.
struct Plane<'a> {
    bytes: &'a [u8],
    stride: usize,
    wide: bool,
}

impl Plane<'_> {
    #[inline]
    fn get(&self, x: usize, y: usize) -> u32 {
        if self.wide {
            let at = y * self.stride + x * 2;
            u32::from(u16::from_ne_bytes([self.bytes[at], self.bytes[at + 1]]))
        } else {
            u32::from(self.bytes[y * self.stride + x])
        }
    }
}

impl Picture {
    fn size(&self) -> (u32, u32) {
        (self.0.p.w.max(0) as u32, self.0.p.h.max(0) as u32)
    }

    fn bits(&self) -> u32 {
        self.0.p.bpc.clamp(0, 16) as u32
    }

    /// Plane `index` of `width` × `height` samples, checked against the
    /// pointer and stride `rav1d` returned.
    fn plane(&self, index: usize, width: u32, height: u32) -> Option<Plane<'_>> {
        let wide = self.0.p.bpc > 8;
        let stride = usize::try_from(self.0.stride[usize::from(index > 0)]).ok()?;
        let row_bytes = width as usize * if wide { 2 } else { 1 };
        if width == 0 || height == 0 || stride < row_bytes {
            return None;
        }
        let pointer: NonNull<c_void> = self.0.data[index]?;
        let length = stride
            .checked_mul(height as usize - 1)?
            .checked_add(row_bytes)?;
        // SAFETY: `rav1d` allocated `height` rows of `stride` bytes for this plane
        // and keeps them alive until `Picture` is dropped; only the last row is
        // read short of its stride.
        let bytes = unsafe { std::slice::from_raw_parts(pointer.as_ptr().cast::<u8>(), length) };
        Some(Plane {
            bytes,
            stride,
            wide,
        })
    }
}

/// How to turn the decoded samples into RGB.
struct Colour {
    /// Luma weights; `None` is the identity matrix (the planes are G, B, R).
    kr_kb: Option<(f32, f32)>,
    full_range: bool,
}

fn colour(sequence: &Sequence) -> Option<Colour> {
    let kr_kb = match sequence.matrix {
        0 => None,
        1 | 2 => Some((0.2126, 0.0722)),
        5 | 6 => Some((0.299, 0.114)),
        7 => Some((0.212, 0.087)),
        9 => Some((0.2627, 0.0593)),
        _ => return None,
    };
    if kr_kb.is_none() && sequence.layout != I444 {
        return None;
    }
    Some(Colour {
        kr_kb,
        full_range: sequence.full_range,
    })
}

/// Reduces the picture(s) to at most `thumb_width` × `thumb_height` pixels
/// (never enlarging), averaging the source pixels of each output pixel.
fn reduce(
    primary: &Picture,
    alpha: Option<&Picture>,
    colour: &Colour,
    premultiplied: bool,
    (thumb_width, thumb_height): (u32, u32),
    cancel: &CancelFlag,
) -> Result<RgbaImage, LoadError> {
    let (width, height) = primary.size();
    let layout = primary.0.p.layout;
    let (shift_x, shift_y) = match layout {
        I420 => (1, 1),
        I422 => (1, 0),
        _ => (0, 0),
    };
    let malformed = || LoadError::Malformed;
    let luma = primary.plane(0, width, height).ok_or_else(malformed)?;
    let chroma = if layout == I400 {
        None
    } else {
        let (cw, ch) = (width.div_ceil(1 << shift_x), height.div_ceil(1 << shift_y));
        Some((
            primary.plane(1, cw, ch).ok_or_else(malformed)?,
            primary.plane(2, cw, ch).ok_or_else(malformed)?,
        ))
    };
    let alpha = match alpha {
        Some(alpha) => Some((
            alpha.plane(0, width, height).ok_or_else(malformed)?,
            alpha.bits(),
        )),
        None => None,
    };

    let bits = primary.bits();
    let max = ((1u32 << bits) - 1) as f32;
    let scale = (1 << (bits - 8)) as f32;
    let (y_offset, y_range, c_range) = if colour.full_range {
        (0.0, max, max)
    } else {
        (16.0 * scale, 219.0 * scale, 224.0 * scale)
    };
    let mid = (1u32 << (bits - 1)) as f32;

    let cells = thumb_width as usize * thumb_height as usize;
    // Sums of red, green, blue (weighted by alpha unless premultiplied), alpha, count.
    let mut sums = vec![[0u64; 5]; cells];
    for y in 0..height as usize {
        if y % 256 == 0 && cancel.is_cancelled() {
            return Err(LoadError::Cancelled);
        }
        let cell_row = (y * thumb_height as usize / height as usize) * thumb_width as usize;
        for x in 0..width as usize {
            let luma_value = (luma.get(x, y) as f32 - y_offset) / y_range;
            let (r, g, b) = match &chroma {
                None => (luma_value, luma_value, luma_value),
                Some((u_plane, v_plane)) => {
                    let u = (u_plane.get(x >> shift_x, y >> shift_y) as f32 - mid) / c_range;
                    let v = (v_plane.get(x >> shift_x, y >> shift_y) as f32 - mid) / c_range;
                    match colour.kr_kb {
                        Some((kr, kb)) => {
                            let r = luma_value + 2.0 * (1.0 - kr) * v;
                            let b = luma_value + 2.0 * (1.0 - kb) * u;
                            let g = (luma_value - kr * r - kb * b) / (1.0 - kr - kb);
                            (r, g, b)
                        }
                        // Identity: the planes are G, B and R.
                        None => (
                            (v_plane.get(x, y) as f32 - y_offset) / y_range,
                            luma_value,
                            (u_plane.get(x, y) as f32 - y_offset) / y_range,
                        ),
                    }
                }
            };
            let a = alpha.as_ref().map_or(255, |(plane, alpha_bits)| {
                let value = plane.get(x, y);
                ((value << 8 >> alpha_bits).min(255)) as u64
            });
            let to_byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u64;
            let weight = if premultiplied { 255 } else { a };
            let cell = &mut sums[cell_row + x * thumb_width as usize / width as usize];
            cell[0] += to_byte(r) * weight;
            cell[1] += to_byte(g) * weight;
            cell[2] += to_byte(b) * weight;
            cell[3] += a;
            cell[4] += 1;
        }
    }

    let mut pixels = Vec::with_capacity(cells * 4);
    for [r, g, b, a, count] in sums {
        if a == 0 {
            // Fully transparent: colour does not matter.
            pixels.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let channel = |sum: u64| (sum + a / 2) / a;
        pixels.extend_from_slice(&[
            channel(r).min(255) as u8,
            channel(g).min(255) as u8,
            channel(b).min(255) as u8,
            ((a + count / 2) / count) as u8,
        ]);
    }
    RgbaImage::from_raw(thumb_width, thumb_height, pixels).ok_or(LoadError::Malformed)
}

/// Decodes the thumbnail of an AVIF file. The caller holds the decode gate.
pub(crate) fn thumbnail(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<Thumbnail, LoadError> {
    let context = catch_unwind(AssertUnwindSafe(|| {
        mp4parse::read_avif(&mut Cursor::new(bytes), ParseStrictness::Normal)
    }))
    .map_err(|_| LoadError::Malformed)?
    .map_err(|_| LoadError::Malformed)?;
    let coded = context
        .primary_item_coded_data()
        .ok_or(LoadError::Unsupported)?;
    let alpha_coded = context
        .alpha_item_coded_data()
        .filter(|data| !data.is_empty());
    let rotation = context.image_rotation().unwrap_or(ImageRotation::D0);

    let sequence = sequence_header(coded).ok_or(LoadError::Malformed)?;
    if sequence.width == 0 || sequence.height == 0 {
        return Err(LoadError::Malformed);
    }
    if sequence.width > limits.max_source_side
        || sequence.height > limits.max_source_side
        || u64::from(sequence.width) * u64::from(sequence.height) > limits.max_source_pixels
    {
        return Err(LoadError::TooLarge);
    }
    if budget_bytes(&sequence, alpha_coded.is_some(), bytes.len()) > limits.max_decode_bytes {
        return Err(LoadError::TooLarge);
    }
    let colour = colour(&sequence).ok_or(LoadError::Unsupported)?;
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }

    let frame_limit = u32::try_from(limits.max_source_pixels).unwrap_or(u32::MAX);
    let primary = Decoder::new(frame_limit)
        .and_then(|decoder| decoder.decode(coded))
        .ok_or(LoadError::Malformed)?;
    // The picture must be the size the header promised; the planes were
    // budgeted for that.
    let (width, height) = primary.size();
    if width == 0
        || height == 0
        || width > sequence.width
        || height > sequence.height
        || !(8..=12).contains(&primary.bits())
        || primary.0.p.layout != sequence.layout
    {
        return Err(LoadError::Malformed);
    }
    let alpha = match alpha_coded {
        Some(alpha_coded) => {
            let picture = Decoder::new(frame_limit)
                .and_then(|decoder| decoder.decode(alpha_coded))
                .ok_or(LoadError::Malformed)?;
            if picture.size() != (width, height)
                || picture.0.p.layout != I400
                || !(8..=12).contains(&picture.bits())
            {
                return Err(LoadError::Malformed);
            }
            Some(picture)
        }
        None => None,
    };

    let (rotated_width, rotated_height) = match rotation {
        ImageRotation::D90 | ImageRotation::D270 => (height, width),
        _ => (width, height),
    };
    let (thumb_width, thumb_height) = decode::fit(
        rotated_width,
        rotated_height,
        limits.thumbnail_width,
        limits.thumbnail_height,
    );
    let (cell_width, cell_height) = match rotation {
        ImageRotation::D90 | ImageRotation::D270 => (thumb_height, thumb_width),
        _ => (thumb_width, thumb_height),
    };
    let image = reduce(
        &primary,
        alpha.as_ref(),
        &colour,
        context.premultiplied_alpha,
        (cell_width, cell_height),
        cancel,
    )?;
    drop(alpha);
    drop(primary);
    // `irot` turns anticlockwise; `imageops` turns clockwise.
    let image = match rotation {
        ImageRotation::D0 => image,
        ImageRotation::D90 => imageops::rotate270(&image),
        ImageRotation::D180 => imageops::rotate180(&image),
        ImageRotation::D270 => imageops::rotate90(&image),
    };
    Ok(Thumbnail {
        width: thumb_width,
        height: thumb_height,
        source_width: rotated_width,
        source_height: rotated_height,
        bgra: decode::into_bgra(image::DynamicImage::ImageRgba8(image)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPAQUE8: &[u8] = include_bytes!("testdata/opaque8.avif");
    const OPAQUE10: &[u8] = include_bytes!("testdata/opaque10.avif");
    const ALPHA: &[u8] = include_bytes!("testdata/alpha.avif");
    const CHROMA_444: &[u8] = include_bytes!("testdata/c444.avif");

    fn decode(bytes: &[u8], limits: &Limits) -> Result<Thumbnail, LoadError> {
        thumbnail(bytes, limits, &CancelFlag::default())
    }

    /// The fixtures are 120 × 80 gradients from red (top) to blue (bottom).
    #[test]
    fn decodes_8_and_10_bit_and_444_gradients() {
        for (name, bytes) in [
            ("8 bit", OPAQUE8),
            ("10 bit", OPAQUE10),
            ("4:4:4", CHROMA_444),
        ] {
            let thumb = decode(bytes, &Limits::default()).unwrap();
            assert_eq!((thumb.width, thumb.height), (120, 80), "{name}");
            assert_eq!(thumb.bgra.len(), 120 * 80 * 4);
            let top = &thumb.bgra[..4];
            assert!(
                top[2] > 220 && top[0] < 40 && top[3] == 255,
                "{name}: {top:?}"
            );
            let bottom = &thumb.bgra[thumb.bgra.len() - 4..];
            assert!(bottom[0] > 220 && bottom[2] < 40, "{name}: {bottom:?}");
        }
    }

    #[test]
    fn a_separate_alpha_item_becomes_transparency() {
        let thumb = decode(ALPHA, &Limits::default()).unwrap();
        // The mask runs from opaque (top) to transparent (bottom).
        assert!(thumb.bgra[3] > 240, "{:?}", &thumb.bgra[..4]);
        assert_eq!(thumb.bgra[thumb.bgra.len() - 1], 0);
    }

    #[test]
    fn large_images_are_averaged_down_and_never_enlarged() {
        let limits = Limits {
            thumbnail_width: 60,
            thumbnail_height: 60,
            ..Limits::default()
        };
        let thumb = decode(OPAQUE8, &limits).unwrap();
        assert_eq!((thumb.width, thumb.height), (60, 40));
        assert_eq!((thumb.source_width, thumb.source_height), (120, 80));
        let middle = &thumb.bgra[(20 * 60 + 30) * 4..][..4];
        // Halfway between red and blue.
        assert!(
            middle[0] > 80 && middle[0] < 180 && middle[2] > 80 && middle[2] < 180,
            "{middle:?}"
        );
    }

    #[test]
    fn limits_apply_before_decoding() {
        let side = Limits {
            max_source_side: 100,
            ..Limits::default()
        };
        assert_eq!(decode(OPAQUE8, &side), Err(LoadError::TooLarge));
        let pixels = Limits {
            max_source_pixels: 9_000,
            ..Limits::default()
        };
        assert_eq!(decode(OPAQUE8, &pixels), Err(LoadError::TooLarge));
        let memory = Limits {
            max_decode_bytes: 20_000,
            ..Limits::default()
        };
        assert_eq!(decode(OPAQUE8, &memory), Err(LoadError::TooLarge));
        // Alpha needs a second plane.
        let enough = Limits {
            max_decode_bytes: 120 * 128 * 128 * 4,
            ..Limits::default()
        };
        assert!(decode(ALPHA, &enough).is_ok());
        let cancel = CancelFlag::default();
        cancel.cancel();
        assert_eq!(
            thumbnail(OPAQUE8, &Limits::default(), &cancel),
            Err(LoadError::Cancelled)
        );
    }

    #[test]
    fn damaged_files_are_errors() {
        for cut in [0, 8, 20, 100, 200, OPAQUE8.len() - 10] {
            let result = decode(&OPAQUE8[..cut], &Limits::default());
            assert!(result.is_err() || cut > 300, "cut at {cut}: {result:?}");
        }
        let mut zeroed = OPAQUE8.to_vec();
        let at = zeroed.windows(4).position(|w| w == b"mdat").unwrap() + 4;
        zeroed[at..].fill(0);
        assert!(decode(&zeroed, &Limits::default()).is_err());
        let mut noise = OPAQUE8.to_vec();
        for (index, byte) in noise[at..].iter_mut().enumerate() {
            *byte = (index * 37 + 11) as u8;
        }
        assert!(decode(&noise, &Limits::default()).is_err());
    }
}
