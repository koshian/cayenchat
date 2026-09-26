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
