//! Inline media display: which media a message refers to, how it is fetched
//! and decoded into a small thumbnail, and a bounded cache of the results.
//! Independent of GPUI and of any chat protocol; the UI renders thumbnails.
//!
//! ```text
//! IRC message text --policy::image_link--> MediaRef::Link
//!                                             |
//!                         cache::PreviewCache (dedupe, queue, byte budget)
//!                                             |
//!                  load_thumbnail: Fetcher (fetch::HttpFetcher) -> decode
//!                                             |
//!                                  Thumbnail (BGRA) -> UI
//! ```
//!
//! This is not the IRC image *upload* path (`cayenchat-upload`); previews need
//! no provider or account.

pub mod cache;
pub mod decode;
pub mod fetch;
pub mod policy;
#[cfg(test)]
mod testing;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub use decode::Thumbnail;
pub use fetch::{Fetcher, HttpFetcher};

/// Media a message refers to. IRC can only carry links, recognized by
/// [`policy::image_link`]. A protocol with native media (for example a
/// Matrix `mxc://` reference with server-made thumbnails) would add its own
/// variant and a [`Fetcher`] with its own transport and authentication, and
/// reuse decoding and the cache.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MediaRef {
    /// A public HTTP(S) URL that looks like a direct image link.
    Link(url::Url),
}

/// Resource limits for one preview. See `spec/performance.md` for the
/// reasoning behind the defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Compressed response body.
    pub max_response_bytes: usize,
    pub max_redirects: u8,
    pub connect_timeout: Duration,
    /// Whole request, including DNS, redirects and the body.
    pub total_timeout: Duration,
    /// Width × height of the source image.
    pub max_source_pixels: u64,
    pub max_source_side: u32,
    /// Bytes the decoder may allocate for the full-size image.
    pub max_decode_bytes: u64,
    /// Thumbnail size in pixels. The UI shows it in a box half this size in
    /// logical pixels, so it stays sharp on 2× displays.
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_response_bytes: 8 * 1024 * 1024,
            max_redirects: 3,
            connect_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_secs(15),
            max_source_pixels: 4096 * 4096,
            max_source_side: 8192,
            max_decode_bytes: 48 * 1024 * 1024,
            thumbnail_width: 400,
            thumbnail_height: 200,
        }
    }
}

/// Set when the result is no longer wanted (previews were turned off).
/// Fetching checks it between reads and before decoding.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Why a preview is unavailable. The message keeps its ordinary text link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The URL or a redirect target is not allowed (scheme, credentials,
    /// port, or a loopback/private/link-local address).
    Blocked,
    /// DNS, connection or timeout failure.
    Network,
    /// A final HTTP status other than 200.
    Status(u16),
    TooManyRedirects,
    /// The response is not an image (declared type or content).
    NotImage,
    /// An image in a format previews do not decode.
    Unsupported,
    /// Response bytes, dimensions or decoder memory above the limits.
    TooLarge,
    /// The content could not be decoded.
    Malformed,
    Cancelled,
}

impl LoadError {
    /// Whether trying again later could succeed. Everything else is final
    /// for the rest of the session (as long as its record is retained).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network => true,
            Self::Status(status) => matches!(status, 408 | 429 | 500..=599),
            _ => false,
        }
    }
}

/// Fetches `source` and decodes it into a thumbnail. Blocking; run it on a
/// background thread.
pub fn load_thumbnail(
    source: &MediaRef,
    fetcher: &dyn Fetcher,
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<Thumbnail, LoadError> {
    let bytes = fetcher.fetch(source, limits, cancel)?;
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    decode::thumbnail(&bytes, limits, cancel)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use image::ImageFormat;

    use super::*;
    use crate::testing::{FixtureServer, Route, encode};

    #[test]
    fn loads_small_and_large_images_and_reports_malformed_content() {
        let large = encode(ImageFormat::Png, 3000, 2000);
        let mut routes = HashMap::new();
        routes.insert(
            "/small.jpg".to_string(),
            Route::image("image/jpeg", encode(ImageFormat::Jpeg, 32, 24)),
        );
        routes.insert("/large.png".to_string(), Route::image("image/png", large));
        routes.insert(
            "/broken.png".to_string(),
            Route::image("image/png", b"\x89PNG\r\n\x1a\nbroken".to_vec()),
        );
        // Declared as PNG but actually a web page.
        routes.insert(
            "/page.png".to_string(),
            Route::image("image/png", b"<!doctype html>".to_vec()),
        );
        let server = FixtureServer::start(routes);
        let limits = Limits::default();
        let fetcher = HttpFetcher::for_local_fixture(&limits);
        let load = |path: &str| {
            let source = MediaRef::Link(url::Url::parse(&server.url(path)).unwrap());
            load_thumbnail(&source, &fetcher, &limits, &CancelFlag::default())
        };
        let small = load("/small.jpg").unwrap();
        assert_eq!((small.width, small.height), (32, 24));
        let large = load("/large.png").unwrap();
        assert_eq!((large.width, large.height), (300, 200));
        assert_eq!((large.source_width, large.source_height), (3000, 2000));
        assert_eq!(load("/broken.png"), Err(LoadError::Malformed));
        assert_eq!(load("/page.png"), Err(LoadError::NotImage));
    }
}
