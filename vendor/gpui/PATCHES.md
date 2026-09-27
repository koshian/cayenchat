# Local GPUI 0.2.2 changes

This directory is a copy of the published GPUI 0.2.2 crate, licensed under
Apache-2.0 (`LICENSE-APACHE`). The workspace uses it through `[patch.crates-io]`
because the published crate predates the upstream Windows keyboard/IME fix:
https://github.com/zed-industries/zed/pull/41259

In `src/platform/windows/events.rs`:

- Translate keydown messages even when GPUI cannot name the key, so Win32 and the
  IME still see language-input keys such as Half-width/Full-width.
- Let unrecognized keyup messages reach `DefWindowProcW`.
- Do not turn `VK_PROCESSKEY` back into a shortcut key via `ImmGetVirtualKey`;
  leave IME-owned keys to the IME.
- Translate unhandled system keydown messages as `WM_SYSKEYDOWN` so Win32 can
  produce the appropriate system-character message.

In `src/platform/windows/window.rs`, `WindowsWindow`'s drop skips
`RevokeDragDrop`/`DestroyWindow` when `IsWindow` reports the handle is gone.
Closing from the title bar or Alt+F4 lets `DefWindowProcW` destroy the window
before GPUI drops it, so upstream logged `0x80040102` and `0x80070578`
("invalid window handle") every time.

In `src/window.rs` and `src/app/async_context.rs`, the platform callbacks
registered in `Window::new` (frame, resize, activation, hover, input, ...) no
longer log `window not found` when the window was already removed; Windows
still delivers activation and vsync redraw messages while a removed window is
torn down. Other failures are still logged.

In `src/platform/linux/wayland/window.rs`:

- `request_decorations` keeps client-side decorations when the compositor does
  not offer `xdg-decoration` (GNOME/mutter). Upstream recorded the requested
  server-side mode anyway, so `window_decorations()` reported `Server`, nothing
  drew a title bar, and the window could not be moved.

In `src/platform/linux/{wayland,x11}/client.rs`:

- The XDG portal appearance handler releases the client `RefCell` borrow before
  calling each window's appearance callback. Upstream held it, so an observer
  that called `App::window_appearance()` panicked with "RefCell already
  borrowed" when the desktop color scheme was reported (seen on Wayland at
  startup).

In `Cargo.toml` and `src/platform/linux/text_system.rs`:

- cosmic-text's `shape-run-cache` feature is enabled, so Linux shaping reuses
  per-word results. GPUI's own line layout cache keeps only the previous
  frame, so every channel switch reshaped all newly visible log lines, taking
  roughly 0.1–0.3 ms per line (about 10–20 ms per switch). The cache is swept
  every 128 shaped lines, dropping words unused for four sweeps; cosmic-text
  never evicts on its own. Measured in a Debian container: 60 new mixed lines
  ~10 ms → ~2 ms, 60 lines shown again ~10 ms → ~0.8 ms, at most ~8 MB extra.

In `src/taffy.rs`, the grid `minmax(length(0.0), fr(1.0))` literals are typed
as `f32`, fixing the `float_literal_f32_fallback` future-incompatibility warning
emitted by newer rustc (seen with 1.98).

The existing mixed indentation in `src/platform/mac/shaders.metal` was also
normalized so the vendored source passes `git diff --check`; shader logic is
unchanged.

This is a focused backport, not the complete upstream message-loop rewrite.
Check Windows 11 with MS-IME on a Japanese keyboard before treating the
reported toggle issue as verified. Once a compatible released GPUI version
contains the full fix and the Wayland decoration fallback, remove this override
and this directory.

In `src/platform/mac/window.rs`, windows also accept file-promise drags
(`NSFilePromiseReceiver`), which Photos, Mail and the screenshot thumbnail
use instead of existing file paths. Drop targets see an empty `ExternalPaths`
while such a drag hovers. On drop, the promised files are written into a new
`0700` directory under `$TMPDIR/gpui-file-promises/`; the readers run on the
main queue, and once all files arrive the drop is replayed as Entered (with
the paths), Submit and Exited, after which the directory is removed. Drop
handlers must therefore read the files synchronously. Upstream registered only
`NSFilenamesPboardType`, so these drags were refused.
