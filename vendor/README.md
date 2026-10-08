# Vendored crates

Each directory is a copy of a published crate with local fixes, used through
`[patch.crates-io]` in the workspace `Cargo.toml`. They are temporary: when
upstream publishes a release with the fix, switch back to it and delete the
copy. Check the conditions below when updating dependencies.

| Crate | Base | Why | Switch back when |
| --- | --- | --- | --- |
| `gpui` | 0.2.2 | Windows IME keys, Linux decorations, atlas reuse and more; see `gpui/PATCHES.md` | A GPUI release contains every fix in `gpui/PATCHES.md` (or they are no longer needed) |
| `zed-xim` | 0.4.0-zed | Lenient COMPOUND_TEXT decoding instead of panics (IBus/Mozc), XIM input style selection, bounded data property atoms | A `zed-xim` release decodes COMPOUND_TEXT without panicking and bounds the atoms; the style selection would move back into GPUI |
| `irc` | 1.1.0 | `ClientStream::close` writes the queue, shuts down the sending side and drains the server, so QUIT is not lost to an RST when the socket is dropped with unread data; the published client exposes no way to end the sending side | An `irc` release can close the sending side and flush a final line before the connection is dropped |
| `irc-proto` | 1.1.0 | `LineCodec` rejects lines over 16 KiB (`MAX_LINE_BYTES`); upstream buffers a line without limit | An `irc-proto` release limits the line length in its codec |

Run the copies' own tests with `cargo test -p zed-xim` and
`cargo test -p irc-proto` (CI does both on Linux).
