use gpui::{FontRun, PlatformTextSystem, font, px};
use gpui_wgpu::CosmicTextSystem;
use std::time::Instant;

fn line(i: usize) -> String {
    match i % 5 {
        0 => format!("<nick{i}> こんにちは、今日はいい天気ですね。散歩に行きませんか？ {i}"),
        1 => format!("<nick{i}> short ascii line number {i}"),
        2 => format!("<nick{i}> The quick brown fox jumps over the lazy dog, again and again {i} and more text to wrap"),
        3 => format!("<nick{i}> see https://example.com/path/{i}?q=日本語 for details 詳細はこちら"),
        _ => format!("<nick{i}> 吾輩は猫である。名前はまだ無い。どこで生まれたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。{i}"),
    }
}

fn main() {
    let sys = CosmicTextSystem::new("sans-serif");
    let id = sys.font_id(&font("Noto Sans CJK JP")).unwrap();
    // 60 visible lines per switch; 20 channels alternate.
    let channels: Vec<Vec<String>> = (0..20)
        .map(|c| (0..60).map(|i| line(c * 1000 + i)).collect())
        .collect();
    let shape = |ch: &Vec<String>| {
        for t in ch {
            let runs = [FontRun { len: t.len(), font_id: id }];
            std::hint::black_box(sys.layout_line(t, px(14.0), &runs));
        }
    };
    // warm up fonts
    shape(&channels[19]);
    let mut first = vec![];
    for ch in &channels[..19] {
        let t = Instant::now();
        shape(ch);
        first.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let mut again = vec![];
    for r in 0..5 {
        for ch in &channels[..19] {
            let t = Instant::now();
            shape(ch);
            if r > 0 { again.push(t.elapsed().as_secs_f64() * 1e3); }
        }
    }
    let med = |v: &mut Vec<f64>| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] };
    println!("cache={} first(ms/60 lines) median={:.2} again median={:.2} again max={:.2}",
        cfg!(feature = "cache"), med(&mut first), med(&mut again.clone()), again.iter().cloned().fold(0.0, f64::max));
}
