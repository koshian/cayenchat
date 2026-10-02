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
    Fetcher, HttpFetcher, Limits, LoadError, Thumbnail,
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
    cache: RefCell<PreviewCache<Arc<RenderImage>, ()>>,
    limits: Limits,
    /// Present only while avatars are shown.
    fetcher: Option<Arc<dyn Fetcher>>,
    released: RefCell<Vec<Released>>,
    /// Default avatars by look; bounded by the 512 combinations.
    defaults: RefCell<HashMap<default_avatar::Params, Arc<gpui::Image>>>,
    #[cfg(test)]
    fetcher_override: Option<Arc<dyn Fetcher>>,
    #[cfg(test)]
    pub lookups: std::cell::Cell<usize>,
}

impl Avatars {
    pub fn new(enabled: bool) -> Self {
        let mut avatars = Self {
            cache: RefCell::new(PreviewCache::new(cache_limits())),
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
        let params = default_avatar::params(nickname);
        self.defaults
            .borrow_mut()
            .entry(params)
            .or_insert_with(|| {
                Arc::new(gpui::Image::from_bytes(
                    gpui::ImageFormat::Svg,
                    default_avatar::svg(params, PIXELS).into_bytes(),
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

    /// The image for an avatar reference (an avatar URL template), queued
    /// for loading if needed. Call only for rows being drawn.
    pub fn lookup(&self, avatar: &str) -> Shown {
        if !self.enabled() {
            return Shown::None;
        }
        #[cfg(test)]
        self.lookups.set(self.lookups.get() + 1);
        let Some(source) = policy::avatar_url(avatar, PIXELS) else {
            return Shown::None;
        };
        match self.cache.borrow_mut().request(&source, (), Instant::now()) {
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
    pub fn cache(&self) -> std::cell::Ref<'_, PreviewCache<Arc<RenderImage>, ()>> {
        self.cache.borrow()
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
            let (source, cancel, fetcher) =
                (job.source.clone(), job.cancel.clone(), fetcher.clone());
            let work = cx.background_executor().spawn(async move {
                cayenchat_media::load_thumbnail(&source, fetcher.as_ref(), &limits, &cancel)
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
        job: Job,
        result: Result<(Arc<RenderImage>, usize), LoadError>,
        cx: &mut Context<Self>,
    ) {
        let finished = self
            .avatars
            .cache
            .get_mut()
            .finish(job, result, Instant::now());
        if finished.applied {
            // Both avatar panes are cached; the slot size does not change.
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

    use crate::ChatWindow;

    const BOB: &str = "https://avatars.example.com/u/bob?s={size}";
    const BOB_URL: &str = "https://avatars.example.com/u/bob?s=32";
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
            Ok(png(96, 64))
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
        settings.appearance.user_avatars = avatars;
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
            chat.handle_events(NetworkId(1), batch, false, cx);
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
            mentioned: false,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        }
    }

    fn set_avatars(chat: &Entity<ChatWindow>, cx: &mut VisualTestContext, on: bool) {
        chat.update(cx, |chat, cx| {
            let mut appearance = chat.appearance.clone();
            appearance.user_avatars = on;
            let mode = chat.theme_mode;
            chat.apply_appearance(appearance, mode, cx);
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
    fn avatars_and_previews_share_the_fetch_limit(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, true, &fetcher);
        chat.update(cx, |chat, cx| {
            for n in 0..10 {
                chat.avatars
                    .lookup(&format!("https://avatars.example.com/n/{n}"));
                let url = format!("https://images.example.com/{n}.png");
                chat.previews.lookup(
                    &crate::log_urls(&url),
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
    fn our_own_messages_use_our_own_avatar_without_a_lookup(cx: &mut TestAppContext) {
        let fetcher = Arc::new(FakeFetcher::default());
        let (chat, cx) = open(cx, true, false, &fetcher);
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.own_avatar_for(NetworkId(1), "me"), None, "none set");
        });
        events(
            &chat,
            cx,
            vec![
                Event::MetadataReady,
                Event::OwnAvatar {
                    url: Some(BOB.into()),
                    request: None,
                },
                say("me", "hello"),
            ],
        );
        chat.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        chat.read_with(cx, |chat, _| {
            assert_eq!(
                chat.own_avatar_for(NetworkId(1), "me").as_deref(),
                Some(BOB)
            );
            assert_eq!(
                chat.own_avatar_for(NetworkId(1), "ME").as_deref(),
                Some(BOB)
            );
            assert_eq!(chat.own_avatar_for(NetworkId(1), "bob"), None);
            assert!(chat.state.avatars().current(NetworkId(1), "me").is_none());
        });
        assert!(
            fetcher.calls().iter().any(|url| url.contains("bob")),
            "{:?}",
            fetcher.calls()
        );
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
