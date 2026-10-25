// SPDX-License-Identifier: GPL-3.0-only
//! User avatars beside main-log messages and in the member list.
//!
//! Which avatar a user or message has comes from `app::avatars` (protocol
//! independent; IRC fills it from `draft/metadata-2`). This module turns the
//! avatar reference into a small image with `cayenchat-media`, in its own
//! cache: a separate, small budget, so avatars and image previews never
//! evict each other, and fetches share one concurrency limit with previews.
//! Rows ask for their avatar while they are drawn, so only visible rows (and
//! the log list's overdraw) cause requests; a large roster fetches nothing
//! until its rows are on screen.
//!
//! The avatar slot has a fixed size smaller than a text line, so an arriving
//! image never changes a row's height. A user without an avatar, or whose
//! image failed, gets the client-drawn default avatar of their nickname
//! (`default_avatar`, at most 512 distinct SVGs kept here); a loading one
//! stays blank. With the setting off, rows have no slot at all, nothing is
//! looked up, fetched, decoded or drawn, and the fetcher with its idle
//! connections is dropped.

use std::{cell::RefCell, collections::HashMap, sync::Arc, time::Instant};

use cayenchat_media::{
    Fetcher, HttpFetcher, Limits, LoadError, MediaRef, Original, Thumbnail,
    cache::{CacheLimits, Job, Lookup, PreviewCache},
    policy,
};
use gpui::{Context, RenderImage, Window};

use crate::{ChatWindow, default_avatar};

/// Slot size in logical pixels, below the 20 px log and member line height.
pub const SLOT: f32 = 16.;
/// Thumbnail size in pixels (sharp on 2× displays), also the size asked for
/// through the registry's `{size}` placeholder.
pub const PIXELS: u32 = 32;
/// Pixels a side of the one size an avatar URL is fetched at (the registry's
/// `{size}` placeholder); every displayed size is made from that download.
pub const ORIGINAL_PIXELS: u32 = 128;
/// Largest default avatar `default_sized` rasterizes and largest thumbnail
/// `lookup` makes.
const MAX_DEFAULT_PIXELS: u32 = 512;
/// Avatar originals kept for resizing: at most this many, and this many
/// bytes together. The oldest go first; they are fetched again when needed.
const MAX_ORIGINALS: usize = 256;
const MAX_ORIGINAL_BYTES: usize = 8 * 1024 * 1024;

/// A ready image is cached per avatar URL and size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    source: MediaRef,
    pixels: u32,
}

/// The fetched bytes of avatar URLs, bounded in count and total size, least
/// recently used first out. Memory only.
struct Originals {
    entries: HashMap<MediaRef, OriginalEntry>,
    bytes: usize,
    clock: u64,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for Originals {
    fn default() -> Self {
        Self::new(MAX_ORIGINALS, MAX_ORIGINAL_BYTES)
    }
}

struct OriginalEntry {
    original: Original,
    bytes: usize,
    last_used: u64,
}

impl Originals {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
            max_entries,
            max_bytes,
        }
    }

    /// The slot loads of `source` share, made if needed.
    fn slot(&mut self, source: &MediaRef) -> Original {
        self.clock += 1;
        let entry = self
            .entries
            .entry(source.clone())
            .or_insert_with(|| OriginalEntry {
                original: Original::default(),
                bytes: 0,
                last_used: 0,
            });
        entry.last_used = self.clock;
        entry.original.clone()
    }

    /// Counts what a finished load left in the slot and enforces the limits.
    fn settle(&mut self, source: &MediaRef) {
        if let Some(entry) = self.entries.get_mut(source) {
            let held = entry.original.len();
            self.bytes = self.bytes - entry.bytes + held;
            entry.bytes = held;
        }
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let victim = self
                .entries
                .iter()
                .filter(|(other, _)| *other != source)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(other, _)| other.clone());
            let Some(victim) = victim else { break };
            if let Some(entry) = self.entries.remove(&victim) {
                self.bytes -= entry.bytes;
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}
/// Image fetches in flight across avatars and previews together, so turning
/// both on does not double the peak of network and memory use. Decoding is
/// already one at a time in the whole process (`media::decode`).
pub const MAX_MEDIA_LOADS: usize = 3;

fn cache_limits() -> CacheLimits {
    CacheLimits {
        max_in_flight: 2,
        // About two screens of distinct senders and members.
        max_queued: 32,
        max_records: 256,
        // 32×32 BGRA is 4 KiB, charged twice (CPU copy and GPU atlas):
        // 256 avatars fit, so the record limit is what normally binds.
        budget_bytes: 2 * 1024 * 1024,
        // Rows keep their height, so no row needs to hear back.
        max_waiters: 0,
        ..CacheLimits::default()
    }
}

/// The two cached panes that draw avatars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    MainLog,
    Members,
}

/// What a row's avatar slot shows.
pub enum Shown {
    Image(Arc<RenderImage>),
    /// Loading: the slot stays empty.
    Pending,
    /// No usable avatar or image; the slot stays empty.
    None,
}

/// An image that left the cache. It may still be in the last paint of
/// either pane, which a cached pane replays, so its GPU texture is freed
/// only after both panes have been drawn again.
struct Released {
    image: Arc<RenderImage>,
    main_log: bool,
    members: bool,
}

pub struct Avatars {
    cache: RefCell<PreviewCache<Arc<RenderImage>, (), Key>>,
    originals: RefCell<Originals>,
    limits: Limits,
    /// Present only while avatars are shown.
    fetcher: Option<Arc<dyn Fetcher>>,
    released: RefCell<Vec<Released>>,
    /// Default avatars by look and pixel size; bounded by the 512
    /// combinations times the few sizes in use.
    defaults: RefCell<HashMap<(default_avatar::Params, u32), Arc<gpui::Image>>>,
    #[cfg(test)]
    fetcher_override: Option<Arc<dyn Fetcher>>,
    #[cfg(test)]
    pub lookups: std::cell::Cell<usize>,
}

impl Avatars {
    pub fn new(enabled: bool) -> Self {
        let mut avatars = Self {
            cache: RefCell::new(PreviewCache::new(cache_limits())),
            originals: RefCell::default(),
            limits: Limits::avatar(PIXELS),
            fetcher: None,
            released: RefCell::new(Vec::new()),
            defaults: RefCell::new(HashMap::new()),
            #[cfg(test)]
            fetcher_override: None,
            #[cfg(test)]
            lookups: std::cell::Cell::new(0),
        };
        avatars.set_enabled(enabled);
        avatars
    }

    pub fn enabled(&self) -> bool {
        self.fetcher.is_some()
    }

    /// The default avatar drawn for `nickname`.
    pub fn default_for(&self, nickname: &str) -> Arc<gpui::Image> {
        self.default_sized(nickname, PIXELS)
    }

    /// The default avatar of `nickname` rasterized at `pixels` × `pixels`
    /// (GPUI draws an SVG at its own size, so a larger slot, such as the
    /// 48 px one in WHOIS, asks for twice its logical size instead of
    /// enlarging the 32 px image).
    pub fn default_sized(&self, nickname: &str, pixels: u32) -> Arc<gpui::Image> {
        // Bounds the cache even if a caller passes a wild size.
        let pixels = pixels.clamp(1, MAX_DEFAULT_PIXELS);
        let params = default_avatar::params(nickname);
        self.defaults
            .borrow_mut()
            .entry((params, pixels))
            .or_insert_with(|| {
                Arc::new(gpui::Image::from_bytes(
                    gpui::ImageFormat::Svg,
                    default_avatar::svg(params, pixels).into_bytes(),
                ))
            })
            .clone()
    }

    /// Default avatars made so far (tests, measurements).
    #[cfg(test)]
    pub fn defaults_made(&self) -> usize {
        self.defaults.borrow().len()
    }

    /// Off cancels loads, forgets every record and queues the images for
    /// release once both panes have redrawn without them.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.cache.get_mut().set_enabled(enabled);
        if !enabled {
            self.fetcher = None;
            self.defaults.get_mut().clear();
            self.originals.get_mut().clear();
        } else if self.fetcher.is_none() {
            #[cfg(test)]
            if let Some(fetcher) = &self.fetcher_override {
                self.fetcher = Some(fetcher.clone());
                return;
            }
            #[cfg(feature = "preview-fixture")]
            if let Some(fetcher) = crate::previews::fixture::DirectoryFetcher::from_env() {
                self.fetcher = Some(Arc::new(fetcher));
                return;
            }
            self.fetcher = Some(Arc::new(HttpFetcher::new(&self.limits)));
        }
    }

    /// The image for an avatar reference (an avatar URL template) at
    /// `pixels` × `pixels`, queued for loading if needed. The URL is always
    /// fetched at [`ORIGINAL_PIXELS`], whatever size is asked for. Call only
    /// for rows being drawn.
    pub fn lookup(&self, avatar: &str, pixels: u32) -> Shown {
        if !self.enabled() {
            return Shown::None;
        }
        #[cfg(test)]
        self.lookups.set(self.lookups.get() + 1);
        let Some(source) = policy::avatar_url(avatar, ORIGINAL_PIXELS) else {
            return Shown::None;
        };
        let key = Key {
            source,
            pixels: pixels.clamp(1, MAX_DEFAULT_PIXELS),
        };
        match self.cache.borrow_mut().request(&key, (), Instant::now()) {
            Lookup::Ready(image) => Shown::Image(image.clone()),
            Lookup::Pending => Shown::Pending,
            Lookup::None => Shown::None,
        }
    }

    pub fn in_flight(&self) -> usize {
        self.cache.borrow().in_flight()
    }

    /// Frees the GPU textures of images that left the cache once both
    /// avatar panes have been drawn since. Call it while `pane` redraws,
    /// never while it may be reused from its cache. Returns whether images
    /// still wait for the other pane.
    pub fn release(&self, pane: Pane, window: &mut Window) -> bool {
        let mut released = self.released.borrow_mut();
        released.extend(
            self.cache
                .borrow_mut()
                .take_released()
                .into_iter()
                .map(|image| Released {
                    image,
                    main_log: false,
                    members: false,
                }),
        );
        for entry in released.iter_mut() {
            match pane {
                Pane::MainLog => entry.main_log = true,
                Pane::Members => entry.members = true,
            }
        }
        released.retain(|entry| {
            let done = entry.main_log && entry.members;
            if done {
                let _ = window.drop_image(entry.image.clone());
            }
            !done
        });
        !released.is_empty()
    }

    #[cfg(test)]
    pub fn use_fetcher(&mut self, fetcher: Arc<dyn Fetcher>) {
        if self.fetcher.is_some() {
            self.fetcher = Some(fetcher.clone());
        }
        self.fetcher_override = Some(fetcher);
    }

    #[cfg(test)]
    pub fn cache(&self) -> std::cell::Ref<'_, PreviewCache<Arc<RenderImage>, (), Key>> {
        self.cache.borrow()
    }

    /// Originals kept and their bytes (tests).
    #[cfg(test)]
    pub fn originals(&self) -> (usize, usize) {
        let originals = self.originals.borrow();
        (originals.entries.len(), originals.bytes)
    }

    #[cfg(test)]
    pub fn waiting_release(&self) -> usize {
        self.released.borrow().len()
    }
}

/// Wraps a thumbnail for GPUI; charged twice like previews (CPU copy and
/// GPU atlas tile).
fn to_image(thumbnail: Thumbnail) -> (Arc<RenderImage>, usize) {
    let bytes = thumbnail.byte_len() * 2;
    let buffer = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.bgra)
        .expect("thumbnail buffer matches its size");
    (
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
        bytes,
    )
}

impl ChatWindow {
    /// Media loads in flight (avatars and previews).
    pub(crate) fn media_loads(&self) -> usize {
        self.avatars.in_flight() + self.previews.in_flight()
    }

    /// Starts queued avatar loads while slots are free.
    pub(crate) fn pump_avatars(&self, cx: &mut Context<Self>) {
        let Some(fetcher) = self.avatars.fetcher.clone() else {
            return;
        };
        let limits = self.avatars.limits;
        while self.media_loads() < MAX_MEDIA_LOADS {
            let Some(job) = self.avatars.cache.borrow_mut().next_job() else {
                break;
            };
            let (key, cancel, fetcher) = (job.source.clone(), job.cancel.clone(), fetcher.clone());
            let original = self.avatars.originals.borrow_mut().slot(&key.source);
            let limits = Limits {
                thumbnail_width: key.pixels,
                thumbnail_height: key.pixels,
                ..limits
            };
            let work = cx.background_executor().spawn(async move {
                cayenchat_media::load_avatar_thumbnail(
                    &key.source,
                    fetcher.as_ref(),
                    &limits,
                    &cancel,
                    &original,
                )
                .map(to_image)
            });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                this.update(cx, |chat, cx| chat.avatar_finished(job, result, cx))
                    .ok();
            })
            .detach();
        }
    }

    fn avatar_finished(
        &mut self,
        job: Job<Key>,
        result: Result<(Arc<RenderImage>, usize), LoadError>,
        cx: &mut Context<Self>,
    ) {
        self.avatars.originals.get_mut().settle(&job.source.source);
        let finished = self
            .avatars
            .cache
            .get_mut()
            .finish(job, result, Instant::now());
        if finished.applied {
            // Both avatar panes are cached; the slot size does not change.
            self.refresh_whois_avatars(cx);
            cx.notify();
        }
        self.pump_avatars(cx);
        self.pump_previews(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::{Arc, Mutex},
    };

    use cayenchat_irc_core::Event;
    use cayenchat_media::{CancelFlag, Fetcher, Limits, LoadError, MediaRef};
    use cayenchat_model::NetworkId;
    use gpui::{Entity, Focusable, Pixels, TestAppContext, VisualTestContext};

    use crate::{ChatWindow, Worker};

    const BOB: &str = "https://avatars.example.com/u/bob?s={size}";
    const BOB_URL: &str = "https://avatars.example.com/u/bob?s=128";
    const CAROL: &str = "https://avatars.example.com/carol";

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 20, 20, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    /// Serves every URL with a small PNG unless it names `missing`, and
    /// records the requests. Never touches the network.
    #[derive(Default)]
    struct FakeFetcher {
        calls: Mutex<Vec<String>>,
    }

    impl FakeFetcher {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Fetcher for FakeFetcher {
        fn fetch(
            &self,
            source: &MediaRef,
            _: &Limits,
            cancel: &CancelFlag,
        ) -> Result<Vec<u8>, LoadError> {
            let url = match source {
                MediaRef::Link(url) => url.to_string(),
                _ => return Err(LoadError::Unsupported),
            };
            self.calls.lock().unwrap().push(url.clone());
            if cancel.is_cancelled() {
                return Err(LoadError::Cancelled);
            }
            if url.contains("missing") {
                return Err(LoadError::Status(404));
            }
            Ok(if url.contains("huge") {
                png(600, 600)
            } else if url.contains("big") {
                png(300, 300)
            } else {
                png(96, 64)
            })
        }
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        avatars: bool,
        previews: bool,
        fetcher: &Arc<FakeFetcher>,
    ) -> (Entity<ChatWindow>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
            crate::secrets::install_memory(cx);
            cx.set_global(crate::theme::Theme::new(
                cayenchat_storage::ThemeMode::Light,
                gpui::WindowAppearance::Light,
                &cayenchat_storage::Appearance::default(),
            ));
        });
        let mut settings = crate::settings_with_channels("#a");
        for server in &mut settings.servers {
            server.user_avatars = avatars;
        }
        settings.appearance.image_previews = previews;
        let fetcher: Arc<dyn Fetcher> = fetcher.clone();
        let (chat, cx) = cx.add_window_view(|window, cx| {
            let mut chat = ChatWindow::with_settings(settings, None, window, cx);
            chat.avatars.use_fetcher(fetcher.clone());
            chat.previews.use_fetcher(fetcher);
            chat
        });
        events(
            &chat,
            cx,
            vec![
                Event::Registered {
                    nickname: "me".into(),
                },
                Event::Joined {
                    channel: "#a".into(),
                },
            ],
        );
        chat.update(cx, |chat, cx| {
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        (chat, cx)
    }

    fn events(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, batch: Vec<Event>) {
        chat.update(cx, |chat, cx| {
            chat.handle_events(NetworkId(1), batch, Worker::Running, cx);
        });
        cx.run_until_parked();
    }

    fn avatar(nickname: &str, url: Option<&str>) -> Event {
        Event::UserAvatar {
            nickname: nickname.into(),
            url: url.map(Into::into),
        }
    }

    fn say(sender: &str, text: &str) -> Event {
        Event::ChannelMessage {
            channel: "#a".into(),
            sender: sender.into(),
            text: text.into(),
            notice: false,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        }
    }

    fn set_avatars(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, on: bool) {
        chat.update(cx, |chat, cx| {
            let mut settings = chat.saved.clone();
            for server in &mut settings.servers {
                server.user_avatars = on;
            }
            chat.apply_servers(settings, cx);
        });
        cx.run_until_parked();
    }

    /// Heights of the rows laid out on screen, by row index.
    fn row_heights(chat: &Entity<ChatWindow>, cx: &VisualTestContext) -> Vec<(usize, Pixels)> {
        chat.read_with(cx, |chat, _| {
            let list = &chat.main_lists[&chat.state.selection()].state;
            (0..list.item_count())
                .filter_map(|index| Some((index, list.bounds_for_item(index)?.size.height)))
                .collect()
        })
    }

    fn roster(count: usize) -> Vec<Event> {
        let mut users: Vec<String> = (0..count).map(|n| format!("user{n:03}")).collect();
        users.extend(["me".into(), "bob".into(), "carol".into()]);
        let mut batch = vec![Event::Names {
            channel: "#a".into(),
            users,
        }];
        batch.extend((0..count).map(|n| {
            let url = format!("https://avatars.example.com/member/{n}");
            avatar(&format!("user{n:03}"), Some(&url))
        }));
        batch
    }

    #[gpui::test]
    fn nothing_is_looked_up_or_fetched_while_avatars_are_off(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, false, false, &fetcher);
        let mut batch = roster(200);
        batch.extend([
            avatar("bob", Some(BOB)),
            say("bob", "hello"),
            say("me", "hi"),
        ]);
        events(&chat, cx, batch);
        chat.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert!(!chat.avatars.enabled());
            assert_eq!(chat.avatars.lookups.get(), 0, "no slot, no lookup");
            assert_eq!(chat.avatars.cache().records(), 0);
            // Metadata is still recorded, ready for when display is on.
            assert!(chat.state.avatars().current(NetworkId(1), "bob").is_some());
        });
        assert!(fetcher.calls().is_empty());
    }

    #[gpui::test]
    fn avatars_follow_each_servers_own_setting(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, false, false, &fetcher);
        let ids = chat.update(cx, |chat, cx| {
            let mut settings = chat.saved.clone();
            settings.add_server("b.example");
            chat.apply_servers(settings, cx);
            let ids: Vec<_> = chat
                .saved
                .servers
                .iter()
                .map(|server| chat.network_of_profile(&server.id).unwrap())
                .collect();
            assert!(!chat.avatars.enabled());
            assert!(ids.iter().all(|id| !chat.avatars_shown(*id)));
            ids
        });
        for (first, second) in [(true, false), (false, true), (false, false)] {
            chat.update(cx, |chat, cx| {
                let mut settings = chat.saved.clone();
                settings.servers[0].user_avatars = first;
                settings.servers[1].user_avatars = second;
                chat.apply_servers(settings, cx);
                assert_eq!(chat.avatars_shown(ids[0]), first);
                assert_eq!(chat.avatars_shown(ids[1]), second);
                // The shared resources live while any server shows avatars.
                assert_eq!(chat.avatars.enabled(), first || second);
            });
        }
    }

    #[gpui::test]
    fn visible_rows_load_each_avatar_once_when_turned_on_live(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, false, false, &fetcher);
        let mut batch = roster(300);
        batch.extend([
            avatar("bob", Some(BOB)),
            avatar("carol", Some(CAROL)),
            // Same image for two users: fetched once.
            avatar("dave", Some(CAROL)),
            avatar("erin", Some("https://avatars.example.com/missing")),
            // Not fetchable at all.
            avatar("mallory", Some("http://127.0.0.1/avatar")),
        ]);
        for n in 0..5 {
            for sender in ["bob", "carol", "dave", "erin", "mallory", "me"] {
                batch.push(say(sender, &format!("line {n}")));
            }
        }
        events(&chat, cx, batch);
        // A bottom-following list reports no rows; look at a scrolled-up
        // window of rows instead.
        chat.update(cx, |chat, cx| {
            let list = &chat.main_lists[&chat.state.selection()].state;
            list.scroll_to(gpui::ListOffset {
                item_ix: 4,
                offset_in_item: gpui::px(0.),
            });
            cx.notify();
        });
        cx.run_until_parked();
        let before = row_heights(&chat, cx);
        assert!(before.len() >= 3, "rows were laid out: {before:?}");
        set_avatars(&chat, cx, true);
        let calls = fetcher.calls();
        assert!(calls.contains(&BOB_URL.to_owned()), "{calls:?}");
        assert_eq!(calls.iter().filter(|url| *url == CAROL).count(), 1);
        assert!(!calls.iter().any(|url| url.contains("127.0.0.1")));
        let members: Vec<_> = calls
            .iter()
            .filter(|url| url.contains("/member/"))
            .collect();
        assert!(
            !members.is_empty() && members.len() < 100,
            "only visible members: {}",
            members.len()
        );
        chat.read_with(cx, |chat, _| {
            let cache = chat.avatars.cache();
            assert_eq!(cache.in_flight(), 0);
            assert!(cache.stats().ready >= 2);
            assert!(cache.stats().failed >= 1, "missing image");
            assert!(cache.ready_bytes() <= 2 * 1024 * 1024);
        });
        // Arrived images did not change any row's height: rows are exactly
        // as tall as without avatars.
        assert_eq!(row_heights(&chat, cx), before);
        // Redraws and repeated lines fetch nothing new.
        let count = fetcher.calls().len();
        events(&chat, cx, vec![say("bob", "again"), say("carol", "again")]);
        assert_eq!(fetcher.calls().len(), count);

        // Off: records and images go, and nothing is fetched afterwards.
        set_avatars(&chat, cx, false);
        chat.read_with(cx, |chat, _| {
            let cache = chat.avatars.cache();
            assert_eq!((cache.records(), cache.ready_bytes()), (0, 0));
            assert_eq!(chat.avatars.waiting_release(), 0, "both panes redrew");
        });
        events(
            &chat,
            cx,
            vec![
                avatar("frank", Some("https://x.example/f")),
                say("frank", "hi"),
            ],
        );
        assert_eq!(fetcher.calls().len(), count);
    }

    /// Looks `url` up at each size, lets the loads finish and returns the
    /// pixel width of each image that arrived.
    fn widths(
        chat: &Entity<ChatWindow>,
        cx: &mut VisualTestContext,
        url: &str,
        sizes: &[u32],
    ) -> Vec<Option<i32>> {
        chat.update(cx, |chat, cx| {
            for &pixels in sizes {
                chat.avatars.lookup(url, pixels);
            }
            chat.pump_avatars(cx);
        });
        cx.run_until_parked();
        chat.update(cx, |chat, _| {
            sizes
                .iter()
                .map(|&pixels| match chat.avatars.lookup(url, pixels) {
                    super::Shown::Image(image) => Some(image.size(0).width.0),
                    _ => None,
                })
                .collect()
        })
    }

    #[gpui::test]
    fn every_size_comes_from_one_download_and_is_never_enlarged(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        let url = "https://avatars.example.com/big/{size}";
        // The log's slot, and the WHOIS box at 1×, 1.5× and 2× scaling.
        let sizes = [32, 48, 72, 96];
        assert_eq!(
            sizes,
            [
                super::PIXELS,
                crate::whois::avatar_pixels(1.),
                crate::whois::avatar_pixels(1.5),
                crate::whois::avatar_pixels(2.),
            ]
        );
        let shown = widths(&chat, cx, url, &sizes);
        assert_eq!(shown, sizes.map(|pixels| Some(pixels as i32)));
        assert_eq!(
            fetcher.calls(),
            ["https://avatars.example.com/big/128".to_owned()],
            "one fetch, at the original size"
        );
        chat.read_with(cx, |chat, _| {
            let (entries, bytes) = chat.avatars.originals();
            assert_eq!(entries, 1);
            assert!(bytes > 0 && bytes <= cayenchat_media::ORIGINAL_MAX_BYTES);
            assert!(chat.avatars.cache().ready_bytes() <= 2 * 1024 * 1024);
        });
    }

    #[gpui::test]
    fn images_too_large_to_keep_are_fetched_again_for_each_size(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        let url = "https://avatars.example.com/huge";
        assert_eq!(widths(&chat, cx, url, &[32]), [Some(32)]);
        assert_eq!(widths(&chat, cx, url, &[96]), [Some(96)]);
        assert_eq!(fetcher.calls().len(), 2);
        chat.read_with(cx, |chat, _| assert_eq!(chat.avatars.originals().1, 0));
    }

    #[test]
    fn kept_originals_stay_within_their_count_and_byte_limits() {
        use cayenchat_media::{CancelFlag, Limits, load_avatar_thumbnail};

        let fetcher = FakeFetcher::default();
        let png_len = png(96, 64).len();
        let mut originals = super::Originals::new(5, 3 * png_len);
        let limits = Limits::avatar(32);
        let source = |n: usize| {
            cayenchat_media::policy::avatar_url(&format!("https://a.example/{n}"), 128).unwrap()
        };
        let load = |originals: &mut super::Originals, n: usize| {
            let source = source(n);
            let slot = originals.slot(&source);
            load_avatar_thumbnail(&source, &fetcher, &limits, &CancelFlag::default(), &slot)
                .unwrap();
            originals.settle(&source);
        };
        for n in 0..10 {
            load(&mut originals, n);
            assert!(originals.entries.len() <= 5 && originals.bytes <= 3 * png_len);
        }
        assert_eq!((originals.entries.len(), originals.bytes), (3, 3 * png_len));
        // The newest survive; an evicted one is fetched again, a kept one not.
        let calls = fetcher.calls().len();
        load(&mut originals, 9);
        assert_eq!(fetcher.calls().len(), calls);
        load(&mut originals, 0);
        assert_eq!(fetcher.calls().len(), calls + 1);
        originals.clear();
        assert_eq!((originals.entries.len(), originals.bytes), (0, 0));
    }

    #[gpui::test]
    fn a_new_occupant_of_a_nickname_does_not_get_the_old_avatar(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        events(
            &chat,
            cx,
            vec![avatar("bob", Some(BOB)), say("bob", "first bob")],
        );
        events(
            &chat,
            cx,
            vec![avatar("bob", None), say("bob", "someone else")],
        );
        chat.read_with(cx, |chat, _| {
            let messages = &chat.state.selected_channel().unwrap().messages;
            let shown = |index: usize| {
                chat.state
                    .avatars()
                    .for_message(NetworkId(1), "bob", messages[index].sequence)
                    .map(|url| url.to_string())
            };
            assert_eq!(shown(messages.len() - 2).as_deref(), Some(BOB));
            assert_eq!(shown(messages.len() - 1), None);
        });
        // A reconnect ends every occupancy; old rows keep theirs.
        events(
            &chat,
            cx,
            vec![Event::Registered {
                nickname: "me".into(),
            }],
        );
        chat.read_with(cx, |chat, _| {
            assert!(chat.state.avatars().current(NetworkId(1), "bob").is_none());
        });
    }

    #[gpui::test]
    fn a_ctcp_avatar_answer_also_shows_on_the_line_that_caused_the_lookup(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        let peer = |url: Option<&str>| Event::PeerAvatar {
            nickname: "Bob".into(),
            url: url.map(Into::into),
        };
        // bob's first line comes before his answer.
        events(&chat, cx, vec![say("bob", "first line")]);
        chat.read_with(cx, |chat, _| {
            assert!(chat.state.avatars().is_empty());
        });
        events(&chat, cx, vec![peer(Some(BOB)), say("bob", "second line")]);
        // Leaving and a reconnect do not take it away, and metadata that
        // ends later does not either.
        events(
            &chat,
            cx,
            vec![Event::Registered {
                nickname: "me".into(),
            }],
        );
        chat.read_with(cx, |chat, _| {
            let messages = &chat.state.selected_channel().unwrap().messages;
            for message in messages.iter().filter(|m| m.sender == "bob") {
                let shown = chat
                    .state
                    .avatars()
                    .for_message(NetworkId(1), "bob", message.sequence)
                    .map(|url| url.to_string());
                assert_eq!(shown.as_deref(), Some(BOB), "{}", message.text);
            }
            assert!(chat.state.avatars().current(NetworkId(1), "bob").is_some());
        });
        // Someone else with a mask without a picture: the name loses it.
        events(&chat, cx, vec![peer(None)]);
        chat.read_with(cx, |chat, _| {
            assert!(chat.state.avatars().current(NetworkId(1), "bob").is_none());
        });
    }

    #[gpui::test]
    fn avatars_and_previews_share_the_fetch_limit(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, true, &fetcher);
        chat.update(cx, |chat, cx| {
            for n in 0..10 {
                chat.avatars
                    .lookup(&format!("https://avatars.example.com/n/{n}"), super::PIXELS);
                let url = format!("https://images.example.com/{n}.png");
                chat.previews.lookup(
                    &cayenchat_app::links::find(&url),
                    crate::previews::RowRef {
                        selection: chat.state.selection(),
                        sequence: n,
                    },
                );
            }
            chat.pump_previews(cx);
            chat.pump_avatars(cx);
            assert_eq!(chat.media_loads(), super::MAX_MEDIA_LOADS);
            assert_eq!(chat.previews.in_flight(), 2);
            assert_eq!(chat.avatars.in_flight(), 1);
        });
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.media_loads(), 0);
            assert_eq!(chat.avatars.cache().stats().ready, 10);
            assert_eq!(chat.previews.cache().stats().ready, 10);
        });
    }

    #[gpui::test]
    fn typing_reuses_panes_while_avatars_are_shown(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        events(
            &chat,
            cx,
            vec![avatar("bob", Some(BOB)), say("bob", "hello")],
        );
        let input = chat.read_with(cx, |chat, _| {
            assert_eq!(chat.avatars.cache().stats().ready, 1);
            chat.inputs[&chat.state.selection()].clone()
        });
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.run_until_parked();
        let rendered = chat.read_with(cx, |chat, _| chat.pane_renders);
        cx.simulate_input("typing");
        cx.run_until_parked();
        assert_eq!(chat.read_with(cx, |chat, _| chat.pane_renders), rendered);
    }

    #[gpui::test]
    fn metadata_about_joiners_and_ourselves_is_not_chat_and_loads_nothing_while_off(
        cx: &mut TestAppContext,
    ) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, false, false, &fetcher);
        let counts = |chat: &Entity<ChatWindow>, cx: &mut VisualTestContext| {
            chat.read_with(cx, |chat, _| {
                (
                    chat.state.server_messages(NetworkId(1)).len(),
                    chat.state.conversations()[0].messages.len(),
                )
            })
        };
        let before = counts(&chat, cx);
        // A later joiner's looked-up avatar and our own confirmed one.
        events(
            &chat,
            cx,
            vec![
                Event::MetadataReady,
                Event::OwnAvatar {
                    url: Some(BOB.into()),
                    request: None,
                },
                avatar("me", Some(BOB)),
                avatar("dave", Some(CAROL)),
                Event::OwnAvatarFailed {
                    request: 99,
                    failure: cayenchat_irc_core::AvatarRequestFailure::NoReply,
                },
            ],
        );
        chat.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(counts(&chat, cx), before, "no chat rows or server lines");
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.avatars.lookups.get(), 0);
            assert_eq!(chat.avatars.cache().records(), 0);
            assert!(chat.state.avatars().current(NetworkId(1), "dave").is_some());
            // A stale failure for a request never made changes nothing.
            assert_eq!(chat.sessions[&NetworkId(1)].own_avatar.outcome(), None);
        });
        assert!(fetcher.calls().is_empty(), "nothing downloaded while off");
    }

    #[gpui::test]
    fn our_own_messages_use_the_saved_icon_without_a_lookup(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.own_avatar_for(NetworkId(1), "me"), Some(None));
            assert_eq!(chat.own_avatar_for(NetworkId(1), "bob"), None);
        });
        // What the server confirms is not what we show (#612).
        events(
            &chat,
            cx,
            vec![
                Event::MetadataReady,
                Event::OwnAvatar {
                    url: Some(CAROL.into()),
                    request: None,
                },
                say("me", "hello"),
            ],
        );
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.own_avatar_for(NetworkId(1), "me"), Some(None));
        });
        // Saving the settings shows the icon at once, with nothing sent.
        chat.update(cx, |chat, cx| {
            let mut settings = chat.saved.clone();
            settings.servers[0].avatar_url = BOB.into();
            chat.apply_servers(settings, cx);
        });
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            let bob = Some(Some(Arc::<str>::from(BOB)));
            assert_eq!(chat.own_avatar_for(NetworkId(1), "me"), bob);
            assert_eq!(chat.own_avatar_for(NetworkId(1), "ME"), bob);
            assert_eq!(chat.own_avatar_for(NetworkId(1), "bob"), None);
            assert!(chat.state.avatars().current(NetworkId(1), "me").is_none());
        });
        assert!(
            fetcher.calls().iter().any(|url| url.contains("bob")),
            "{:?}",
            fetcher.calls()
        );
        // Removing it shows the placeholder at once.
        chat.update(cx, |chat, cx| {
            let mut settings = chat.saved.clone();
            settings.servers[0].avatar_url.clear();
            chat.apply_servers(settings, cx);
        });
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.own_avatar_for(NetworkId(1), "me"), Some(None));
        });
    }

    #[gpui::test]
    fn users_without_an_avatar_get_the_default_one_only_while_shown(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, false, false, &fetcher);
        let mut batch = roster(20);
        batch.extend([
            say("bob", "no avatar"),
            say("carol", "no avatar either"),
            avatar("dave", Some("https://avatars.example.com/missing")),
            say("dave", "failed image"),
        ]);
        events(&chat, cx, batch);
        chat.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.avatars.defaults_made(), 0, "off: none")
        });

        set_avatars(&chat, cx, true);
        chat.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let made = chat.read_with(cx, |chat, _| chat.avatars.defaults_made());
        // Bob, carol, "me" and dave (failed) at least; one per look.
        assert!((1..=512).contains(&made), "{made}");
        chat.read_with(cx, |chat, _| {
            let bob = chat.avatars.default_for("bob");
            assert!(
                Arc::ptr_eq(&bob, &chat.avatars.default_for("bob")),
                "shared"
            );
        });
        chat.read_with(cx, |chat, _| {
            let large = chat.avatars.default_sized("bob", 96);
            let svg = String::from_utf8(large.bytes().to_vec()).unwrap();
            assert!(svg.contains(r#"width="96" height="96""#), "own size");
            assert!(Arc::ptr_eq(&large, &chat.avatars.default_sized("bob", 96)));
        });
        // No image is fetched for them.
        assert!(fetcher.calls().iter().all(|url| !url.contains("bob")));

        set_avatars(&chat, cx, false);
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.avatars.defaults_made(), 0, "released")
        });
    }

    #[gpui::test]
    fn removing_a_server_forgets_its_avatars(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        events(&chat, cx, vec![avatar("bob", Some(BOB))]);
        chat.update(cx, |chat, cx| {
            let mut settings = chat.saved.clone();
            settings.remove_selected_server();
            chat.apply_servers(settings, cx);
            assert!(chat.state.avatars().is_empty());
        });
    }
}
