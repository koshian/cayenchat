# Vendored crates

Each directory is a copy of a published crate with local fixes, used through
`[patch.crates-io]` in the workspace `Cargo.toml`. They are temporary: when
upstream publishes a release with the fix, switch back to it and delete the
copy. Check the conditions below when updating dependencies.

| Crate | Base | Why | Switch back when |
| --- | --- | --- | --- |
| `gpui` | 0.2.2 | Windows IME keys, Linux decorations, atlas reuse and more; see `gpui/PATCHES.md` | A GPUI release contains every fix in `gpui/PATCHES.md`, including running quit handlers on Windows `WM_ENDSESSION` (or they are no longer needed) |
| `zed-xim` | 0.4.0-zed | Lenient COMPOUND_TEXT decoding instead of panics (IBus/Mozc), XIM input style selection, bounded data property atoms | A `zed-xim` release decodes COMPOUND_TEXT without panicking and bounds the atoms; the style selection would move back into GPUI |
| `irc-proto` | 1.1.0 | `LineCodec` rejects lines over 16 KiB (`MAX_LINE_BYTES`); upstream buffers a line without limit. Its `encoding` feature uses `encoding_rs` instead of the unmaintained `encoding` crate (RUSTSEC-2021-0153), and the workspace enables it directly rather than through `irc`'s `encoding` feature | An `irc-proto` release limits the line length in its codec and decodes legacy charsets with a maintained crate; `irc`'s `encoding` feature must then no longer pull in `encoding` |

Run the copies' own tests with `cargo test -p zed-xim` and
`cargo test -p irc-proto` (CI does both on Linux).
