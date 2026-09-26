//! Attachments the user chose to send, independent of how a protocol
//! transports them. IRC uploads them to an external host and sends a link;
//! a protocol with native media (such as Matrix) would upload them itself.

use std::{fmt, sync::Arc};

/// Largest attachment read into memory. A local guard against unbounded
/// reads, not a limit published by any hosting provider.
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;

/// How the user supplied the attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentSource {
    Clipboard,
    Drop,
}

/// Image formats recognized from their leading bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Tiff,
    /// HEIF with HEVC images, the default photo format on iPhone and in
    /// macOS Photos libraries.
    Heic,
    /// HEIF without a more specific brand.
    Heif,
    Avif,
}

impl ImageFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Heic => "image/heic",
            Self::Heif => "image/heif",
            Self::Avif => "image/avif",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Heic => "heic",
            Self::Heif => "heif",
            Self::Avif => "avif",
        }
    }

    /// Identifies the format from file content, not from a name or a
    /// declared type, so a renamed file cannot pass as an image.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"BM") && bytes.len() > 14 {
            Some(Self::Bmp)
        } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
            Some(Self::Tiff)
        } else {
            Self::sniff_heif(bytes)
        }
    }

    /// Reads the brands of an ISO base media `ftyp` box (ISO/IEC 23008-12
    /// for HEIF, AV1 Image File Format for AVIF). Video brands are ignored.
    fn sniff_heif(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
            return None;
        }
        let size = u32::from_be_bytes(bytes[..4].try_into().ok()?) as usize;
        let end = size.clamp(16, bytes.len().min(256));
        // The major brand, then compatible brands after the minor version.
        let brands = std::iter::once(&bytes[8..12]).chain(bytes[16..end].chunks_exact(4));
        let mut heif = false;
        for brand in brands {
            match brand {
                b"avif" | b"avis" => return Some(Self::Avif),
                b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"hevm" | b"hevs" => {
                    return Some(Self::Heic);
                }
                b"mif1" | b"msf1" => heif = true,
                _ => {}
            }
        }
        heif.then_some(Self::Heif)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentError {
    NotAnImage,
    TooLarge { limit: usize },
}

/// An image selected for sending. Cloning shares the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub format: ImageFormat,
    pub bytes: Arc<[u8]>,
    pub source: AttachmentSource,
}

impl fmt::Debug for Attachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attachment")
            .field("name", &self.name)
            .field("format", &self.format)
            .field("len", &self.bytes.len())
            .field("source", &self.source)
            .finish()
    }
}

impl Attachment {
    /// Accepts `bytes` only when they are a recognized image within the size
    /// guard. A missing name becomes `image.<ext>`.
    pub fn image(
        name: Option<&str>,
        bytes: impl Into<Arc<[u8]>>,
        source: AttachmentSource,
    ) -> Result<Self, AttachmentError> {
        let bytes = bytes.into();
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::TooLarge {
                limit: MAX_ATTACHMENT_BYTES,
            });
        }
        let format = ImageFormat::sniff(&bytes).ok_or(AttachmentError::NotAnImage)?;
        let name = name
            .map(|name| {
                name.chars()
                    .filter(|ch| !ch.is_control())
                    .collect::<String>()
            })
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("image.{}", format.extension()));
        Ok(Self {
            name,
            format,
            bytes,
            source,
        })
    }

    pub fn size_text(&self) -> String {
        format_size(self.bytes.len())
    }
}

/// A compact size such as `512 B`, `12.3 KB` or `4.0 MB` (1024-based).
pub fn format_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{:.1} MB", value / KB / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognized_by_content() {
        let png = b"\x89PNG\r\n\x1a\n rest".to_vec();
        let attachment = Attachment::image(None, png, AttachmentSource::Clipboard).unwrap();
        assert_eq!(attachment.format, ImageFormat::Png);
        assert_eq!(attachment.name, "image.png");
        assert_eq!(attachment.format.media_type(), "image/png");
        assert_eq!(
            Attachment::image(Some("a.png"), b"hello".to_vec(), AttachmentSource::Drop),
            Err(AttachmentError::NotAnImage)
        );
        let webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        assert_eq!(ImageFormat::sniff(&webp), Some(ImageFormat::Webp));
        assert_eq!(ImageFormat::sniff(b"GIF89a.."), Some(ImageFormat::Gif));
        assert_eq!(
            ImageFormat::sniff(b"\xff\xd8\xff\xe0"),
            Some(ImageFormat::Jpeg)
        );
    }

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compatible.len();
        let mut bytes = (size as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(major);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        for brand in compatible {
            bytes.extend_from_slice(*brand);
        }
        bytes.extend_from_slice(b"\0\0\0\x08meta");
        bytes
    }

    #[test]
    fn heif_family_is_recognized_by_brand() {
        // An iPhone photo: major brand heic, compatible mif1/heic.
        let heic = ftyp(b"heic", &[b"mif1", b"heic"]);
        assert_eq!(ImageFormat::sniff(&heic), Some(ImageFormat::Heic));
        let photo = Attachment::image(Some("IMG_0001.HEIC"), heic, AttachmentSource::Drop).unwrap();
        assert_eq!(photo.format.media_type(), "image/heic");
        assert_eq!(
            ImageFormat::sniff(&ftyp(b"mif1", &[b"mif1", b"heic"])),
            Some(ImageFormat::Heic)
        );
        assert_eq!(
            ImageFormat::sniff(&ftyp(b"avif", &[b"mif1", b"miaf"])),
            Some(ImageFormat::Avif)
        );
        assert_eq!(
            ImageFormat::sniff(&ftyp(b"mif1", &[b"mif1"])),
            Some(ImageFormat::Heif)
        );
        // MP4 and QuickTime videos share the box format but are not images.
        assert_eq!(ImageFormat::sniff(&ftyp(b"isom", &[b"mp41"])), None);
        assert_eq!(ImageFormat::sniff(&ftyp(b"qt  ", &[b"qt  "])), None);
    }

    #[test]
    fn oversized_input_is_rejected_and_names_are_cleaned() {
        let big = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        assert!(matches!(
            Attachment::image(None, big, AttachmentSource::Drop),
            Err(AttachmentError::TooLarge { .. })
        ));
        let gif = b"GIF89a....".to_vec();
        let named = Attachment::image(Some("shot\n.gif"), gif, AttachmentSource::Drop).unwrap();
        assert_eq!(named.name, "shot.gif");
        assert!(!format!("{named:?}").contains("GIF89a"));
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MB");
    }
}
