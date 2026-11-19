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

## Quit request and Window menu (#1081)

- The application delegate implements `applicationShouldTerminate:` and asks
  the `on_quit_request` callback (gpui-pre's patch) for a Dock "Quit".
  `Platform::quit` sets `APP_INITIATED_QUIT` first, and a quit Apple event
  with a reason other than the plain "quit" (shutdown, restart, log out) is
  recognised through `NSAppleEventManager`; neither is asked.
- `create_menu_bar` hands a top-level menu whose `menu_type` is
  `SystemMenuType::Window` to `setWindowsMenu:` (upstream compares the name
  with `"Window"`), and a nested `os_submenu` of that type is registered the
  same way.

Not compiled on Linux; check on macOS: Dock "Quit" asks, cancelling keeps the
app, the Japanese-titled Window menu lists the windows.

## File dialog filter and file promises (#1081)

- `prompt_for_paths` sets `allowedFileTypes` from `PathPromptOptions::filter`
  (gpui-pre's patch). The property is deprecated but still honored, and needs
  no UniformTypeIdentifiers symbols.
- The native window also registers for the types `NSFilePromiseReceiver` reads
  (`readableDraggedTypes`) next to `NSFilenamesPboardType`. A drag that carries
  promises instead of paths enters with an empty `ExternalPaths`; on drop the
  promised files are written to a new `0700` directory under
  `$TMPDIR/gpui-file-promises/` (readers run on the main queue), and once all
  have arrived the drop is replayed as Exited, Entered (with the paths),
  Submit, Exited. `ExternalPaths::keep_alive` (gpui-pre's patch) holds the
  directory until the last handle is dropped. Upstream refused these drags.

Not compiled on Linux; check on macOS: the file chooser offers only the
images, and dropping a photo from Photos or the screenshot thumbnail on a
channel or an attachment field attaches it.
