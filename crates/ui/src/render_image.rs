// SPDX-License-Identifier: GPL-3.0-only
//! The one place a `media::Thumbnail` becomes a GPUI image, and the one rule
//! for what a cache is charged for it.

use std::sync::Arc;

use cayenchat_media::Thumbnail;
use gpui::RenderImage;

/// The image for GPUI and the bytes to charge a cache for it: twice the
/// pixels, because they stay in the `RenderImage` and are copied into the
/// GPU sprite atlas when drawn.
pub(crate) fn from_thumbnail(thumbnail: Thumbnail) -> (Arc<RenderImage>, usize) {
    let bytes = thumbnail.byte_len() * 2;
    let buffer = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.bgra)
        .expect("thumbnail buffer matches its size");
    (
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
        bytes,
    )
}
