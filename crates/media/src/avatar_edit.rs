//! Preparing our own avatar image before it is uploaded: the user picks a
//! square of a photo, which is scaled down to a small image.
//!
//! Decoding uses the preview limits (header checked before any pixel buffer,
//! capped decoder allocation, one decode at a time in the process) and
//! follows the EXIF orientation, so phone photos are upright. The decoded
//! image is kept at most [`MAX_WORKING_SIDE`] pixels a side while the editor
//! is open. Nothing is fetched or uploaded here.

use std::io::Cursor;

use cayenchat_model::attachment::ImageFormat as Sniffed;
use image::{
    DynamicImage, ImageDecoder, ImageFormat, ImageReader, codecs::jpeg::JpegEncoder,
    imageops::FilterType,
};

use crate::{Limits, LoadError, decode::Thumbnail};

/// Side of the uploaded avatar in pixels. Clients show avatars small (this
/// one at 16 logical pixels); 256 leaves room for larger displays and
/// `{size}`-aware hosts without uploading the whole photo.
pub const AVATAR_OUTPUT_SIDE: u32 = 256;
/// The decoded image is reduced to this while being edited.
pub const MAX_WORKING_SIDE: u32 = 2048;
/// Smallest square the user can select, in working pixels.
const MIN_CROP_SIDE: f64 = 32.0;
const JPEG_QUALITY: u8 = 88;

/// A decoded, upright image being edited.
pub struct AvatarSource {
    image: DynamicImage,
    has_alpha: bool,
}

impl std::fmt::Debug for AvatarSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarSource")
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}

/// The selected square, in working-image pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub side: f64,
}

/// The image to upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedAvatar {
    pub bytes: Vec<u8>,
    /// `png` or `jpg`, for the upload's file name.
    pub extension: &'static str,
}

/// Decodes `bytes` for editing. PNG, JPEG, GIF (first frame), WebP, BMP and
/// TIFF are accepted; HEIC and AVIF are not decodable here.
pub fn open(bytes: &[u8]) -> Result<AvatarSource, LoadError> {
    let format = match Sniffed::sniff(bytes) {
        Some(Sniffed::Png) => ImageFormat::Png,
        Some(Sniffed::Jpeg) => ImageFormat::Jpeg,
        Some(Sniffed::Gif) => ImageFormat::Gif,
        Some(Sniffed::Webp) => ImageFormat::WebP,
        Some(Sniffed::Bmp) => ImageFormat::Bmp,
        Some(Sniffed::Tiff) => ImageFormat::Tiff,
        Some(_) => return Err(LoadError::Unsupported),
        None => return Err(LoadError::NotImage),
    };
    let limits = Limits::default();
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
    let _decoding = crate::decode::DECODE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_source_side);
    decoder_limits.max_image_height = Some(limits.max_source_side);
    decoder_limits.max_alloc = Some(limits.max_decode_bytes);
    reader.limits(decoder_limits);
    let failed = |error: image::ImageError| match error {
        image::ImageError::Limits(_) => LoadError::TooLarge,
        _ => LoadError::Malformed,
    };
    let mut decoder = reader.into_decoder().map_err(failed)?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).map_err(failed)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    if image.width() > MAX_WORKING_SIDE || image.height() > MAX_WORKING_SIDE {
        image = image.resize(MAX_WORKING_SIDE, MAX_WORKING_SIDE, FilterType::Triangle);
    }
    let has_alpha = image.color().has_alpha() && image.to_rgba8().pixels().any(|p| p[3] < 255);
    Ok(AvatarSource { image, has_alpha })
}

impl AvatarSource {
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// The whole image fitted into `max_side` pixels, for the editor.
    pub fn preview(&self, max_side: u32) -> Thumbnail {
        let (width, height) = crate::decode::fit(self.width(), self.height(), max_side, max_side);
        let small = self.image.thumbnail_exact(width, height);
        let mut bgra = small.into_rgba8().into_raw();
        for pixel in bgra.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Thumbnail {
            width,
            height,
            source_width: self.width(),
            source_height: self.height(),
            bgra,
        }
    }

    /// The largest centered square: the starting selection.
    pub fn initial_crop(&self) -> Crop {
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let side = width.min(height);
        Crop {
            x: (width - side) / 2.0,
            y: (height - side) / 2.0,
            side,
        }
    }

    /// `crop` kept inside the image, no larger than it and not tiny.
    pub fn clamp(&self, crop: Crop) -> Crop {
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let side = crop
            .side
            .clamp(MIN_CROP_SIDE.min(width.min(height)), width.min(height));
        Crop {
            x: crop.x.clamp(0.0, width - side),
            y: crop.y.clamp(0.0, height - side),
            side,
        }
    }

    /// Moves the selection by working pixels.
    pub fn moved(&self, crop: Crop, dx: f64, dy: f64) -> Crop {
        self.clamp(Crop {
            x: crop.x + dx,
            y: crop.y + dy,
            ..crop
        })
    }

    /// Resizes the selection by `factor` around its center (below 1 zooms
    /// in).
    pub fn scaled(&self, crop: Crop, factor: f64) -> Crop {
        let side = crop.side * factor;
        let center = (crop.x + crop.side / 2.0, crop.y + crop.side / 2.0);
        self.clamp(Crop {
            x: center.0 - side / 2.0,
            y: center.1 - side / 2.0,
            side,
        })
    }

    /// The selected square scaled to [`AVATAR_OUTPUT_SIDE`] (never
    /// enlarged beyond the selection), as PNG when the image has
    /// transparency and JPEG otherwise.
    pub fn encode(&self, crop: Crop) -> Result<EncodedAvatar, LoadError> {
        let crop = self.clamp(crop);
        let side = crop.side.round().max(1.0) as u32;
        let x = (crop.x.round() as u32).min(self.width() - side);
        let y = (crop.y.round() as u32).min(self.height() - side);
        let square = self.image.crop_imm(x, y, side, side);
        let out = side.min(AVATAR_OUTPUT_SIDE);
        let square = if out == side {
            square
        } else {
            square.resize_exact(out, out, FilterType::Lanczos3)
        };
        let mut bytes = Vec::new();
        let extension = if self.has_alpha {
            square
                .to_rgba8()
                .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
                .map_err(|_| LoadError::Malformed)?;
            "png"
        } else {
            JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
                .encode_image(&square.to_rgb8())
                .map_err(|_| LoadError::Malformed)?;
            "jpg"
        };
        Ok(EncodedAvatar { bytes, extension })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::encode;

    #[test]
    fn large_photos_become_a_small_square_of_the_chosen_part() {
        let bytes = encode(ImageFormat::Jpeg, 3000, 2000);
        let source = open(&bytes).unwrap();
        // Reduced for editing, aspect kept.
        assert_eq!((source.width(), source.height()), (2048, 1365));
        let crop = source.initial_crop();
        assert_eq!(crop.side, 1365.0);
        assert!((crop.x - (2048.0 - 1365.0) / 2.0).abs() < 1.0);
        let out = source.encode(crop).unwrap();
        assert_eq!(out.extension, "jpg");
        let decoded = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
        assert!(out.bytes.len() < 64 * 1024, "{}", out.bytes.len());
        // A small selection is not enlarged.
        let small = source.clamp(Crop {
            x: 10.0,
            y: 10.0,
            side: 100.0,
        });
        let decoded = image::load_from_memory(&source.encode(small).unwrap().bytes).unwrap();
        assert_eq!(decoded.width(), 100);
    }

    #[test]
    fn the_selection_stays_inside_the_image() {
        let source = open(&encode(ImageFormat::Png, 400, 200)).unwrap();
        let crop = source.initial_crop();
        assert_eq!(
            crop,
            Crop {
                x: 100.0,
                y: 0.0,
                side: 200.0
            }
        );
        let moved = source.moved(crop, 1000.0, -50.0);
        assert_eq!((moved.x, moved.y), (200.0, 0.0));
        let zoomed = source.scaled(moved, 0.5);
        assert_eq!(zoomed.side, 100.0);
        assert_eq!((zoomed.x, zoomed.y), (250.0, 50.0), "around the center");
        assert_eq!(source.scaled(zoomed, 10.0).side, 200.0, "at most the image");
        assert_eq!(source.scaled(zoomed, 0.001).side, MIN_CROP_SIDE);
        let preview = source.preview(320);
        assert_eq!((preview.width, preview.height), (320, 160));
        assert_eq!(preview.bgra.len(), 320 * 160 * 4);
    }

    #[test]
    fn transparency_is_kept_as_png() {
        let mut image = image::RgbaImage::new(64, 64);
        image.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        let source = open(&bytes).unwrap();
        assert_eq!(
            source.encode(source.initial_crop()).unwrap().extension,
            "png"
        );
        // Opaque PNG screenshots become JPEG.
        let opaque = open(&encode(ImageFormat::Png, 64, 64)).unwrap();
        assert_eq!(
            opaque.encode(opaque.initial_crop()).unwrap().extension,
            "jpg"
        );
    }

    #[test]
    fn unsupported_oversized_and_broken_input_is_refused() {
        assert_eq!(open(b"not an image").unwrap_err(), LoadError::NotImage);
        let mut heic = vec![0, 0, 0, 24];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        assert_eq!(open(&heic).unwrap_err(), LoadError::Unsupported);
        let mut broken = encode(ImageFormat::Png, 64, 64);
        broken.truncate(60);
        assert!(open(&broken).is_err());
        // Beyond the preview decode limits.
        let huge = encode(ImageFormat::Png, 8193, 1);
        assert_eq!(open(&huge).unwrap_err(), LoadError::TooLarge);
    }

    #[test]
    fn bmp_and_tiff_from_the_clipboard_can_be_edited() {
        for format in [ImageFormat::Bmp, ImageFormat::Tiff] {
            let mut bytes = Vec::new();
            image::RgbImage::from_pixel(40, 30, image::Rgb([0, 128, 255]))
                .write_to(&mut Cursor::new(&mut bytes), format)
                .unwrap();
            let source = open(&bytes).unwrap();
            assert_eq!((source.width(), source.height()), (40, 30), "{format:?}");
        }
    }
}
