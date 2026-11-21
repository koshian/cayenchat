//! Shapes Japanese/English log lines with GPUI's real Linux text system.
//!
//! Default build: GPUI 0.3.12 (`gpui-pre-wgpu`, cosmic-text 0.19); `--features
//! cache` turns on cosmic-text's `shape-run-cache`. The `legacy` feature is for
//! a copy of `vendor/gpui` 0.2.2 (the master backend) whose `CosmicTextSystem`
//! was made public; see `spec/performance.md`.

use gpui::{FontRun, PlatformTextSystem, font, px};
use std::time::Instant;

#[cfg(not(feature = "legacy"))]
fn text_system() -> impl PlatformTextSystem {
    gpui_wgpu::CosmicTextSystem::new("sans-serif")
}

#[cfg(feature = "legacy")]
fn text_system() -> impl PlatformTextSystem {
    gpui::platform::linux::CosmicTextSystem::new()
}

fn line(i: usize) -> String {
    match i % 5 {
        0 => format!("<nick{i}> こんにちは、今日はいい天気ですね。散歩に行きませんか？ {i}"),
        1 => format!("<nick{i}> short ascii line number {i}"),
        2 => format!(
            "<nick{i}> The quick brown fox jumps over the lazy dog, again and again {i} and more text to wrap"
        ),
        3 => {
            format!("<nick{i}> see https://example.com/path/{i}?q=日本語 for details 詳細はこちら")
        }
        _ => format!(
            "<nick{i}> 吾輩は猫である。名前はまだ無い。どこで生まれたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。{i}"
        ),
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let sys = text_system();
    let id = sys.font_id(&font("Noto Sans CJK JP")).unwrap();
    let shape = |t: &str| {
        let runs = [FontRun {
            len: t.len(),
            font_id: id,
        }];
        std::hint::black_box(sys.layout_line(t, px(14.0), &runs));
    };
    // 60 visible lines per switch; 20 channels alternate.
    let channels: Vec<Vec<String>> = (0..20)
        .map(|c| (0..60).map(|i| line(c * 1000 + i)).collect())
        .collect();
    let shape_channel = |ch: &Vec<String>| ch.iter().for_each(|t| shape(t));
    // Warm up the fonts.
    shape_channel(&channels[19]);

    // First display of a channel: lines never shaped before.
    let mut first = vec![];
    for ch in &channels[..19] {
        let t = Instant::now();
        shape_channel(ch);
        first.push(t.elapsed().as_secs_f64() * 1e3);
    }
    // Switching back and forth: every line was shaped before, but GPUI keeps
    // only the previous frame's lines, so each switch shapes them again.
    let mut again = vec![];
    for r in 0..5 {
        for ch in &channels[..19] {
            let t = Instant::now();
            shape_channel(ch);
            if r > 0 {
                again.push(t.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
    // Alternating between three channels (about 180 lines), which fits the
    // window of a per-word cache.
    let mut few = vec![];
    for r in 0..20 {
        for ch in &channels[..3] {
            let t = Instant::now();
            shape_channel(ch);
            if r > 2 {
                few.push(t.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
    // Scrolling: one new line at a time, never shaped before.
    let scroll: Vec<String> = (0..1000).map(|i| line(100_000 + i)).collect();
    let t = Instant::now();
    scroll.iter().for_each(|t| shape(t));
    let scroll_us = t.elapsed().as_secs_f64() * 1e6 / scroll.len() as f64;

    println!(
        "backend={} cache={} first(ms/60 lines) median={:.2} again19 median={:.2} again19 max={:.2} again3 median={:.2} scroll(us/new line)={:.1}",
        if cfg!(feature = "legacy") {
            "gpui-0.2.2"
        } else {
            "gpui-0.3.12"
        },
        // vendor/gpui 0.2.2 always enables the cache.
        cfg!(any(feature = "cache", feature = "legacy")),
        median(&mut first),
        median(&mut again.clone()),
        again.iter().cloned().fold(0.0, f64::max),
        median(&mut few),
        scroll_us
    );
}
