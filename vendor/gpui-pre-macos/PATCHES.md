# Patches to gpui-pre-macos 0.3.12

Base: the published `gpui-pre-macos` 0.3.12 (`src`, license).

## Roman input sources for direct-input fields (#1081)

`set_input_handler` limits the window's own view's `NSTextInputContext`
(`allowedInputSourceLocales`) to `NSAllRomanInputSourcesLocaleIdentifier`
while the registered handler `prefers_direct_input()` (gpui-pre's patch), as
AppKit does for a password field; `nil` lifts it and AppKit returns to the
user's input source. It acts on the window's own view, never on
`currentInputContext`, only when the state changes, and a missing input
context is not recorded as done, so the next registration asks again.

Not compiled on Linux; check on macOS with the Japanese input sources.
