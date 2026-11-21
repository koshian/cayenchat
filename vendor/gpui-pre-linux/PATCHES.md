# Patches to gpui-pre-linux 0.3.12

Base: the published `gpui-pre-linux` 0.3.12 (`src`, `build.rs`, license).

## Direct-input fields (#1081)

The Wayland window's `update_ime_enabled` asks
`PlatformInputHandler::query_ime_enabled()` (gpui-pre's `prefers_direct_input`
patch) instead of `query_accepts_text_input()`. The text-input `enter` event
(upstream: enables the IME unconditionally) asks the keyboard-focused window's
handler first and leaves the IME off for a direct-input field; with no handler
it still enables and the next frame decides. X11 has no per-field switch.

## File dialog filter (#1081)

`prompt_for_paths` passes `PathPromptOptions::filter` (gpui-pre's patch) to the
XDG portal as a `FileFilter`. Portal globs are case-sensitive, so each
extension is added in lower and upper case.

## Transparency and blur queries (#1081)

Answers gpui-pre's `is_compositing` and `supports_background_blur` patch. X11
`is_compositing` is whether the `_NET_WM_CM_S<n>` selection of the window's
screen has an owner (`compositor_selection_owned`; `check_compositor_present`
also accepts a mere window manager, so it is not used); without a compositor
the X server composes see-through pixels over black. X11 has no blur request
(`supports_background_blur` is false). Wayland answers true when the
ext-background-effect or the KDE blur manager global was bound; the manager's
capability event is not consulted.

## Clipboard reads (#1081, CayenChat #684)

Upstream reads a Wayland selection's pipe on the calling thread with an idle
timeout and no size limit, so a slow selection froze the UI and an endless one
grew memory without bound.

- `read_fd_with_timeout` (`linux/platform.rs`) now refuses more than 32 MiB
  (`PIPE_READ_MAX_BYTES`, through `read_fd_bounded`). The synchronous clipboard
  read and the drag-and-drop file list read both go through it.
- `Platform::read_from_clipboard_async` is overridden (`linux/platform.rs`,
  `display_connection.rs`, `wayland/client.rs`). Wayland runs
  `Clipboard::read_async` (`wayland/clipboard.rs`), which does the existing
  text-then-image read on the background executor; X11 and headless keep the
  synchronous read. The pipe is read by `read_fd_async` (`linux/platform.rs`),
  which awaits a short timer while no data is ready, so it never holds a worker
  while waiting. It ends at one 5 second deadline for the whole paste
  (`CLIPBOARD_READ_DEADLINE`) however steadily the owner sends, and all formats
  of one paste share one 32 MiB budget. Dropping the task drops the future,
  which ends the read and closes the pipe. Tests run the real function on a
  real Linux dispatcher: a steady trickle hits the deadline, more waiting reads
  than workers leave an unrelated task running, and cancelling closes the pipes.
  (The synchronous read keeps its 4 second idle timeout; it blocks the caller
  anyway.)
Check on Wayland: pasting text and an image still works, and the UI stays
responsive while a selection owner stalls.

## X11 input context (#1081, CayenChat #227)

`XimHandler` keeps the input style chosen by `xim::choose_input_style` from the
server's `XNQueryInputStyle` reply (upstream hard-codes `PREEDIT_CALLBACKS`,
which IBus does not offer) and uses it for every `create_ic`, including the one
in `enable_ime`. `set_ic_values` sends only `FocusWindow` and the preedit spot;
`InputStyle` and `ClientWindow` are creation-time only. The attribute lists are
built in `XimHandler::create_ic_attributes` and `spot_ic_attributes`, which the
tests in `xim_handler.rs` check. Needs `zed-xim` to export `ClientCore` for the
test client.
