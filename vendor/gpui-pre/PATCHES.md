# Patches to gpui-pre 0.3.12

Base: the published `gpui-pre` 0.3.12 without `benches/`, `docs/`, `examples/`
and `tests/`.

## FreeBSD build (#1081)

`gpui-pre-linux` serves FreeBSD too, and `gpui-kit` (through `native-theme-gpui`)
turns its `wayland` feature on for every desktop target. 0.3.12 gates the
following on `target_os = "linux"` alone, so `gpui-pre-linux` does not compile
on FreeBSD. They are also gated on `freebsd` here:

- `queue` module and its `PriorityQueueReceiver` / `PriorityQueueSender`
  re-export (`src/gpui.rs`);
- `layer_shell` module, `WindowKind::LayerShell`, `PlatformWindow::set_exclusive_edge`
  and `Window::set_exclusive_edge` (`src/platform.rs`, `src/window.rs`).

Switch back when a `gpui-pre` release builds on FreeBSD.
