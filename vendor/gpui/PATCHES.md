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

In `src/platform/{mac/metal_atlas.rs,windows/directx_atlas.rs,blade/blade_atlas.rs}`,
`PlatformAtlas::remove` (used by `Window::drop_image`) now always forgets the
removed key and returns the tile's rectangle to the texture's `etagere`
allocator when other tiles keep the texture alive. Upstream only decremented
the texture's reference count, so the space of a dropped image was never
reused, and Metal/DirectX kept the stale key. CayenChat's inline image
previews drop evicted thumbnails; without this, every texture that still held
one live tile (another thumbnail or an emoji glyph) kept all of its dead space
and new thumbnails allocated new 1024×1024 textures (4 MiB each).

In `src/platform.rs`, `Image::to_image_data` swaps the red and blue channels
of rasterized SVG images like it does for every other format: the renderer
expects BGRA, but the SVG branch passed resvg's RGBA through unchanged, so
colored SVGs shown with `img(Arc<Image>)` had red and blue exchanged.
CayenChat's default avatars are such SVGs. (Monochrome `svg()` elements use
a separate path and were unaffected.)

In `src/platform/linux/x11/client.rs`, `process_x11_events` stops the event
loop (so the quit callback runs) when polling fails with a connection error.
Upstream logged a warning and returned; the dead socket stayed readable, so
the same warning repeated forever at 100% CPU after the X server exited
(issue #196).

## Local zed-xim

`vendor/zed-xim` is a copy of `zed-xim` 0.4.0-zed (MIT, used through
`[patch.crates-io]`) without its examples and benches. Its client called
`xim_ctext::compound_text_to_utf8(..).expect(..)` on preedit and commit text, so
IBus/Mozc text that mixes ASCII with UTF-8 or JIS X 0208 segments panicked the
whole app on X11 (issue #227). `src/ctext.rs` now decodes COMPOUND_TEXT leniently
(UTF-8, ASCII, Latin-1, JIS X 0201 katakana, JIS X 0208, GB2312 and KS C 5601, with GL and GR tracked separately; others are dropped) and never fails.

In `src/platform/linux/x11/{xim_handler,client}.rs`, the XIM input context is
created with a style chosen from the server's `XNQueryInputStyle` reply
(preedit callbacks + status nothing; `xim::choose_input_style` in zed-xim so its tests run in CI) instead of a hard-coded
`PREEDIT_CALLBACKS`, which IBus does not offer. `set_ic_values` no longer
resends `InputStyle`/`ClientWindow`, which are creation-time only (issue #227).

Requests too large for a ClientMessage are passed through `_XIM_DATA_N`
properties. `src/property.rs` limits N to 21 names (as Xlib's `_clientN` does)
instead of a new atom per request: IBus' IMdkit offset cache corrupts the heap
of `ibus-x11` past 22 atoms, which dropped the IME connection after a few dozen
keystrokes (issue #227).
