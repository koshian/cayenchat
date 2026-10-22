# Vendored crates

Each directory is a copy of a published crate with local fixes, used through
`[patch.crates-io]` in the workspace `Cargo.toml`. They are temporary: when
upstream publishes a release with the fix, switch back to it and delete the
copy. Check the conditions below when updating dependencies.

| Crate | Base | Why | Switch back when |
| --- | --- | --- | --- |
| `gpui` | 0.2.2 | Windows IME keys, Linux decorations, atlas reuse and more; see `gpui/PATCHES.md` | A GPUI release contains every fix in `gpui/PATCHES.md`, including running quit handlers on Windows `WM_ENDSESSION` (or they are no longer needed) |
| `zed-xim` | 0.4.0-zed | Lenient COMPOUND_TEXT decoding instead of panics (IBus/Mozc), XIM input style selection, bounded data property atoms | A `zed-xim` release decodes COMPOUND_TEXT without panicking and bounds the atoms; the style selection would move back into GPUI |
| `native-theme` | 0.5.7 | Windows fonts and metrics are read at 96 DPI so they are logical pixels; 0.5.7 read them at the system DPI and the settings window was scaled twice (#290). Backported from 0.6.1; see `native-theme/PATCHES.md` | The workspace can use native-theme 0.6.1 or later, which needs `native-theme-gpui` on a newer GPUI (#265) |
| `irc-proto` | 1.1.0 | `LineCodec` rejects lines over 16 KiB (`MAX_LINE_BYTES`); upstream buffers a line without limit. Its `encoding` feature uses `encoding_rs` instead of the unmaintained `encoding` crate (RUSTSEC-2021-0153), and the workspace enables it directly rather than through `irc`'s `encoding` feature. `charsets` lets the codec convert the text of a `PRIVMSG`/`NOTICE` to a given target with another encoding than the rest of the line (per-channel `/charset`, #396). `LineCodec::encode` drops a line holding CR, LF or NUL besides the final `\r\n`, so no line, including the `irc` crate's own PONG and CTCP replies, can inject a command (#554) | An `irc-proto` release limits the line length in its codec and decodes legacy charsets with a maintained crate; `irc`'s `encoding` feature must then no longer pull in `encoding` |
| `irc` | 1.1.0 | A received KICK of our own nickname drops the channel's `channel-lists` entry, like a PART we send does (`src/client/mod.rs`). "Our own nickname" is the one the server confirmed (RPL_WELCOME, then our own NICK), not `current_nickname()`, which ignores later nick changes. `Client::forget_channel_list` lets irc-core drop the list when its own casemapping-aware check sees the KICK, because the built-in check is ASCII-only (`nick{1}` vs `nick[1]`). Upstream only removed the kicked user, so a rejoin appended to stale entries: modes went to the old entry, members who left meanwhile stayed, and `list_channels()` still reported the channel (#342). The `body-charsets` feature adds `Config::body_charsets`, handed to the line codec for per-channel text encodings (#396) | `Client` is no longer used (#266); upstream 1.1.0 is the latest release and has no fix |

`irc` and `native-theme` set `[lints.rust] warnings = "allow"` in their
`Cargo.toml`: their upstream code has unused items and old idioms that Cargo
would hide in a crates.io dependency, and fixing them here would only widen
the difference from upstream. Keep local fixes warning-free all the same.

Run the copies' own tests with `cargo test -p zed-xim` and
`cargo test -p irc-proto` (CI does both on Linux). The `native-theme` change
is Windows-only code; it is compiled by the Windows CI builds, and its own test
suite needs dev-dependencies the workspace does not have.
