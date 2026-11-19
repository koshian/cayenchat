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
