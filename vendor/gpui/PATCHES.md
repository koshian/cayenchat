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

In `src/platform.rs`, `src/input.rs`, `src/window.rs` and the platform windows,
the per-window IME switch is ported from upstream Zed (`update_ime_enabled` in
`crates/gpui_windows/src/events.rs` and the Wayland window):
`InputHandler::accepts_text_input` (upstream's name and meaning, default true),
the CayenChat-only `InputHandler::prefers_direct_input` (default false) and
`PlatformWindow::update_ime_enabled`. `Window::draw` calls the latter only when
`accepts_text_input && !prefers_direct_input` changes. Windows detaches the IME
with `ImmAssociateContextEx` after the frame and discards the composition commit
that ends it (it would reach the newly focused field); Wayland skips `enable` on
text-input enter while off and switches the shared text-input only for the
keyboard-focused window; macOS limits the window's own view's
`allowedInputSourceLocales` to Roman input sources. `update_ime_enabled` returns
whether it was applied; `Window` retries on the next frame otherwise. X11 keeps
the default no-op.

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
the paths), Submit and Exited. `ExternalPaths` carries a handle
(`ExternalPaths::keep_alive`, a `PromisedFiles`) that removes the directory
when the last clone is dropped, so a handler that reads the files later, off
the UI thread, holds the handle until it is done; other platforms return
`None`. Upstream registered only
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

In `src/elements/text.rs`, `InteractiveText::hoverable_tooltip` is `tooltip`
whose bubble stays while the pointer moves onto it (upstream hardcoded
non-hoverable text tooltips), so a click on it can be handled. CayenChat's
shortened URLs open from their full-URL bubble (issue #232).
The tooltip belongs to the text range `hoverable_tooltip`'s `anchor` returns
for the hovered byte: while it is shown, the pointer on other text (another
URL) no longer keeps it, so it is replaced instead of opening a stale target.
`tooltip_bridge` declares the tooltip view's transparent margin (left, top)
that only bridges the gap to the pointer. A pointer on another range with a
tooltip of its own, outside the visible part of the shown one, replaces the
tooltip at once (`TooltipRetarget` in `src/elements/div.rs`: the shown tooltip
is cleared and the next one is scheduled without another mouse move).

In `src/platform/windows/destination_list.rs`, the jump list's removed-item
descriptions are measured up to their NUL in Rust instead of with ICU's
`u_strlen`. That one call made the executable import `icuuc.dll`, which
Windows only ships since Windows 10 1703 and Wine does not provide, so the
Windows build did not start under Wine at all. Nothing else uses ICU.

In `src/platform/windows/{events,platform}.rs`, windows handle
`WM_QUERYENDSESSION` (always TRUE, never delays or vetoes the OS) and
`WM_ENDSESSION`. When the session really ends (`wParam` TRUE, including
Restart Manager closes with `ENDSESSION_CLOSEAPP`), the window synchronously
sends a private message to the platform window, which runs the quit callback
(`App::shutdown`, hence `on_app_quit` handlers) before returning to Windows;
`run` skips it afterwards. Upstream ignored both messages, so sign-out,
shutdown and restart killed the process without running any quit handler.
CayenChat uses this to send IRC QUIT (issue #284).

In `src/window/prompts.rs`, `FallbackPromptRenderer` (used on X11 and Wayland)
answers Escape with the prompt's cancel button, is wider and centers the detail
text, so a long detail is no longer cut off. Upstream handles clicks only. This
applies to every fallback prompt in the application, not only the quit one.

In `src/app.rs`, `src/platform.rs` and `src/platform/mac/platform.rs`,
`App::on_quit_request` lets the application cancel a quit the user starts from
outside it. The macOS delegate implements `applicationShouldTerminate:` and asks
the handler for a Dock "Quit". Quits from `App::quit` and the system's shutdown,
restart and log out (a quit Apple event with a reason other than "quit") are
never asked, and other platforms ignore the handler.

In `src/platform/app_menu.rs` and `src/platform/mac/platform.rs`, `Menu` and
`OwnedMenu` gain a `menu_type: Option<SystemMenuType>` field and
`SystemMenuType` gains `Window`. macOS hands a top-level menu marked
`SystemMenuType::Window` to AppKit as the Window menu (`setWindowsMenu:`), so
AppKit adds the window list and other window items. Upstream did this only for
a menu whose name was exactly `"Window"`, so translating the title (CayenChat's
Japanese "ウインドウ") lost the system items. A nested `os_submenu` of type
`Window` is registered the same way. Other platforms ignore the field.

In `src/platform/windows/window.rs`, `activate` does not send the simulated
Alt press when a window of this process is already the foreground window
(`GetForegroundWindow` + `GetWindowThreadProcessId`): `SetForegroundWindow`
succeeds then, and the Alt would only reach our own window and focus its menu
bar. CayenChat activates the chat window and then the initial settings window
at startup (issue #354).

In `src/platform.rs` and the Linux, macOS and Windows `prompt_for_paths`
implementations, `PathPromptOptions` has a `filter: Option<PathPromptFilter>`
(a name and a list of extensions) so a file dialog can offer only some files:

- Windows: `IFileOpenDialog::SetFileTypes` with one `*.ext;...` entry.
- macOS: `NSOpenPanel` `allowedFileTypes` with the extensions. It is the
  deprecated form, chosen because it needs no UniformTypeIdentifiers symbols
  (`allowedContentTypes` would); current macOS still honors it.
- Linux: an `ashpd` `FileFilter` with a lower- and an upper-case glob per
  extension (portal globs are case-sensitive). Portals that ignore filters show
  every file.

In `src/window/prompts.rs`, the root of `FallbackPromptRenderer` occludes the
mouse (`.occlude()`). Without it a click on the dialog's button (or anywhere on
its backdrop) also reached the views behind it, so closing a CayenChat notice
could select the channel tree row under the button.

In `src/text_system.rs`, `src/text_system/line_layout.rs` and
`src/elements/text.rs`, phrase-aware wrapping (CayenChat #537): `StyledText::with_wrap_joins`
takes sorted byte offsets, passed through the new `TextSystem::shape_text_joined`
(`shape_text` is unchanged and passes none) into `compute_wrap_boundaries`, which
then does not use a position before such an offset as a wrap opportunity. A line
with no other opportunity still breaks at the overflowing glyph. The offsets are
part of the wrapped-line cache key.

In `src/text_system/line_wrapper.rs`, `LineWrapper::is_word_char` counts the
no-break space (U+00A0), figure space (U+2007) and narrow no-break space
(U+202F) as word characters. They offered a wrap opportunity before each of
them, so the room CayenChat keeps before a link icon (#550) could be split
across two rows and the icon covered the start of the URL. Both wrappers
(`LineWrapper::wrap_line` and `compute_wrap_boundaries`) use this function.
