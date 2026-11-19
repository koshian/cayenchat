# Patches to gpui-pre-linux 0.3.12

Base: the published `gpui-pre-linux` 0.3.12 (`src`, `build.rs`, license).

## Direct-input fields (#1081)

The Wayland window's `update_ime_enabled` asks
`PlatformInputHandler::query_ime_enabled()` (gpui-pre's `prefers_direct_input`
patch) instead of `query_accepts_text_input()`. Known gap: the text-input
`enter` event enables the IME unconditionally, so a direct-input field keeps
it on after a window switch until the next frame (the 0.2.2 copy honored the
wish on enter). X11 has no per-field switch.
