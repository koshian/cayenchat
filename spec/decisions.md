# Decisions

Record only decisions that materially constrain future work. Keep entries short.

## D001 — Rust

**Status:** Accepted

Use Rust for the application.

Rationale: suitable for a long-running native network client, good async/networking ecosystem, strong cross-platform support, and a good fit for keeping protocol/state logic explicit.

## D002 — GPUI

**Status:** Provisional

Use GPUI as the primary desktop UI framework.

Target both macOS and Windows from the beginning. If a required feature exposes a serious platform blocker, document the issue before changing frameworks.

## D003 — LimeChat as behavioral reference

**Status:** Accepted

Use the open-source LimeChat project as a reference for IRC behavior, feature coverage, workflows, and useful interaction patterns.

Do not mechanically translate its implementation or treat its architecture as mandatory.

Licensing implications must be reviewed before copying implementation code.

## D004 — Chocoa-like look and feel

**Status:** Accepted

The UI should evoke Chocoa's simple, compact, utilitarian desktop-client character
rather than contemporary web-chat aesthetics. Its four information panes are a
structural requirement: channels, selected-channel log, other-channel subwindow
and user list. Follow the two-column reference arrangement: main log and subwindow
on the left with the draft input between them; users above the channel tree on
the right.

This is a design direction, not a requirement for pixel-perfect reproduction.

## D005 — UI/protocol separation

**Status:** Accepted

IRC/networking code must not depend on GPUI. Application state mediates between protocol events and the interface.

## D006 — Async networking

**Status:** Accepted for the initial connection layer

Use a dedicated thread with a current-thread Tokio runtime for IRC work. GPUI polls bounded, owned events and sends commands through a bounded queue. This avoids running socket work on the UI thread and keeps the GPUI dependency out of `irc-core`. The integration passed a deterministic local IRC-server test; longer-running and cross-platform behavior still needs validation.

## D007 — IRC implementation dependency

**Status:** Accepted for the initial connection layer

Use `irc` 1.1.0 with its rustls and channel-list features, hidden inside `irc-core`. Its Tokio-based `Client`, 1.x API, TLS support, and MPL-2.0 license make it the lowest-effort fit for registration, channel join, message I/O, and roster retrieval. TLS certificate verification is enabled by default and can be disabled per server. The app receives owned events and never exposes `irc` types.

Install rustls's ring `CryptoProvider` before starting TLS. The full GUI unifies
`irc`'s aws-lc-rs feature with GPUI's HTTP client's ring feature; without an
explicit process default, `rustls::ClientConfig::builder()` panics before the TCP
connection or TLS handshake begins.

This choice was exercised with local server fixtures covering registration, auto-join, incoming `PRIVMSG`, NAMES, outgoing `PRIVMSG` and `NOTICE`, and SASL PLAIN negotiation. It does not yet establish public-network interoperability, reconnect, broader IRCv3 support, or long-term upstream maintenance. Reassess the dependency if those needs expose a real limitation. `vinezombie` remains an alternative for deeper IRCv3 work; an in-house stack would carry substantially more protocol maintenance.

Selection criteria:

- maintenance status
- TLS
- SASL
- IRCv3 support
- Tokio compatibility
- API stability
- license
- ability to hide the dependency behind our own boundary

### D007 investigation — 2026-09-24

This research preceded the initial adapter. Documentation inspection alone was
not treated as a connectivity, interoperability, security or load test.

| Option | Evidence and strengths | Risks / remaining validation |
| --- | --- | --- |
| `irc` 1.1.0 | Published 2025-03-24 (1.0.0: 2024-03-18). MPL-2.0. Tokio 1.x async streams; native-tls or rustls backends. Upstream claims RFC 2812 and IRCv3.1/3.2 coverage. `Sender` has CAP and SASL PLAIN/EXTERNAL helpers. | Most mature candidate here, with a 1.x API, but release age alone does not establish present maintenance responsiveness. Commit-history endpoints could not be retrieved in this investigation. SASL helpers are not proof of complete negotiation/retry handling; modern capabilities such as chathistory need individual review. Avoid adopting its config format, channel state or wire messages as application types. |
| `vinezombie` 0.3.1 | EUPL-1.2-only. Modular IRCv3 parsing, tags, labeled-response, client registration including SASL, optional Tokio and rustls transport helpers. | Upstream explicitly warns of further breaking 0.x releases. Current commit dates/release cadence could not be verified. A less established integration surface and additional licensing compatibility review before adoption. Keep its handler/string types inside an adapter. |
| Narrow in-house protocol/transport | Can define our own owned events and minimal parser/registration state machine; Tokio + rustls would fit the intended boundary. No third-party IRC API coupling. | We own framing, limits, encoding/casemapping, CAP/SASL sequencing, failure handling, reconnect and IRCv3 interoperability indefinitely. Transport/crypto should still use established libraries. Highest implementation and maintenance cost; no evidence yet that existing crates are insufficient. |

Further evaluation should cover CAP, SASL success/failure, message tags, unknown
commands, cancellation, backpressure, and public-server TLS interoperability.
Review actual recent commits/issues and license compatibility before distribution.

Primary sources inspected:

- [irc published versions](https://docs.rs/crate/irc/latest)
- [irc manifest and feature flags](https://github.com/aatxe/irc/blob/develop/Cargo.toml)
- [irc coverage and license](https://github.com/aatxe/irc)
- [irc Sender SASL/CAP API](https://docs.rs/irc/1.1.0/irc/client/struct.Sender.html)
- [vinezombie API/features](https://docs.rs/vinezombie/0.3.1/vinezombie/)
- [vinezombie scope, stability warning and license](https://github.com/vinezombie/vinezombie)

## D008 — Reproducible GPUI bootstrap

**Status:** Accepted for bootstrap; framework choice remains provisional (D002).

Pin `gpui` to 0.2.2 and retain Cargo.lock. The project now uses a local copy of
that published crate in `vendor/gpui` to patch Windows IME key-message handling;
see `vendor/gpui/PATCHES.md`. Use Rust edition 2024
and resolver 3. Enable `font-kit` for macOS glyph rendering and `runtime_shaders` for
Metal shader compilation at application startup. Keep unused default features off;
enable Wayland and X11 on Linux and the window manifest on Windows. This avoids
requiring the optional offline Metal compiler for this build; startup cost and release
packaging should be revisited before distribution.
No global toolchain override is installed. GPUI's pre-1.0 API is confined to `ui`.

The single-line input is adapted from the Apache-2.0 GPUI example with attribution
and license retained (see THIRD_PARTY_NOTICES.md). It handles native text input rather
than interpreting printable key events, preserving the path for IME and Unicode.

GPUI 0.2.2's macOS backend forwards raw keystrokes after AppKit resolves text command
selectors but discards the selectors, so user-defined text command mappings in
`~/Library/KeyBindings/DefaultKeyBinding.dict` do not reach this editor. The UI
binds common macOS and Windows text actions inside the focused input and reserves
Ctrl+Tab for conversation switching. Fully honoring user-customized macOS text
bindings remains a GPUI/platform integration limitation;
do not claim that the current shortcut mapping is a native text-system replacement.

See [Apple's text binding model](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/TextDefaultsBindings/TextDefaultsBindings.html)
and [the upstream GPUI selector issue](https://github.com/zed-industries/zed/issues/52550).

Sources: [published GPUI](https://docs.rs/crate/gpui/0.2.2),
[build script](https://docs.rs/crate/gpui/0.2.2/source/build.rs),
[macOS text backend selection](https://docs.rs/crate/gpui/0.2.2/source/src/platform/mac/platform.rs).

## D009 — Platform-specific navigation bindings

**Status:** Accepted

Keep channel/server navigation semantics in `app::Command` and bind keys only in
`ui`. Follow the supplied Chocoa-like macOS shortcuts. On Windows and Linux, use
Ctrl+Tab for unread channels and substitute non-system-reserved key combinations
for Option+Tab/Space and macOS Command navigation; preserve Ctrl+Left/Right for
draft word movement. Numbered channel shortcuts follow visible tree order across
servers, with `0` as the tenth item. Because Linux desktops commonly reserve
Ctrl+digit for workspaces, a Windows/Linux preference selects Ctrl, Alt or Super
for channel numbers (default Ctrl); server numbers remain Ctrl+Alt. Live messages and join/registration state drive
the unread and active sets; the offline mock retains deterministic fixture state.
An offline mock must not claim to have sent a message or NOTICE.

## D010 — Connection preferences and credentials

**Status:** Accepted

Keep the four-pane chat window open and show connection settings in a separate
window at startup and from the menu. Persist versioned preferences in `CayenChat/settings.json` under the
platform user configuration directory. Default to `irc.ircnet.ne.jp:6667`
without TLS, offer `irc6.ircnet.ne.jp`, and allow custom host/port and TLS.
Password saving is per server and off by default. Before enabling it, display a
confirmation that server and SASL passwords are stored as plaintext; switching
it off immediately removes those stored values. Require TLS whenever sending
either password; unverified TLS remains possible only after the user switches
off certificate verification for that server. Support SASL PLAIN through IRCv3
CAP negotiation in `irc-core`. The initial implementation covers one network
and manual reconnect.

Version 4 preferences store multiple server profiles and order user-added entries
before built-in choices. The active connection remains single-server. Versions 1–3
migrate on load with certificate verification enabled. Each profile owns its
wire character encoding. RFC 2812 specifies no IRC charset, and channel names are
protocol parameters whose encoded bytes identify the channel. Use the same selected
encoding for full IRC lines, including Japanese channel names, rather than assuming
UTF-8 channel names. The `irc` crate's optional encoding codec handles this; outgoing
lines are strictly checked before queueing because the codec itself uses replacement
for unrepresentable characters. Refuse a legacy-encoding session when the server
advertises `UTF8ONLY`, which forbids non-UTF-8 traffic.

Do not split encoded channel names on comma or colon on the client: ISO-2022-JP
characters can contain those bytes. A local test uses `#がが`, whose encoded name
contains comma bytes, and verifies one channel identity through JOIN and PRIVMSG.
Server handling of those bytes varies; a Japanese IRCnet server patch historically
addressed this, while an unpatched server may misinterpret the name. The current
per-server choice does not support mixed encodings within one connection or preserve
noncanonical incoming ISO-2022-JP byte variants.

Connection diagnostics report DNS, transport, and IRC registration stages plus
directional, decoded IRC command/reply lines through core events. The chat window
keeps a bounded in-memory transcript and copies it from the native menu. Known
credential commands are masked, while normal message bodies remain visible.
The terminal disconnect reason is also part of the copied transcript.
The library generates some maintenance traffic internally; PONG and configured
JOIN are included, but the transcript is not a complete byte-for-byte capture.
The trace is visible by default until registration succeeds and after failures;
the View menu controls the full transcript during an established session.
GPUI 0.2.2 has no rendered native menus on Windows/Linux, so Ctrl+, opens settings
and Ctrl+Shift+D/L controls the trace there.

References: [RFC 2812 character codes and channels](https://datatracker.ietf.org/doc/rfc2812/),
[IRCv3 UTF8ONLY](https://ircv3.net/specs/extensions/utf8-only),
[WIDE PROJECT Japanese IRC analysis, section 3.4](https://www.wide.ad.jp/About/report/pdf1999/part17.pdf).

## D011 — GPLv3 publication license and app icon

**Status:** Accepted

License the project's original source and user-provided `icon.png` under
GPL-3.0-only. Keep the adapted GPUI input source under Apache-2.0, and preserve
GPUI's Apache-2.0 and `irc`'s MPL-2.0 notices and license texts. GPLv2 was
requested initially but is incompatible with the Apache-2.0 code in this app;
the project owner chose GPLv3 instead. The macOS development and beta release
bundles include a generated `.icns`, and Windows embeds an `.ico` derived from
the same `icon.png`.

Sources: [Apache Software Foundation compatibility guidance](https://www.apache.org/licenses/GPL-compatibility.html),
[GPLv3 text](https://www.gnu.org/licenses/gpl-3.0.txt), and
[MPL 2.0](https://www.mozilla.org/en-US/MPL/2.0/).

## D012 — Appearance, selectable channel log, and Windows IME scope

**Status:** Accepted

Keep connection and appearance preferences in one versioned settings file, but
present them in separate tabs. Version 5 adds colors for the app and both logs,
optional alternating message-row colors, and font families for the two logs,
member list, channel tree, input, and timestamp. Timestamp defaults to a
platform-specific monospaced face. Load version 4 without altering connection
settings. Limit URL opening and drag text selection to the upper channel log so
the lower combined log keeps its one-click channel navigation. Only HTTP(S)
URLs open, on double-click; log selection copies message-body text.

GPUI 0.2.2's Windows input handling predates upstream's merged November 2025
Japanese/Korean keyboard and IME corrections. Guard Send and nickname completion
while this app's text input has marked composition. The local GPUI copy now
translates unrecognized keydown messages, including language keys, and does not
unwrap `VK_PROCESSKEY` into app shortcuts. This targets the reported MS-IME
half-width/full-width toggle failure without changing app bindings. The upstream
fix is broader, so Windows hardware validation remains required. Do not claim
Windows IME is fixed from a macOS build.

Source: [upstream Windows input/IME fix](https://github.com/zed-industries/zed/pull/41259).
