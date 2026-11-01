// SPDX-License-Identifier: MPL-2.0
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

pub mod avatar_edit;
pub mod cache;
pub mod decode;
pub mod fetch;
pub mod policy;
#[cfg(test)]
mod testing;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
    /// Crop the centered square before resizing (avatars).
    pub crop_square: bool,
    /// Shrink with Lanczos3, premultiplied when the image has transparency
    /// (avatars). Costs an RGBA copy of the source and a slower filter, so
    /// previews keep the cheaper `thumbnail_exact`.
    pub smooth_resize: bool,
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
            crop_square: false,
            smooth_resize: false,
        }
    }
}

impl Limits {
    /// Limits for small square avatars of `size` pixels a side: small
    /// responses and sources, since only a tiny thumbnail is kept.
    pub fn avatar(size: u32) -> Self {
        Self {
            max_response_bytes: 2 * 1024 * 1024,
            max_redirects: 3,
            connect_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_secs(10),
            max_source_pixels: 2048 * 2048,
            max_source_side: 4096,
            max_decode_bytes: 24 * 1024 * 1024,
            thumbnail_width: size,
            thumbnail_height: size,
            crop_square: true,
            smooth_resize: true,
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
    load_thumbnail_confirmed(source, fetcher, limits, cancel, &|| {})
}

/// [`load_thumbnail`] that calls `confirmed` once the fetcher knows the
/// source is an image (see [`Fetcher::fetch_confirmed`]). It is called on
/// the loading thread, at most once, and not at all for sources that turn
/// out not to be images.
pub fn load_thumbnail_confirmed(
    source: &MediaRef,
    fetcher: &dyn Fetcher,
    limits: &Limits,
    cancel: &CancelFlag,
    confirmed: &dyn Fn(),
) -> Result<Thumbnail, LoadError> {
    let bytes = fetcher.fetch_confirmed(source, limits, cancel, confirmed)?;
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    decode::thumbnail(&bytes, limits, cancel)
}

/// Avatar originals kept for resizing are at most this many bytes...
pub const ORIGINAL_MAX_BYTES: usize = 256 * 1024;
/// ...and at most this many pixels on each side.
pub const ORIGINAL_MAX_SIDE: u32 = 512;

/// The fetched bytes of one avatar URL, shared by every size made from it.
/// Filled by the first load, only when the image is small enough to keep.
/// The bytes stay untrusted: every size is decoded from them through the
/// same checks as a fresh download.
#[derive(Clone, Debug, Default)]
pub struct Original(Arc<OriginalInner>);

#[derive(Debug, Default)]
struct OriginalInner {
    /// Held across a fetch, so only background loads take it.
    bytes: std::sync::Mutex<Option<Arc<[u8]>>>,
    /// Size of `bytes`, readable without the lock (the UI accounts with it
    /// and must not wait for a fetch in flight).
    len: AtomicUsize,
}

impl Original {
    /// Bytes held (0 while empty). Never waits for a load.
    pub fn len(&self) -> usize {
        self.0.len.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<[u8]>>> {
        self.0
            .bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Like [`load_thumbnail`] for an avatar, made from `original` when it holds
/// the bytes. Otherwise fetches, keeps the bytes in `original` if they are
/// small (see [`ORIGINAL_MAX_BYTES`]) and decodes. Loads of the same
/// `original` run one after another, so a second size waits for the first
/// fetch instead of fetching again. Blocking; run it on a background thread.
pub fn load_avatar_thumbnail(
    source: &MediaRef,
    fetcher: &dyn Fetcher,
    limits: &Limits,
    cancel: &CancelFlag,
    original: &Original,
) -> Result<Thumbnail, LoadError> {
    let mut held = original.lock();
    let bytes = match held.as_ref() {
        Some(bytes) => bytes.clone(),
        None => {
            let bytes: Arc<[u8]> = fetcher.fetch(source, limits, cancel)?.into();
            if decode::is_small_original(&bytes) {
                *held = Some(bytes.clone());
                original.0.len.store(bytes.len(), Ordering::Release);
            }
            bytes
        }
    };
    drop(held);
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

    #[test]
    fn extensionless_avatar_endpoints_load_but_must_be_images() {
        let mut routes = HashMap::new();
        routes.insert(
            "/avatar/32/u1".to_string(),
            Route::image("image/webp", encode(ImageFormat::WebP, 120, 80)),
        );
        routes.insert(
            "/avatar/32/page".to_string(),
            Route::image("text/html", b"<!doctype html>".to_vec()),
        );
        routes.insert(
            "/avatar/32/svg".to_string(),
            Route::image("image/svg+xml", b"<svg/>".to_vec()),
        );
        let server = FixtureServer::start(routes);
        let limits = Limits::avatar(32);
        let fetcher = HttpFetcher::for_local_fixture(&limits);
        let load = |path: &str| {
            let source = MediaRef::Link(url::Url::parse(&server.url(path)).unwrap());
            load_thumbnail(&source, &fetcher, &limits, &CancelFlag::default())
        };
        let avatar = load("/avatar/32/u1").unwrap();
        assert_eq!((avatar.width, avatar.height), (32, 32));
        assert_eq!(load("/avatar/32/page"), Err(LoadError::NotImage));
        assert_eq!(load("/avatar/32/svg"), Err(LoadError::NotImage));
    }

    #[test]
    fn small_avatar_originals_are_kept_and_serve_every_size_from_one_fetch() {
        // Noise does not compress: 500×500 stays within 512 px but not
        // within 256 KiB.
        let noise = |side: u32| {
            let mut state = 0x2545_F491_u32;
            let image = image::RgbaImage::from_fn(side, side, |_, _| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let [a, b, c, _] = state.to_be_bytes();
                image::Rgba([a, b, c, 255])
            });
            let mut bytes = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
                .unwrap();
            bytes
        };
        let heavy = noise(500);
        assert!(heavy.len() > ORIGINAL_MAX_BYTES);
        let mut routes = HashMap::new();
        routes.insert(
            "/small".to_string(),
            Route::image("image/png", encode(ImageFormat::Png, 128, 96)),
        );
        routes.insert("/heavy".to_string(), Route::image("image/png", heavy));
        routes.insert(
            "/wide".to_string(),
            Route::image("image/png", encode(ImageFormat::Png, 513, 100)),
        );
        let server = FixtureServer::start(routes);
        let base = Limits::avatar(32);
        let fetcher = HttpFetcher::for_local_fixture(&base);
        let load = |path: &str, pixels: u32, original: &Original| {
            let source = MediaRef::Link(url::Url::parse(&server.url(path)).unwrap());
            let limits = Limits {
                thumbnail_width: pixels,
                thumbnail_height: pixels,
                ..base
            };
            load_avatar_thumbnail(&source, &fetcher, &limits, &CancelFlag::default(), original)
        };

        let original = Original::default();
        for pixels in [32, 48, 96] {
            let thumbnail = load("/small", pixels, &original).unwrap();
            assert_eq!((thumbnail.width, thumbnail.height), (pixels, pixels));
        }
        assert_eq!(server.hits(), 1, "later sizes come from the kept bytes");
        assert!(!original.is_empty());

        // Too heavy or too wide: nothing is kept, each size fetches again.
        for path in ["/heavy", "/wide"] {
            let hits = server.hits();
            let original = Original::default();
            load(path, 32, &original).unwrap();
            load(path, 96, &original).unwrap();
            assert!(original.is_empty(), "{path}");
            assert_eq!(server.hits(), hits + 2, "{path}");
        }
    }

    #[test]
    fn counting_an_original_does_not_wait_for_a_fetch_in_flight() {
        use std::sync::mpsc;

        struct Gated {
            png: Vec<u8>,
            entered: mpsc::Sender<()>,
            release: std::sync::Mutex<mpsc::Receiver<()>>,
            calls: AtomicUsize,
        }
        impl Fetcher for Gated {
            fn fetch(
                &self,
                _: &MediaRef,
                _: &Limits,
                _: &CancelFlag,
            ) -> Result<Vec<u8>, LoadError> {
                if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
                    self.entered.send(()).unwrap();
                    self.release.lock().unwrap().recv().unwrap();
                }
                Ok(self.png.clone())
            }
        }

        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        // Too wide to keep, so every size fetches again.
        let fetcher = Gated {
            png: encode(ImageFormat::Png, 513, 100),
            entered: entered_tx,
            release: std::sync::Mutex::new(release_rx),
            calls: AtomicUsize::new(0),
        };
        let source = MediaRef::Link(url::Url::parse("https://example.test/a").unwrap());
        let limits = Limits::avatar(32);
        let original = Original::default();
        load_avatar_thumbnail(
            &source,
            &fetcher,
            &limits,
            &CancelFlag::default(),
            &original,
        )
        .unwrap();

        std::thread::scope(|scope| {
            let second = scope.spawn(|| {
                let limits = Limits::avatar(48);
                load_avatar_thumbnail(
                    &source,
                    &fetcher,
                    &limits,
                    &CancelFlag::default(),
                    &original,
                )
            });
            entered.recv().unwrap();
            // The second size is now inside its fetch, holding the slot.
            let (len_tx, len_rx) = mpsc::channel();
            let counter = original.clone();
            scope.spawn(move || len_tx.send(counter.len()).unwrap());
            let counted = len_rx.recv_timeout(Duration::from_secs(5));
            release_tx.send(()).unwrap();
            assert_eq!(counted, Ok(0), "len() waited for the fetch");
            second.join().unwrap().unwrap();
        });
    }
}
