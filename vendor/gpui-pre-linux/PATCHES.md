# Patches to gpui-pre-linux 0.3.12

Base: the published `gpui-pre-linux` 0.3.12 (`src`, `build.rs`, license).

## Direct-input fields (#1081)

The Wayland window's `update_ime_enabled` asks
`PlatformInputHandler::query_ime_enabled()` (gpui-pre's `prefers_direct_input`
patch) instead of `query_accepts_text_input()`. The text-input `enter` event
(upstream: enables the IME unconditionally) asks the keyboard-focused window's
handler first and leaves the IME off for a direct-input field; with no handler
it still enables and the next frame decides. X11 has no per-field switch.
