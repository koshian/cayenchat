# Patches to gpui-pre 0.3.12

Base: the published `gpui-pre` 0.3.12 without `benches/`, `docs/`, `examples/`
and `tests/`.

## FreeBSD build (#1081)

`gpui-pre-linux` serves FreeBSD too, and `gpui-kit` (through `native-theme-gpui`)
turns its `wayland` feature on for every desktop target. 0.3.12 gates the
following on `target_os = "linux"` alone, so `gpui-pre-linux` does not compile
on FreeBSD. They are also gated on `freebsd` here:

- `queue` module and its `PriorityQueueReceiver` / `PriorityQueueSender`
  re-export (`src/gpui.rs`);
- `layer_shell` module, `WindowKind::LayerShell`, `PlatformWindow::set_exclusive_edge`
  and `Window::set_exclusive_edge` (`src/platform.rs`, `src/window.rs`).

Switch back when a `gpui-pre` release builds on FreeBSD.

## Direct-input fields (#1081)

`InputHandler::prefers_direct_input` / `EntityInputHandler::prefers_direct_input`
(default `false`) are CayenChat's own. A field that mostly takes ASCII
(nicknames, hosts, passwords) detaches the platform IME while it has focus.
`accepts_text_input` keeps upstream's meaning (the handler is a text field;
`false` also lets key bindings win over characters), so it cannot do this.

- `Window::draw` reads `prefers_direct_input` when it registers the focused
  handler and stores it in the `PlatformInputHandler`
  (`prefers_direct_input()`); the platforms cannot ask the handler later
  without a re-entrant window update.
- `PlatformInputHandler::query_ime_enabled()` is
  `accepts_text_input && !prefers_direct_input`; the Windows and Wayland
  windows use it where upstream asked `query_accepts_text_input()`.

Upstream already has the `GCS_RESULTSTR`-before-`GCS_COMPSTR` order (#880) and
the language-key handling, so those patches of the 0.2.2 copy are dropped.
Candidate for an upstream proposal.

## Quit request and Window menu (#1081)

- `App::on_quit_request` and `Platform::on_quit_request` (default: ignored).
  The handler is asked before a quit the user starts from outside the
  application and cancels it by returning `false`. `App::quit` and the
  system's shutdown never reach it. If the app state is borrowed when the
  platform asks, the quit is cancelled instead of panicking. Only macOS asks
  (`gpui-pre-macos/PATCHES.md`). Candidate for an upstream proposal.
- `Menu::menu_type: Option<SystemMenuType>` and `OwnedMenu::menu_type` mark a
  top-level menu the system manages whatever its (translated) name is.
  `SystemMenuType::Window` is new; macOS gives the marked menu to AppKit as
  the Window menu (and a nested `os_submenu` of that type the same way).
  Upstream decides by the name `"Window"` only. Other platforms ignore it.

## File dialogs and file promises (#1081)

- `PathPromptOptions::filter: Option<PathPromptFilter>` (new type: a name and
  extensions without the dot). A dialog shows only those files where the
  platform can filter (`gpui-pre-{linux,macos,windows}`); the caller still
  checks what it gets. Candidate for an upstream proposal.
- `ExternalPaths` has a second field, `Option<Arc<PromisedFiles>>`, and
  `ExternalPaths::keep_alive()`. `PromisedFiles` owns the temporary directory
  of a macOS file promise (Photos, Mail, the screenshot thumbnail) and removes
  it with the last handle, so a handler that reads the files after it has
  returned keeps one. Other platforms construct `ExternalPaths(paths, None)`.

## Window transparency and background blur (#1081)

`PlatformWindow` and `Window` gain `is_compositing` (default `true`),
`supports_background_blur` (default `true`), `supports_background_blur_radius`
(default `false`) and `set_background_blur_radius(u32)` (default no-op). The
platform crates answer them (`gpui-pre-linux`, `gpui-pre-macos`). Upstream
0.3.12 has none of them, so an app cannot tell that a transparent window is
drawn over black (X11 without a compositor) or that no blur is available.

## Phrase-aware wrapping (#1081, CayenChat #537, #550)

`StyledText::with_wrap_joins(Arc<[u32]>)` takes sorted byte offsets of the
text. They travel through the new `TextSystem::shape_text_joined`
(`shape_text` is unchanged and passes none; a later line of a multi-line text
gets its own copy shifted by the line start) and
`LineLayoutCache::layout_wrapped_line` into `LineLayout::compute_wrap_boundaries`,
which drops the UAX #14 break opportunities at those offsets. A line with no
other opportunity still breaks at the overflowing glyph. The offsets are part
of the wrapped-line cache key (`CacheKey::joins`), so the same text with
other joins is laid out again.

`compute_wrap_boundaries` also adds a break opportunity before a no-break
space (U+00A0) followed by a figure space (U+2007), the room CayenChat keeps
before a link icon (#550). Both are "GL" in UAX #14, which forbids a break
before them, so after a long unspaced prefix the icon could cover the first
letters of the URL; the opportunity lets the room start a row.

Upstream 0.3.12 already treats the no-break spaces as glue (the 0.2.2
patch to `LineWrapper::is_word_char` is dropped with the word-character
wrapper). The unit tests of the 0.2.2 copy do not run in this stripped crate;
`crates/ui/src/phrase_wrap.rs` (`layout_tests`) and
`crates/ui/src/main_tests/link_icon_tests.rs` exercise both changes.

## Hoverable link tooltips (#1081, CayenChat #232)

`InteractiveText::hoverable_tooltip(anchor, builder)` is `tooltip` whose bubble
stays while the pointer moves onto it, so a click on it is handled (the
shortened URL opens from its full-URL bubble; a decoded character reference
has a copy button). Upstream 0.3.12 has `Interactivity::hoverable_tooltip` for
elements but hard-codes text tooltips as not hoverable.

- The tooltip belongs to the text range `anchor` returns for the hovered byte
  (`InteractiveTextState::tooltip_range`). While it is shown, the pointer on
  other text no longer keeps it, so it is replaced instead of opening a stale
  target.
- `InteractiveText::tooltip_bridge(left, top)` declares the tooltip view's
  transparent margin that only bridges the gap to the pointer.
- `TooltipRetarget` (`src/elements/div.rs`) is passed to
  `register_tooltip_mouse_handlers`, `show_tooltip` and
  `handle_tooltip_check_visible_and_update` (`None` for ordinary elements).
  A pointer on another range with a tooltip of its own, outside the visible
  part of the shown tooltip, clears it and schedules the next one at once
  (`schedule_tooltip_show`, the former inline code of the mouse-move handler,
  with the default show delay), without another mouse move.

`scripts/e2e/url_bubble_gui.py` (real binary under Xvfb) and
`a_decoded_reference_shows_its_original_and_copies_only_that` exercise it.

## Quiet callbacks for removed windows (#1081)

Platform callbacks registered in `Window::new` (frame requests, resize, move,
activation, hover, visibility, input, ...) can still arrive after GPUI removed
the window while the native one is torn down; Windows sends activation and
redraw messages around `DestroyWindow`. Upstream logged `window not found` for
each. `LogUnlessWindowClosed::log_unless_window_closed` (`src/window.rs`) drops
the error when `AsyncApp::window_closed` (`src/app/async_context.rs`) says the
window is gone, and still logs any other failure. Callbacks that do not touch
the window (tabs, accessibility, close) keep `log_err`.

## Fallback prompt (#1081)

`FallbackPromptRenderer` (`src/window/prompts.rs`), the Linux dialog:

- Escape picks the cancel button (`KeyDownEvent`, `PromptButton::is_cancel`).
- The box is `w_96` instead of `w_72`, and the detail text is centered at full
  width, so CayenChat's notices do not wrap word by word.
- The root occludes the mouse (`.occlude()`). Without it a click on the
  dialog's button (or on its backdrop) also reached the views behind it, so
  closing a CayenChat notice could select the channel tree row under it.

## List content height (#1081)

`ListState::content_height()` (`src/elements/list.rs`) returns the height of
all items. 0.2.2 reported it as the scroll offset of a list that follows its
bottom; 0.3.12 reports the overflow (0 for a short list) and has no bounds for
rows before the scroll top, so `LogList::unfilled_from` could not find where a
short log ends (the filler above it is drawn from there). Candidate for an
upstream proposal.

## Bounds of the focused element (#622)

`Window::focused_bounds()` returns the bounds the element holding the focus
painted last, and `Window::clear_focused_bounds()` forgets them. `Div::paint`
records them when its tracked focus handle is the focused one
(`src/elements/div.rs`, `Window::focused_bounds` in `src/window.rs`). The
settings window follows Tab inside the GPUI Kit's pages, whose inputs report
nothing themselves and which give no scroll handle (`focus_reveal`). Candidate
for an upstream proposal.

## One tab stop per focus handle (#622)

`TabStopMap::insert` (`src/tab_stop.rs`) ignores a handle that is already in the
map. The GPUI Kit's number input tracks one handle on its frame and on the
editor inside it; the second entry was its own neighbour, so Shift+Tab stayed
on the field. Candidate for an upstream proposal.
