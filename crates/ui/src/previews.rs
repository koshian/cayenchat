//! Inline image previews in the main channel log.
//!
//! `cayenchat-media` decides which links are previewed, loads and decodes
//! them and keeps the bounded cache; this module draws the thumbnails and
//! runs the loads on GPUI's background executor. Rows ask for their preview
//! while they are drawn, so only rows on screen (and the log list's overdraw)
//! cause requests. When previews are off nothing is looked up, loaded or
//! decoded, and the fetcher with its idle connections is dropped.

use std::{cell::RefCell, ops::Range, sync::Arc, time::Instant};

use cayenchat_app::Selection;
use cayenchat_media::{
    Fetcher, HttpFetcher, Limits, LoadError, Thumbnail,
    cache::{CacheLimits, Job, Lookup, PreviewCache},
    decode, policy,
};
use gpui::{Context, RenderImage, Window};

use crate::ChatWindow;

/// Largest preview in logical pixels. Thumbnails are made at up to twice
/// this size in pixels (`Limits::thumbnail_*`) so they stay sharp on 2×
/// displays. A pending preview reserves the full height.
pub const BOX_WIDTH: u32 = 200;
pub const BOX_HEIGHT: u32 = 100;

/// A log row that showed a pending preview: its log and message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRef {
    pub selection: Selection,
    pub sequence: u64,
}

/// A decoded preview and its size on screen in logical pixels.
#[derive(Clone)]
pub struct Preview {
    pub image: Arc<RenderImage>,
    pub width: f32,
    pub height: f32,
}

/// What a message row shows below its text.
pub enum Shown {
    Image(Preview),
    /// Loading; the box keeps its place so the row does not jump.
    Pending,
}

pub struct Previews {
    cache: RefCell<PreviewCache<Preview, RowRef>>,
    limits: Limits,
    /// Present only while previews are on.
    fetcher: Option<Arc<dyn Fetcher>>,
    /// Replaces the HTTP fetcher; tests use a local fake.
    #[cfg(test)]
    fetcher_override: Option<Arc<dyn Fetcher>>,
}

impl Previews {
    pub fn new(enabled: bool) -> Self {
        let mut previews = Self {
            cache: RefCell::new(PreviewCache::new(CacheLimits::default())),
            limits: Limits {
                thumbnail_width: BOX_WIDTH * 2,
                thumbnail_height: BOX_HEIGHT * 2,
                ..Limits::default()
            },
            fetcher: None,
            #[cfg(test)]
            fetcher_override: None,
        };
        previews.set_enabled(enabled);
        previews
    }

    pub fn enabled(&self) -> bool {
        self.fetcher.is_some()
    }

    /// Turning previews off cancels loads, forgets every record and queues
    /// the decoded images for release on the next draw.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.cache.get_mut().set_enabled(enabled);
        if !enabled {
            self.fetcher = None;
        } else if self.fetcher.is_none() {
            #[cfg(test)]
            if let Some(fetcher) = &self.fetcher_override {
                self.fetcher = Some(fetcher.clone());
                return;
            }
            #[cfg(feature = "preview-fixture")]
            if let Some(fetcher) = fixture::DirectoryFetcher::from_env() {
                self.fetcher = Some(Arc::new(fetcher));
                return;
            }
            self.fetcher = Some(Arc::new(HttpFetcher::new(&self.limits)));
        }
    }

    /// The preview for the first previewable link among `urls` (as found by
    /// `log_urls`), with that link. Requests it if needed.
    pub fn lookup(&self, urls: &[(Range<usize>, String)], row: RowRef) -> Option<(String, Shown)> {
        if !self.enabled() {
            return None;
        }
        let (link, source) = urls
            .iter()
            .find_map(|(_, link)| Some((link, policy::image_link(link)?)))?;
        let shown = match self
            .cache
            .borrow_mut()
            .request(&source, row, Instant::now())
        {
            Lookup::Ready(preview) => Shown::Image(preview.clone()),
            Lookup::Pending => Shown::Pending,
            Lookup::None => return None,
        };
        Some((link.clone(), shown))
    }

    /// Frees the GPU textures of images that left the cache. Only images the
    /// cache no longer holds are released, so nothing drawn later uses them.
    /// Call it while the main log pane redraws, never while that pane may be
    /// reused from its cache.
    pub fn in_flight(&self) -> usize {
        self.cache.borrow().in_flight()
    }

    pub fn release(&self, window: &mut Window) {
        for preview in self.cache.borrow_mut().take_released() {
            let _ = window.drop_image(preview.image);
        }
    }

    /// Forgets rows of conversations that no longer exist.
    pub fn retain_rows(&mut self, keep: impl FnMut(&RowRef) -> bool) {
        self.cache.get_mut().retain_waiters(keep);
    }

    #[cfg(test)]
    pub fn use_fetcher(&mut self, fetcher: Arc<dyn Fetcher>) {
        if self.fetcher.is_some() {
            self.fetcher = Some(fetcher.clone());
        }
        self.fetcher_override = Some(fetcher);
    }

    #[cfg(test)]
    pub fn cache(&self) -> std::cell::Ref<'_, PreviewCache<Preview, RowRef>> {
        self.cache.borrow()
    }
}

/// Wraps a thumbnail for GPUI. Charges its bytes twice: the pixels stay in
/// the `RenderImage` and are copied into the GPU sprite atlas when drawn.
fn to_preview(thumbnail: Thumbnail) -> (Preview, usize) {
    let (width, height) = decode::fit(
        thumbnail.source_width,
        thumbnail.source_height,
        BOX_WIDTH,
        BOX_HEIGHT,
    );
    let bytes = thumbnail.byte_len() * 2;
    let buffer = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.bgra)
        .expect("thumbnail buffer matches its size");
    let image = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
    (
        Preview {
            image,
            width: width as f32,
            height: height as f32,
        },
        bytes,
    )
}

/// Local images for measurements and GUI checks (`preview-fixture`
/// feature): `https://images.cayenchat.test/<name>` is read from
/// `$CAYENCHAT_PREVIEW_FIXTURE_DIR/<name>`, after an optional
/// `$CAYENCHAT_PREVIEW_FIXTURE_DELAY_MS` standing in for the network. Every
/// other link fails without any request. Candidate recognition and decoding
/// are the production code.
#[cfg(feature = "preview-fixture")]
pub(crate) mod fixture {
    use std::{path::PathBuf, time::Duration};

    use cayenchat_media::{CancelFlag, Fetcher, Limits, LoadError, MediaRef};

    pub struct DirectoryFetcher {
        directory: PathBuf,
        delay: Duration,
    }

    impl DirectoryFetcher {
        pub fn from_env() -> Option<Self> {
            let directory = PathBuf::from(std::env::var_os("CAYENCHAT_PREVIEW_FIXTURE_DIR")?);
            let delay = std::env::var("CAYENCHAT_PREVIEW_FIXTURE_DELAY_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_millis)
                .unwrap_or_default();
            log::warn!(
                "image previews read local fixtures from {}",
                directory.display()
            );
            Some(Self { directory, delay })
        }
    }

    impl Fetcher for DirectoryFetcher {
        fn fetch(
            &self,
            source: &MediaRef,
            limits: &Limits,
            cancel: &CancelFlag,
        ) -> Result<Vec<u8>, LoadError> {
            let url = match source {
                MediaRef::Link(url) if url.host_str() == Some("images.cayenchat.test") => url,
                _ => return Err(LoadError::Blocked),
            };
            let name = url
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty() && !name.starts_with('.'))
                .ok_or(LoadError::Status(404))?;
            std::thread::sleep(self.delay);
            if cancel.is_cancelled() {
                return Err(LoadError::Cancelled);
            }
            let path = self.directory.join(name);
            let length = std::fs::metadata(&path)
                .map_err(|_| LoadError::Status(404))?
                .len();
            if length > limits.max_response_bytes as u64 {
                return Err(LoadError::TooLarge);
            }
            std::fs::read(path).map_err(|_| LoadError::Status(404))
        }
    }
}

impl ChatWindow {
    /// Starts queued loads while slots are free.
    pub(crate) fn pump_previews(&self, cx: &mut Context<Self>) {
        let Some(fetcher) = self.previews.fetcher.clone() else {
            return;
        };
        let limits = self.previews.limits;
        // Fetches in flight are shared with avatars.
        while self.media_loads() < crate::avatars::MAX_MEDIA_LOADS {
            let Some(job) = self.previews.cache.borrow_mut().next_job() else {
                break;
            };
            let (source, cancel, fetcher) =
                (job.source.clone(), job.cancel.clone(), fetcher.clone());
            let work = cx.background_executor().spawn(async move {
                cayenchat_media::load_thumbnail(&source, fetcher.as_ref(), &limits, &cancel)
                    .map(to_preview)
            });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                this.update(cx, |chat, cx| chat.preview_finished(job, result, cx))
                    .ok();
            })
            .detach();
        }
    }

    fn preview_finished(
        &mut self,
        job: Job,
        result: Result<(Preview, usize), LoadError>,
        cx: &mut Context<Self>,
    ) {
        let height = result.as_ref().ok().map(|(preview, _)| preview.height);
        let finished = self
            .previews
            .cache
            .get_mut()
            .finish(job, result, Instant::now());
        if finished.applied {
            // Rows that showed the full-height placeholder change height.
            // Rows off screen keep a measured height in their list; replace
            // it so scrolling back does not jump.
            if height != Some(BOX_HEIGHT as f32) {
                for row in finished.waiters {
                    if let Some(list) = self.main_lists.get_mut(&row.selection) {
                        list.invalidate(row.sequence);
                    }
                }
            }
            // The main log is a cached pane.
            cx.notify();
        }
        self.pump_previews(cx);
        self.pump_avatars(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::Cursor,
        sync::{Arc, Mutex},
    };

    use cayenchat_app::Selection;
    use cayenchat_irc_core::Event;
    use cayenchat_media::{CancelFlag, Fetcher, Limits, LoadError, MediaRef};
    use cayenchat_model::NetworkId;
    use gpui::{Entity, Focusable, TestAppContext, VisualTestContext};

    use crate::ChatWindow;

    const A: &str = "https://images.example.com/a.png";
    const B: &str = "https://images.example.com/b.png";
    const WIDE: &str = "https://images.example.com/wide.png";
    const PAGE: &str = "https://images.example.com/page.png";

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([0, 128, 255, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    /// Serves fixed bodies and records every request; never touches the
    /// network.
    #[derive(Default)]
    struct FakeFetcher {
        bodies: HashMap<String, Vec<u8>>,
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
            self.bodies.get(&url).cloned().ok_or(LoadError::Status(404))
        }
    }

    fn fetcher() -> Arc<FakeFetcher> {
        Arc::new(FakeFetcher {
            bodies: HashMap::from([
                (A.to_string(), png(800, 400)),
                (B.to_string(), png(40, 30)),
                (WIDE.to_string(), png(1000, 100)),
                (
                    PAGE.to_string(),
                    b"<!doctype html><title>no</title>".to_vec(),
                ),
            ]),
            calls: Mutex::default(),
        })
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        channels: &str,
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
        let mut settings = crate::settings_with_channels(channels);
        settings.appearance.image_previews = previews;
        let fetcher: Arc<dyn Fetcher> = fetcher.clone();
        let (chat, cx) = cx.add_window_view(|window, cx| {
            let mut chat = ChatWindow::with_settings(settings, None, window, cx);
            chat.previews.use_fetcher(fetcher);
            chat
        });
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![Event::Registered {
                    nickname: "me".into(),
                }],
                false,
                cx,
            );
            let channel = chat.state.conversations()[0].id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        (chat, cx)
    }

    fn say(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, sender: &str, text: &str) {
        chat.update(cx, |chat, cx| {
            let name = chat.state.selected_channel().unwrap().name.clone();
            chat.handle_events(
                NetworkId(1),
                vec![Event::ChannelMessage {
                    channel: name,
                    sender: sender.into(),
                    text: text.into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                }],
                false,
                cx,
            );
        });
        cx.run_until_parked();
    }

    fn set_previews(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, on: bool) {
        chat.update(cx, |chat, cx| {
            let mut appearance = chat.appearance.clone();
            appearance.image_previews = on;
            let mode = chat.theme_mode;
            chat.apply_appearance(appearance, mode, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn previews_load_once_per_link_only_while_enabled(cx: &mut TestAppContext) {
        let fetcher = fetcher();
        let (chat, cx) = open(cx, "#a", false, &fetcher);
        for n in 0..20 {
            say(&chat, cx, "bob", &format!("{n} {A} and {B}"));
        }
        say(&chat, cx, "bob", &format!("* {PAGE}"));
        assert!(fetcher.calls().is_empty(), "nothing is fetched while off");
        chat.read_with(cx, |chat, _| {
            assert!(!chat.previews.enabled());
            assert_eq!(chat.previews.cache().records(), 0);
        });

        // Turned on live: visible rows ask once per link. Only the first
        // image link of a message is previewed.
        set_previews(&chat, cx, true);
        let mut calls = fetcher.calls();
        calls.sort();
        assert_eq!(calls, [A, PAGE]);
        chat.read_with(cx, |chat, _| {
            let cache = chat.previews.cache();
            assert_eq!(cache.stats().ready, 1);
            assert_eq!(cache.stats().failed, 1, "a page is not an image");
            assert_eq!(cache.ready_bytes(), 400 * 200 * 4 * 2);
            assert_eq!(cache.in_flight(), 0);
        });

        // A local echo of our own link is previewed like any message.
        say(&chat, cx, "me", &format!("mine: {B}"));
        assert_eq!(fetcher.calls().iter().filter(|url| *url == B).count(), 1);

        // Redraws, repeated links and failures cause no further requests.
        for _ in 0..3 {
            say(&chat, cx, "bob", &format!("again {A} {PAGE}"));
        }
        say(&chat, cx, "bob", PAGE);
        assert_eq!(fetcher.calls().len(), 3);

        // Turned off: records and images are dropped and nothing is fetched.
        set_previews(&chat, cx, false);
        chat.read_with(cx, |chat, _| {
            let cache = chat.previews.cache();
            assert_eq!(
                (cache.records(), cache.ready_bytes(), cache.queued()),
                (0, 0, 0)
            );
        });
        chat.update(cx, |chat, _| {
            assert!(
                chat.previews.cache.borrow_mut().take_released().is_empty(),
                "released images were dropped from the GPU atlas on the next draw"
            );
        });
        say(&chat, cx, "bob", WIDE);
        assert_eq!(fetcher.calls().len(), 3);
    }

    #[gpui::test]
    fn typing_reuses_panes_while_previews_are_shown(cx: &mut TestAppContext) {
        let fetcher = fetcher();
        let (chat, cx) = open(cx, "#a", true, &fetcher);
        say(&chat, cx, "bob", A);
        let input = chat.read_with(cx, |chat, _| {
            assert_eq!(chat.previews.cache().stats().ready, 1);
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
    fn disabling_during_a_load_ignores_its_result(cx: &mut TestAppContext) {
        let fetcher = fetcher();
        let (chat, cx) = open(cx, "#a", false, &fetcher);
        say(&chat, cx, "bob", A);
        chat.update(cx, |chat, cx| {
            chat.previews.set_enabled(true);
            let row = super::RowRef {
                selection: chat.state.selection(),
                sequence: 1,
            };
            let urls = crate::log_urls(A);
            assert!(matches!(
                chat.previews.lookup(&urls, row),
                Some((_, super::Shown::Pending))
            ));
            chat.pump_previews(cx);
            assert_eq!(chat.previews.cache().in_flight(), 1);
            // Off and on again before the load returns.
            chat.previews.set_enabled(false);
            chat.previews.set_enabled(true);
        });
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            let cache = chat.previews.cache();
            assert_eq!(cache.stats().stale, 1);
            assert_eq!(cache.in_flight(), 0);
        });
        // The row is visible again after the next draw and loads anew.
        chat.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.previews.cache().stats().ready, 1);
        });
    }

    #[gpui::test]
    fn removed_conversations_and_other_servers_do_not_receive_previews(cx: &mut TestAppContext) {
        let fetcher = fetcher();
        let (chat, cx) = open(cx, "#a,#b", true, &fetcher);
        say(&chat, cx, "bob", WIDE);
        let (a, b) = chat.read_with(cx, |chat, _| {
            let conversations = chat.state.conversations();
            (conversations[0].id, conversations[1].id)
        });
        // A message in an unselected channel is not drawn, so not fetched.
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                NetworkId(1),
                vec![Event::ChannelMessage {
                    channel: "#b".into(),
                    sender: "bob".into(),
                    text: B.into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                }],
                false,
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), [WIDE]);

        // A wide image is shorter than the placeholder: its row was
        // remeasured without resetting the list.
        chat.read_with(cx, |chat, _| {
            let list = &chat.main_lists[&Selection::Channel(a)];
            assert_eq!(list.state.item_count(), 1);
        });

        // Rows of removed conversations are forgotten, even while queued.
        chat.update(cx, |chat, _| {
            let row = super::RowRef {
                selection: Selection::Channel(b),
                sequence: 99,
            };
            chat.previews.lookup(&crate::log_urls(B), row);
            assert_eq!(chat.previews.cache().queued(), 1);
            chat.forget_conversations(&[b]);
            assert_eq!(chat.previews.cache().queued(), 0);
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), [WIDE]);
    }

    #[gpui::test]
    fn a_removed_server_loses_its_rows_while_another_keeps_its_preview(cx: &mut TestAppContext) {
        let fetcher = fetcher();
        let (chat, cx) = open(cx, "#a", true, &fetcher);
        let (first, settings) = chat.update(cx, |chat, cx| {
            let mut settings = chat.saved.clone();
            settings.add_server("irc.example.org").channels = "#a".into();
            chat.apply_servers(settings.clone(), cx);
            (chat.state.selection(), settings)
        });
        let second = NetworkId(2);
        // The second server's channel is selected and drawn: its load starts.
        chat.update(cx, |chat, cx| {
            chat.handle_events(
                second,
                vec![Event::Registered {
                    nickname: "me".into(),
                }],
                false,
                cx,
            );
            chat.handle_events(
                second,
                vec![Event::ChannelMessage {
                    channel: "#a".into(),
                    sender: "carol".into(),
                    text: A.into(),
                    notice: false,
                    server_time: None,
                    msgid: None,
                    account: None,
                    replayed: false,
                }],
                false,
                cx,
            );
            let channel = chat
                .state
                .conversations()
                .iter()
                .find(|c| c.network == second)
                .unwrap()
                .id;
            chat.state
                .dispatch(cayenchat_app::Command::SelectChannel(channel));
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), [A]);

        // Remove the second server; the first shows the same link from the
        // shared cache without fetching it again.
        chat.update(cx, |chat, cx| {
            let mut next = settings.clone();
            next.selected_server = next.servers[1].id.clone();
            next.remove_selected_server();
            chat.apply_servers(next, cx);
            assert!(!chat.main_lists.keys().any(|s| matches!(s, Selection::Channel(id) if chat.state.conversations().iter().all(|c| c.id != *id))));
            if let Selection::Channel(id) = first {
                chat.state.dispatch(cayenchat_app::Command::SelectChannel(id));
            }
            cx.notify();
        });
        say(&chat, cx, "bob", A);
        assert_eq!(fetcher.calls(), [A]);
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.state.networks().len(), 1);
            assert_eq!(chat.previews.cache().stats().ready, 1);
        });
    }
}
