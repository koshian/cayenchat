//! Headless timing baseline for typing, channel switching and event bursts.
//!
//! Ignored by default. Run it in release mode:
//!
//! ```sh
//! cargo test --release --locked -p cayenchat-ui perf_baseline -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `perf_baseline` uses one server; `perf_baseline_4_servers` connects four
//! servers with the same channels each, so the difference shows the cost
//! of more networks (channel switches then also cross servers).
//! `perf_baseline_previews` turns image previews on: every 20th line carries
//! one of 120 image links (repeated links, and more than the cache budget
//! holds), served by an in-memory fetcher with an 800×400 PNG. Loads run
//! between samples (`run_until_parked`), outside the timed sections; the
//! test dispatcher runs background work on the test thread, so only the
//! UI-side cost is timed. `perf_baseline_avatars` turns user avatars on
//! instead: every member has an avatar (one URL each, served by the same
//! fetcher and cropped to a 32×32 square), so message and member rows draw
//! avatar slots. All
//! variants also time scrolling the main log.
//!
//! GPUI's test platform draws a dirty window synchronously at the end of each
//! update, so each timing covers the app's state update plus element
//! construction, layout, prepaint and paint into a scene. The test platform
//! uses a no-op text system and never presents to a GPU, so text shaping,
//! rasterization and display latency are not included. Treat the numbers as
//! the app-side CPU cost per interaction, not as time until pixels change.

use std::time::{Duration, Instant};

use cayenchat_app::Command;
use cayenchat_irc_core::{Event, WireDirection};
use cayenchat_model::NetworkId;
use gpui::{Focusable, TestAppContext};

use crate::{ChatWindow, Selection};

const CHANNELS: usize = 10;
const MEMBERS: usize = 50;
/// Lines per channel: exactly the 2,000-line cap, the largest a log gets
/// (one more line trims it to 1,000). The wire diagnostics saturate too.
const HISTORY: usize = 2_000;
const EVENT_BATCH: usize = 256;
const TYPING_SAMPLES: usize = 300;
const SWITCH_SAMPLES: usize = 200;
const BURST_SAMPLES: usize = 100;
const SCROLL_SAMPLES: usize = 200;
/// With previews on, one line in this many carries an image link.
const IMAGE_EVERY: usize = 20;
const IMAGE_LINKS: usize = 120;

const TEXTS: [&str; 4] = [
    "a somewhat longer line of ordinary IRC chatter about the build and the release plan",
    "日本語のメッセージです。チャンネル切替と入力の応答性を確認するための行です。",
    "mixed 日本語 and English text with a URL https://example.invalid/path?q=1",
    "short line",
];

fn channel(index: usize) -> String {
    format!("#perf{index:02}")
}

fn image_link(index: usize) -> String {
    format!("https://images.load.example/{index:02}.png")
}

/// One incoming line as the worker reports it: its wire diagnostic, then the
/// translated message.
fn incoming(sequence: usize, images: bool) -> [Event; 2] {
    let channel = channel(sequence % CHANNELS);
    let sender = format!("user{:03}", sequence % MEMBERS);
    // Lines go round-robin over the channels; count within the channel.
    let line = sequence / CHANNELS;
    let text = if images && line.is_multiple_of(IMAGE_EVERY) {
        let link = (line / IMAGE_EVERY + sequence % CHANNELS * 7) % IMAGE_LINKS;
        format!("see {}", image_link(link))
    } else {
        TEXTS[sequence % TEXTS.len()].to_owned()
    };
    [
        Event::Wire {
            elapsed: Duration::from_millis(sequence as u64),
            direction: WireDirection::Received,
            line: format!(":{sender}!{sender}@load.invalid PRIVMSG {channel} :{text}"),
        },
        Event::ChannelMessage {
            channel,
            sender,
            text,
            notice: false,
            mentioned: false,
            server_time: None,
            msgid: None,
            account: None,
            replayed: false,
        },
    ]
}

struct Timings {
    servers: usize,
    name: &'static str,
    samples: Vec<Duration>,
}

impl Timings {
    fn new(servers: usize, name: &'static str) -> Self {
        Self {
            servers,
            name,
            samples: Vec::new(),
        }
    }

    fn time<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let started = Instant::now();
        let result = f();
        self.samples.push(started.elapsed());
        result
    }

    fn report(&mut self) {
        self.samples.sort();
        let micros = |d: Duration| d.as_secs_f64() * 1e6;
        let at = |q: f64| micros(self.samples[((self.samples.len() - 1) as f64 * q) as usize]);
        println!(
            "perf_baseline servers={} {:<16} n={:<4} median={:>9.1}us p95={:>9.1}us max={:>9.1}us",
            self.servers,
            self.name,
            self.samples.len(),
            at(0.5),
            at(0.95),
            micros(*self.samples.last().unwrap()),
        );
    }
}

#[gpui::test]
#[ignore = "performance baseline; run with --release -- --ignored --nocapture"]
fn perf_baseline(cx: &mut TestAppContext) {
    run(cx, 1, false, false);
}

#[gpui::test]
#[ignore = "performance baseline; run with --release -- --ignored --nocapture"]
fn perf_baseline_4_servers(cx: &mut TestAppContext) {
    run(cx, 4, false, false);
}

#[gpui::test]
#[ignore = "performance baseline; run with --release -- --ignored --nocapture"]
fn perf_baseline_previews(cx: &mut TestAppContext) {
    run(cx, 1, true, false);
}

#[gpui::test]
#[ignore = "performance baseline; run with --release -- --ignored --nocapture"]
fn perf_baseline_avatars(cx: &mut TestAppContext) {
    run(cx, 1, false, true);
}

/// Serves one 800×400 PNG for every link and counts requests.
struct MemoryFetcher {
    png: Vec<u8>,
    requests: std::sync::atomic::AtomicUsize,
}

impl cayenchat_media::Fetcher for MemoryFetcher {
    fn fetch(
        &self,
        _: &cayenchat_media::MediaRef,
        _: &cayenchat_media::Limits,
        _: &cayenchat_media::CancelFlag,
    ) -> Result<Vec<u8>, cayenchat_media::LoadError> {
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.png.clone())
    }
}

fn memory_fetcher() -> std::sync::Arc<MemoryFetcher> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(800, 400, image::Rgba([40, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    std::sync::Arc::new(MemoryFetcher {
        png,
        requests: Default::default(),
    })
}

/// Networks 1..=`servers` are user-added servers, each with the same
/// channels, followed by the two IRCnet presets without channels.
fn settings(servers: usize) -> cayenchat_storage::Settings {
    let channels = (0..CHANNELS).map(channel).collect::<Vec<_>>().join(",");
    let mut settings = cayenchat_storage::Settings::default();
    for index in 0..servers {
        settings.add_server("");
        let profile = settings.selected_profile_mut().unwrap();
        profile.host = format!("irc{index}.load.invalid");
        profile.channels = channels.clone();
    }
    settings
}

fn run(cx: &mut TestAppContext, servers: usize, images: bool, avatars: bool) {
    let networks: Vec<NetworkId> = (1..=servers as u32).map(NetworkId).collect();
    cx.update(|cx| {
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        crate::secrets::install_memory(cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ))
    });
    let mut settings = settings(servers);
    settings.appearance.image_previews = images;
    settings.appearance.user_avatars = avatars;
    let fetcher = memory_fetcher();
    let (chat, cx) = cx.add_window_view(|window, cx| {
        let mut chat = ChatWindow::with_settings(settings, None, window, cx);
        chat.previews.use_fetcher(fetcher.clone());
        chat.avatars.use_fetcher(fetcher.clone());
        chat
    });

    // Registered session with rosters and saturated logs and diagnostics.
    let mut setup = vec![Event::Registered {
        nickname: "perfclient".into(),
    }];
    for index in 0..CHANNELS {
        setup.push(Event::Joined {
            channel: channel(index),
        });
        setup.push(Event::Names {
            channel: channel(index),
            users: (0..MEMBERS)
                .map(|member| format!("user{member:03}"))
                .collect(),
        });
    }
    if avatars {
        setup.extend((0..MEMBERS).map(|member| Event::UserAvatar {
            nickname: format!("user{member:03}"),
            url: Some(format!("https://avatars.load.example/{member:03}/{{size}}")),
        }));
    }
    for network in &networks {
        chat.update(cx, |chat, cx| {
            chat.handle_events(*network, setup.clone(), false, cx)
        });
    }
    let mut sequence = 0;
    let history: Vec<Event> = (0..HISTORY * CHANNELS)
        .flat_map(|_| {
            sequence += 1;
            incoming(sequence, images)
        })
        .collect();
    for network in &networks {
        for batch in history.chunks(EVENT_BATCH) {
            chat.update(cx, |chat, cx| {
                chat.handle_events(*network, batch.to_vec(), false, cx)
            });
        }
    }
    cx.run_until_parked();

    let (ids, input) = chat.update(cx, |chat, _| {
        let ids: Vec<_> = chat.state.conversations().iter().map(|c| c.id).collect();
        chat.state.dispatch(Command::SelectChannel(ids[0]));
        let input = chat.inputs[&Selection::Channel(ids[0])].clone();
        (ids, input)
    });
    cx.update(|window, cx| {
        window.focus(&input.focus_handle(cx));
        window.refresh();
    });
    cx.run_until_parked();
    let (retained, diagnostics) = chat.read_with(cx, |chat, _| {
        (
            chat.state
                .conversations()
                .iter()
                .map(|c| c.messages.len())
                .sum::<usize>(),
            chat.sessions
                .values()
                .map(|session| session.diagnostics.len())
                .sum::<usize>(),
        )
    });
    println!(
        "perf_baseline servers={servers} previews={images} avatars={avatars} setup channels={CHANNELS} members={MEMBERS} retained_messages={retained} diagnostics={diagnostics}"
    );

    // Typing into the selected channel's draft; panes must stay cached.
    let renders = chat.read_with(cx, |chat, _| chat.pane_renders);
    let mut typing = Timings::new(servers, "typing");
    for _ in 0..TYPING_SAMPLES {
        typing.time(|| cx.simulate_input("a"));
    }
    assert_eq!(chat.read_with(cx, |chat, _| chat.pane_renders), renders);
    typing.report();

    // Selecting another channel through the same path as the shortcuts.
    let mut switching = Timings::new(servers, "channel_switch");
    for index in 0..SWITCH_SAMPLES {
        let id = ids[(index + 1) % ids.len()];
        switching.time(|| {
            cx.update(|window, cx| {
                chat.update(cx, |chat, cx| {
                    chat.dispatch(Command::SelectChannel(id), window, cx)
                })
            })
        });
        // Previews requested by the switch load outside the timed section.
        cx.run_until_parked();
    }
    switching.report();

    // Scrolling the selected main log up through its history, 20 rows (one
    // image line with previews on) per step, and back down.
    let selected = chat.read_with(cx, |chat, _| chat.state.selection());
    let rows = chat.read_with(cx, |chat, _| chat.main_lists[&selected].state.item_count());
    let mut scrolling = Timings::new(servers, "scroll_20_rows");
    for index in 0..SCROLL_SAMPLES {
        let step = if index < SCROLL_SAMPLES / 2 {
            index + 1
        } else {
            SCROLL_SAMPLES - index - 1
        };
        let item_ix = rows.saturating_sub(step * IMAGE_EVERY);
        scrolling.time(|| {
            cx.update(|window, cx| {
                chat.update(cx, |chat, _| {
                    chat.main_lists[&selected]
                        .state
                        .scroll_to(gpui::ListOffset {
                            item_ix,
                            offset_in_item: gpui::px(0.),
                        })
                });
                window.refresh();
            })
        });
        cx.run_until_parked();
        if index == SCROLL_SAMPLES / 2 - 1 {
            let top = chat.read_with(cx, |chat, _| {
                chat.main_lists[&selected]
                    .state
                    .logical_scroll_top()
                    .item_ix
            });
            println!("perf_baseline servers={servers} scrolled_to_row={top} of {rows}");
        }
    }
    scrolling.report();

    // A full event batch (128 incoming lines) while a channel is selected.
    let mut burst = Timings::new(servers, "event_batch_256");
    for index in 0..BURST_SAMPLES {
        let batch: Vec<Event> = (0..EVENT_BATCH / 2)
            .flat_map(|_| {
                sequence += 1;
                incoming(sequence, images)
            })
            .collect();
        let network = networks[index % networks.len()];
        burst.time(|| chat.update(cx, |chat, cx| chat.handle_events(network, batch, false, cx)));
    }
    burst.report();

    // Typing again after the burst; the panes are cached again.
    cx.run_until_parked();
    let mut typing_after = Timings::new(servers, "typing_after");
    for _ in 0..TYPING_SAMPLES {
        typing_after.time(|| cx.simulate_input("b"));
    }
    typing_after.report();

    chat.read_with(cx, |chat, _| {
        let cache = chat.previews.cache();
        let stats = cache.stats();
        println!(
            "perf_baseline servers={servers} previews={images} fetches={} jobs={} ready={} evicted={} dropped_from_queue={} records={} ready_bytes={} in_flight={} queued={}",
            fetcher.requests.load(std::sync::atomic::Ordering::Relaxed),
            stats.jobs_started,
            stats.ready,
            stats.evicted,
            stats.dropped_from_queue,
            cache.records(),
            cache.ready_bytes(),
            cache.in_flight(),
            cache.queued(),
        );
        let cache = chat.avatars.cache();
        let stats = cache.stats();
        println!(
            "perf_baseline servers={servers} avatars={avatars} jobs={} ready={} failed={} evicted={} dropped_from_queue={} records={} ready_bytes={} in_flight={} waiting_release={} message_size={}",
            stats.jobs_started,
            stats.ready,
            stats.failed,
            stats.evicted,
            stats.dropped_from_queue,
            cache.records(),
            cache.ready_bytes(),
            cache.in_flight(),
            chat.avatars.waiting_release(),
            std::mem::size_of::<cayenchat_model::Message>(),
        );
    });
}
