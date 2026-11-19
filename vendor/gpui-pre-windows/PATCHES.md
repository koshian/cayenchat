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
