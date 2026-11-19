# Patches to gpui-pre-wgpu 0.3.12

Base: the published `gpui-pre-wgpu` 0.3.12 without `benches/` and `Cargo.lock`.
It is the text system and renderer of `gpui-pre-linux` (Linux and FreeBSD).

## Shaping cache (#1081)

In `Cargo.toml` and `src/cosmic_text_system.rs`, cosmic-text's
`shape-run-cache` feature is enabled, so Linux shaping reuses per-word
results. GPUI's own line layout cache keeps only the previous frame, so every
channel switch reshaped all newly visible log lines, taking roughly 0.1–0.3 ms
per line (about 10–20 ms per switch). cosmic-text never evicts on its own, so
the cache is swept every 128 shaped lines (`SHAPE_RUN_CACHE_TRIM_INTERVAL`),
dropping words unused for four sweeps (`SHAPE_RUN_CACHE_KEEP_AGES`). Measured
with the GPUI 0.2.2 copy in a Debian container: 60 new mixed lines ~10 ms →
~2 ms, 60 lines shown again ~10 ms → ~0.8 ms, at most ~8 MB extra.
