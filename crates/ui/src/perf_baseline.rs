//! Headless timing baseline for typing, channel switching and event bursts.
//!
//! Ignored by default. Run it in release mode:
//!
//! ```sh
//! cargo test --release --locked -p cayenchat-ui perf_baseline -- --ignored --nocapture
//! ```
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
use cayenchat_storage::Settings;
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

const TEXTS: [&str; 4] = [
    "a somewhat longer line of ordinary IRC chatter about the build and the release plan",
    "日本語のメッセージです。チャンネル切替と入力の応答性を確認するための行です。",
    "mixed 日本語 and English text with a URL https://example.invalid/path?q=1",
    "short line",
];

fn channel(index: usize) -> String {
    format!("#perf{index:02}")
}

/// One incoming line as the worker reports it: its wire diagnostic, then the
/// translated message.
fn incoming(sequence: usize) -> [Event; 2] {
    let channel = channel(sequence % CHANNELS);
    let sender = format!("user{:03}", sequence % MEMBERS);
    let text = TEXTS[sequence % TEXTS.len()].to_owned();
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
        },
    ]
}

struct Timings {
    name: &'static str,
    samples: Vec<Duration>,
}

impl Timings {
    fn new(name: &'static str) -> Self {
        Self {
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
            "perf_baseline {:<16} n={:<4} median={:>9.1}us p95={:>9.1}us max={:>9.1}us",
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
    cx.update(|cx| {
        crate::apply_shortcuts(crate::ShortcutPrefs::default(), cx);
        cx.set_global(crate::theme::Theme::new(
            cayenchat_storage::ThemeMode::Light,
            gpui::WindowAppearance::Light,
            &cayenchat_storage::Appearance::default(),
        ))
    });
    let settings = Settings {
        channels: (0..CHANNELS).map(channel).collect::<Vec<_>>().join(","),
        ..Settings::default()
    };
    let (chat, cx) =
        cx.add_window_view(|window, cx| ChatWindow::with_settings(settings, None, window, cx));

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
    chat.update(cx, |chat, cx| chat.handle_events(setup, false, cx));
    let mut sequence = 0;
    let history: Vec<Event> = (0..HISTORY * CHANNELS)
        .flat_map(|_| {
            sequence += 1;
            incoming(sequence)
        })
        .collect();
    for batch in history.chunks(EVENT_BATCH) {
        chat.update(cx, |chat, cx| chat.handle_events(batch.to_vec(), false, cx));
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
            chat.diagnostics.len(),
        )
    });
    println!(
        "perf_baseline setup channels={CHANNELS} members={MEMBERS} retained_messages={retained} diagnostics={diagnostics}"
    );

    // Typing into the selected channel's draft; panes must stay cached.
    let renders = chat.read_with(cx, |chat, _| chat.pane_renders);
    let mut typing = Timings::new("typing");
    for _ in 0..TYPING_SAMPLES {
        typing.time(|| cx.simulate_input("a"));
    }
    assert_eq!(chat.read_with(cx, |chat, _| chat.pane_renders), renders);
    typing.report();

    // Selecting another channel through the same path as the shortcuts.
    let mut switching = Timings::new("channel_switch");
    for index in 0..SWITCH_SAMPLES {
        let id = ids[(index + 1) % ids.len()];
        switching.time(|| {
            cx.update(|window, cx| {
                chat.update(cx, |chat, cx| {
                    chat.dispatch(Command::SelectChannel(id), window, cx)
                })
            })
        });
    }
    switching.report();

    // A full event batch (128 incoming lines) while a channel is selected.
    let mut burst = Timings::new("event_batch_256");
    for _ in 0..BURST_SAMPLES {
        let batch: Vec<Event> = (0..EVENT_BATCH / 2)
            .flat_map(|_| {
                sequence += 1;
                incoming(sequence)
            })
            .collect();
        burst.time(|| chat.update(cx, |chat, cx| chat.handle_events(batch, false, cx)));
    }
    burst.report();

    // Typing again after the burst; the panes are cached again.
    cx.run_until_parked();
    let mut typing_after = Timings::new("typing_after");
    for _ in 0..TYPING_SAMPLES {
        typing_after.time(|| cx.simulate_input("b"));
    }
    typing_after.report();
}
