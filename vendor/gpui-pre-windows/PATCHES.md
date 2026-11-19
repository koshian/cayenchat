# Patches to gpui-pre-windows 0.3.12

Base: the published `gpui-pre-windows` 0.3.12 (`src`, `build.rs`, license).

## Discarding the IME commit while detaching (#1081)

`update_ime_enabled` completes the composition (`CPS_COMPLETE`) before it
detaches the IME. By then the registered input handler is the field that just
took focus, so the committed string would land there (for example text typed
in a password field replacing the selection of the next field). While the
call runs, `WindowsWindowState::discarding_ime_commit` is set and
`handle_ime_composition` answers `WM_IME_COMPOSITION` without delivering
anything. The old field keeps the text as marked text and drops the mark when
it loses focus. `update_ime_enabled` asks `query_ime_enabled()` (gpui-pre's
`prefers_direct_input` patch) instead of `query_accepts_text_input()`.

Not built or run in CI on Linux; check with MS-IME and Google Japanese Input
on Windows.

## Window teardown and activation (#1081)

- `WindowsWindow`'s drop skips `RevokeDragDrop`/`DestroyWindow` when `IsWindow`
  reports the handle is gone. Closing from the title bar or Alt+F4 lets
  `DefWindowProcW` destroy the window first, so upstream logged `0x80040102`
  and `0x80070578` every time.
- `activate` does not send the simulated Alt press when a window of this
  process is already the foreground window (`GetForegroundWindow` +
  `GetWindowThreadProcessId`): `SetForegroundWindow` succeeds then, and the
  Alt would only reach our own window and focus its menu bar (#354).

Check on Windows: no `0x80040102` in the log after closing a window, and no
menu bar focus when the settings window opens at startup.

## File dialog filter (#1081)

`file_open_dialog` sets `PathPromptOptions::filter` (gpui-pre's patch) with
`SetFileTypes` as one `*.ext;*.ext` entry named by the filter. Not compiled on
Linux; check on Windows that the open dialog lists only the offered images.

## Clipboard text size (#1081, CayenChat #684)

`get_clipboard_string` (`clipboard.rs`) decodes through
`decode_clipboard_utf16` (`clipboard_data.rs`), which refuses an empty or
odd-sized allocation, one over 16 MiB, and one without a UTF-16 NUL inside the
`GlobalSize`-bounded bytes. Upstream scans for a NUL but falls back to the
whole buffer and has no size cap. The decoder has no `windows` dependency and
its tests run in CayenChat's workspace (`crates/ui/src/main.rs`).
