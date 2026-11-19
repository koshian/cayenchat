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
