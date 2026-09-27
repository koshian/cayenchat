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

Use a dedicated thread with a current-thread Tokio runtime for IRC work. GPUI awaits bounded, owned events (no timer polling) and sends commands through a bounded queue. This avoids running socket work on the UI thread and keeps the GPUI dependency out of `irc-core`. The integration passed a deterministic local IRC-server test; longer-running and cross-platform behavior still needs validation.

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
that published crate in `vendor/gpui` to patch Windows IME key-message handling
and Linux issues, including enabling cosmic-text's bounded per-word shaping
cache for faster channel switching; see `vendor/gpui/PATCHES.md`. Use Rust edition 2024
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
platform user configuration directory. Offer `irc.ircnet.ne.jp:6667` without
TLS and `irc6.ircnet.ne.jp` as suggestions, and allow custom host/port and TLS
(since D017 the server list starts empty).
Password saving is per server and off by default; switching it off
immediately removes those stored values. (Where passwords are stored is now
D014; the original plaintext-in-settings storage is superseded.) Require TLS whenever sending
either password; unverified TLS remains possible only after the user switches
off certificate verification for that server. Support SASL PLAIN through IRCv3
CAP negotiation in `irc-core`. The initial implementation covered one network
and manual reconnect; multi-server support is D017.

Version 4 preferences store multiple server profiles and order user-added entries
before built-in choices. Versions 1–3
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
the lower combined log keeps its channel navigation. Only HTTP(S)
URLs open, on double-click; log selection copies message-body text.
Combined-log channel navigation also requires a double-click, because a single
click moved channels too easily while reading.

GPUI 0.2.2's Windows input handling predates upstream's merged November 2025
Japanese/Korean keyboard and IME corrections. Guard Send and nickname completion
while this app's text input has marked composition. The local GPUI copy now
translates unrecognized keydown messages, including language keys, and does not
unwrap `VK_PROCESSKEY` into app shortcuts. This targets the reported MS-IME
half-width/full-width toggle failure without changing app bindings. The upstream
fix is broader, so Windows hardware validation remains required. Do not claim
Windows IME is fixed from a macOS build.

Source: [upstream Windows input/IME fix](https://github.com/zed-industries/zed/pull/41259).

## D013 — Keyboard settings tab and GTK Emacs key theme

**Status:** Accepted

GPUI draws its own text input, so GTK's `gtk-key-theme=Emacs` never reaches the
draft on Linux. Key preferences move to a separate Keyboard settings tab, shown
on Windows and Linux only because macOS has no choices there: the channel-number
modifier (D009) and, on Linux, draft editing keys with Follow GTK (default),
Standard and Emacs. Follow GTK reads `gtk-key-theme` from the XDG desktop
portal's `org.gnome.desktop.interface` namespace and rebinds live when it
changes; where the portal lacks the key (for example non-GNOME backends) it
falls back to `gtk-key-theme-name` in `$XDG_CONFIG_HOME/gtk-3.0/settings.ini`.
GTK 4 dropped key themes, so only the GTK 3 setting is consulted.

Emacs mode mirrors GTK 3's `gtk-keys.css.emacs` for GtkEntry: Ctrl+B/F/A/E
(with Shift to select), Alt+B/F (with Shift), Ctrl+D/H, Alt+D, Ctrl+K, Ctrl+U
(whole line), Ctrl+W cut, Ctrl+Y paste, Alt+\\ delete surrounding whitespace
and Alt+Space collapse it to one space. As in GTK, Select All moves to Ctrl+/
and Ctrl+Y no longer redoes (Ctrl+Shift+Z still does). Ctrl+W/Y use the system
clipboard, not an Emacs kill ring. Alt chords do not reveal the in-window menu
bar, which only reacts to Alt pressed alone.

Source: [GTK 3 Emacs key theme](https://gitlab.gnome.org/GNOME/gtk/-/blob/gtk-3-24/gtk/gtk-keys.css.emacs).

## D014 — Credential storage

**Status:** Accepted

All secrets (IRC `PASS`, SASL passwords, image uploader tokens and future
credentials) go through one `storage::credentials::CredentialStore`. UI, IRC
and provider code never store secrets themselves, and the preferences file
never contains them.

Backends: the OS store through `keyring` 4.2 (`v1` feature: Keychain on macOS,
Credential Manager on Windows, freedesktop Secret Service via the pure-Rust
zbus client on Linux/BSD, reusing the zbus already in GPUI's tree; it resolves
under Rust 1.88), and an explicit local file. `keyring` is maintained and
covers all three targets; no separate crate was needed.

The local file (`$XDG_CONFIG_HOME/cayenchat/credentials.json`, falling back to
`~/.config/cayenchat/`, on Linux; the CayenChat config directory elsewhere) is
unencrypted. Encrypting it with a key kept on the same disk would give no real
protection, and a user passphrase would add an unlock step for little gain for
an IRC client; the UI states the trade-off instead. It is `0600` in a `0700`
directory, written via a fresh `0600` temporary file and rename, and its
permissions are tightened before reading if found looser. It exists because
Linux desktops without a Secret Service must keep working.

Rules: the backend is an explicit setting (default System); nothing falls back
silently; choosing the local file needs confirmation; switching migrates known
secrets. The one automatic move is migrating version ≤10 plaintext passwords
out of `settings.json`: into the chosen store, or into the local file only if
the system store is unavailable, since those passwords were already plaintext
with the user's consent. Keys use stable internal IDs, never nicknames,
hostnames or display names. `Secret`, `ConnectionConfig` and `SaslCredentials`
redact their `Debug` output; credential errors carry sanitized text only.

On macOS the system backend keeps every secret in one Keychain item and every
backend value is cached for the process (2026-09-27). Separate items made the
legacy Keychain ask for the login password once per secret on each connect and
again on opening settings, since ad hoc signed builds drop off each item's
access list on every update. The data protection Keychain would avoid the
prompts but needs a keychain access group entitlement, which requires a
Developer ID signature. One item costs one prompt per launch and rewrites the
whole map on change, which is negligible for a handful of passwords.

Sources: [keyring 4.2 crate](https://crates.io/crates/keyring),
[keyring-rs wiki](https://github.com/open-source-cooperative/keyring-rs/wiki/Keyring),
[XDG Base Directory specification](https://specifications.freedesktop.org/basedir-spec/latest/),
[freedesktop Secret Service API](https://specifications.freedesktop.org/secret-service-spec/latest/).

## D015 — IRC image sharing through an external uploader

**Status:** Accepted

IRC carries only text, so images are uploaded to a hosting account the user
owns and the link is inserted into the draft; the user sends it. Uploading is
disabled until a provider is chosen and an account connected, always asks
before an image leaves the computer, and never sends the IRC message.
Anonymous public upload services are not offered.

Boundaries: `model::Attachment` is protocol-neutral; `app::attachments` is the
GPUI-free flow shared by paste and drop; `upload::ExternalUploader` is the IRC
transport only. The UI refers to providers by registry ID. A future Matrix
client must upload through Matrix's native media API and emit image events; it
must not implement `ExternalUploader` or route through `upload`. There is no
universal "uploader" abstraction.

First provider: **ImgBB** (2026-09-26). `POST https://api.imgbb.com/1/upload`
takes `key` (the account's API key, shown after signing in at
api.imgbb.com) and `image` (up to 32 MB; imgbb.com's configuration states
32,000,000 bytes and lists HEIC/HEIF/AVIF/WebP among accepted types). The JSON
reply carries `data.url`, the direct image link. An invalid key is answered
with HTTP 400 and error code 100 (observed with a dummy key), mapped to the
reconnect prompt. The key is a per-account credential the user pastes; no
application registration or client secret exists, and none is shipped. The
docs show the key in the query string; CayenChat sends it in the multipart
body so it never appears in URLs. The optional `expiration` is not set.
Whether a real key is accepted in the body was not verified with a real
account (only the invalid-key response); if it is not, move it to the query.

Rejected first providers:

- Gyazo (implemented first, then replaced): its documented upload API uses a
  per-user access token, but gyazo.com and upload.gyazo.com were in
  maintenance (HTTP 502) throughout this work.
- Imgur: authenticated uploads need a registered client (`client_id` and
  `client_secret`); `api.imgur.com/oauth2/addclient` now redirects to the home
  page (new registrations have been unavailable since about December 2025),
  access tokens expire after a month and refreshing needs the client secret.

Adding a provider: a module in `crates/upload` using the shared `http`
helpers, an entry in `providers()` (ID, name, setup URL, size limit) and
`connect()`, and an `image_setup_<id>` locale string. HTTP uses `ureq` 3
(blocking, rustls/ring, no redirects, 64 KiB reply limit) on GPUI's background
executor.

Attachments are recognized by content (PNG, JPEG, GIF, WebP, BMP, TIFF, and
HEIC/HEIF/AVIF via ISO base media `ftyp` brands). On macOS the vendored GPUI
accepts file-promise drags (Photos, Mail, screenshot thumbnail), which
upstream refused; see `vendor/gpui/PATCHES.md`.

Sources: [ImgBB API](https://api.imgbb.com/),
[Gyazo API overview](https://gyazo.com/api/docs),
[Imgur API documentation](https://apidocs.imgur.com/),
[Tautulli issue on Imgur registration](https://github.com/Tautulli/Tautulli/issues/2620),
[NSFilePromiseReceiver](https://developer.apple.com/documentation/appkit/nsfilepromisereceiver).

## D016 — OS desktop notifications through notify-rust

**Status:** Accepted

Notifications use the operating system's service, not in-app popups. Linux
and BSD call `org.freedesktop.Notifications` on the session bus, which every
major desktop implements; the XDG portal notification API is not used.
`notify-rust` 4.18 provides this over zbus 5, which GPUI and keyring already
bring in, and also covers macOS (`mac-notification-sys`, reusing objc2) and
Windows (`tauri-winrt-notification`, reusing `windows` 0.61), so no
per-platform code is kept here. The Linux body is escaped because servers with
`body-markup` parse it.

macOS: `mac-notification-sys` uses the deprecated but working
`NSUserNotificationCenter`. Without an explicit application it asks
AppleScript for an app named "use_default" and falls back to Finder, so the UI
registers the main bundle identifier and disables notifications when there is
none (`cargo run`); use `scripts/bundle-macos.sh`. notify-rust's
`UNUserNotificationCenter` backend is still a preview feature.

Windows: toasts need an AppUserModelID. notify-rust defaults to PowerShell's,
so toasts are attributed to Windows PowerShell until CayenChat registers its
own ID (a Start menu shortcut or the per-user `AppUserModelId` registry key);
that is follow-up work, as is activating the conversation on click.

Sources: [Desktop Notifications Specification](https://specifications.freedesktop.org/notification-spec/latest/),
[notify-rust](https://github.com/hoodie/notify-rust),
[Windows toast notifications from desktop apps](https://learn.microsoft.com/en-us/windows/apps/design/shell/tiles-and-notifications/send-local-toast-other-apps).

## D017 — Multiple servers at once

**Status:** Accepted

Every server profile is a network in the channel tree, shown whether or not it
is connected, and any number of them can be connected at the same time
(LimeChat's model). Settings version 13 moves the nickname, `USER` username,
auto-join channels, SASL account and startup connection into each profile, so
each network has its own identity. Versions 1–12 give their single
identity (nickname, username, channels, SASL account and SASL, startup
connection) only to the server it was used with: the previously selected
server, or the only server when the selection is gone. Other servers keep
their connection details, password-saving choice and stored credentials
(keyed by profile ID) but start without a nickname, username, channels or
account, to be entered for them. SASL stays on only with TLS.

2026-09-27: until then migration copied the shared identity into every
profile, so every server auto-joined the same channels and used the same
account; that was the reported "shared auto-join channels" (the settings
window itself kept values per server). Files already migrated (version 13
and later) are left as they are: identical values on several servers may
be intended, so they are not cleared or second-guessed. New servers always
start blank; a preset fills only its host.

The IRCnet servers are not stored by default: they are suggestions offered
when adding a server (`storage::PRESETS`), because unused built-in servers in
the tree got in the way once every profile became a network. The list starts
empty and every profile can be edited and removed. Migration from 1–12 keeps
an IRCnet profile only if it was selected or has saved passwords (keeping its
ID so the passwords stay attached) and drops the others.

Each connection keeps the existing design (D006): its own worker thread and
current-thread Tokio runtime, bounded queues, and an awaited event stream.
The UI adds no timers per connection. Connection state lives in one
`ServerSession` per server rather than in the chat window. Per-server bounds
(logs, conversations, transcript, WHOIS) are unchanged; application-wide
bounds are deliberately left for a separate change after measuring (see
`performance.md`).

## D018 — Inline image previews

**Status:** Accepted

An ordinary Appearance setting (settings version 14, off by default for new
and existing users) shows a small static thumbnail below main-log channel
messages that contain a direct image link. Avatars will be a separate
ordinary setting; IRCv3 draft features get their own experimental opt-ins.

Candidates: the first link of a message (found by the log's existing URL
recognition) that is `http`/`https`, has no credentials, uses the default
port, names a public host (no `localhost`, single-label names or non-public
IP literals) and whose last path segment ends in `.png`, `.jpg`, `.jpeg`,
`.gif` or `.webp`. ImgBB's `https://i.ibb.co/<id>/<name>.<ext>` links
qualify; ImgBB's `ibb.co/<id>` pages do not. Web pages are never fetched to
discover images: no HTML, Open Graph, oEmbed or provider-specific scraping.

Loading treats URLs and bytes as untrusted: GET only, at most 3 redirects
followed by hand with each target re-checked and no HTTPS→HTTP downgrade, a
resolver that drops loopback, private, shared, link-local, multicast,
documentation and reserved addresses (IPv4 and IPv6, including mapped and
NAT64/6to4 forms) before connecting, so names and redirects cannot reach
local services either; no proxy, cookies, `Referer`, credentials or
compression; 5 s to connect and 15 s per preview in total. The response must
be 200 with an `image/*` type (not SVG), at most 8 MiB, and the content must
sniff as PNG, JPEG, GIF or WebP; the extension is only a hint. Dimensions are
read from the header first (at most 8192 px a side and 16.7 megapixels), the
decoder may allocate at most 48 MiB, one decode runs at a time in the whole
process, and the full image is dropped as soon as the thumbnail (at most
400×200 px, shown at up to 200×100 logical px) exists. GIF and animated WebP
show their first frame. There is no disk cache. Anything unsupported or
failed leaves the ordinary text link.

Implementation: a GPUI-free `cayenchat-media` crate owns recognition,
loading, decoding and the application-wide `PreviewCache`; the UI runs loads
on GPUI's background executor and draws `RenderImage`s. It uses `ureq` 3
(already used by `upload`) with its resolver hook, and the `image` crate
GPUI already depends on (PNG/JPEG/GIF/WebP decoders only); no new crates.
GPUI's own `img()` URL loading was not used: it has no address, redirect,
size or pixel limits and caches without a byte budget. GPUI 0.2.2's sprite
atlases never reused the space of a removed image; the vendored copy now
frees it (`vendor/gpui/PATCHES.md`), since evicted thumbnails are removed
from the atlas.

Matrix, if added, supplies its own media references (`mxc://` and
server-made thumbnails) and fetcher with its own authentication to the same
decode, cache and display layer; nothing Matrix-specific exists now, and
`MediaRef` is non-exhaustive for that reason.

Tests and measurements never contact real hosts: the HTTP loader is tested
against a local fixture server through a test-only constructor that also
allows 127.0.0.1, and the `preview-fixture` cargo feature (never used for
release builds) swaps in a fetcher that reads `images.cayenchat.test` links
from a local directory for GUI checks and `scripts/perf`.

## D019 — Native settings appearance without a GPUI migration

2026-09-27. Keep vendored GPUI 0.2.2 and its platform fixes. Pin
`native-theme-gpui = 0.5.7`, `native-theme = 0.5.7`,
`native-theme-derive = 0.5.7`, and
`gpui-component = 0.5.1`. Connector 0.5.8 and 0.5.9 use `gpui-pre` and
are incompatible with this application's GPUI types. Core 0.5.9 also changed
APIs used by connector 0.5.7, so pinning the connector alone is insufficient.
The transitive derive crate must also be pinned: 0.5.9 compiled with core
0.5.7 but made platform presets fail resolution in regression tests.
The UI now requires Rust 1.94; other workspace crates keep their existing floor.
Disable icon bundles/system-icon loading. Enable only `svg-rasterize`, because
0.5.7's icon module references that module even when default features are off.

`ui::settings_theme` calls `SystemTheme::from_system()` and converts both
variants with `native_theme_gpui::to_theme()`. The connector supplies settings
palette colors; raw `ResolvedTheme` supplies the per-widget geometry, fonts,
input colors, and checkbox states that its flat GPUI theme cannot represent.
Apply this to the existing settings controls, retaining IDs, callbacks,
secret handling, text editing and IME composition. Native OS widget embedding,
a full gpui-component widget migration, keyboard focus for buttons and
checkboxes, and accessibility semantics are outside this change.

Cache both variants outside rendering. Read on opening settings and after
GPUI reports an appearance change; the saved Light/Dark choice overrides the
OS mode. AppKit reads run on the macOS main thread; Windows and Linux reads
run on GPUI's background executor. There is no polling or additional watcher.
An accent/font-only change is refreshed when settings are reopened. A failed
read restores the existing app palette. Chat pane colors/fonts are independent.

macOS reads AppKit colors/fonts and combines them with the Sonoma preset.
Windows uses system colors/fonts and the Windows 11 preset; Linux reads KDE
configuration or the desktop portal, with platform preset fallback. These
are approximations rendered by GPUI, not native AppKit/WinUI/GTK widgets.
Only fields consumed by this adapter affect controls; high-contrast mode and
assistive-technology behavior need separate native-desktop validation.

Sources: [connector 0.5.7 API](https://docs.rs/native-theme-gpui/0.5.7/native_theme_gpui/),
[connector source](https://docs.rs/crate/native-theme-gpui/0.5.7/source/),
[upstream](https://github.com/tiborgats/native-theme). Version compatibility was
also checked against the downloaded crates' manifests and source, including
0.5.8 and 0.5.9 (the website index lagged the registry).

## D020 — Standard Tab traversal in settings

2026-09-27. Settings must not capture keys for chat features. Scope the chat
draft bindings (Tab nickname completion, Enter send, Ctrl+Enter NOTICE) to
`ChatWindow > TextInput`; they formerly matched every `TextInput`, so Tab in a
settings field dispatched a completion nobody handled and focus never moved.
In `SettingsWindow`, Tab/Shift+Tab call GPUI's `focus_next`/`focus_prev`.
`TextInput::new_settings_field` makes its focus handle a GPUI tab stop, so
order follows paint order (top to bottom) and wraps, with no manual indices.
Only text fields are tab stops; buttons, checkboxes and selectors still have
no keyboard focus (see D019).

## D021 — Settings save as they change

**Status:** Accepted

The settings window has no Save buttons. An edit is written to
`settings.json` 500 ms after the last change (text fields, choices and
toggles alike) and applied to the chat window: appearance, language and
shortcuts only when they changed, the server list only when servers changed.
Nothing reconnects; **Connect** stays as the one explicit action.

Exceptions keep a half-finished edit from doing damage:

- A typed password is stored when its field loses focus, not while it is
  being typed, because storing empties the field. Closing the window (close
  button or Back), quitting the app and choosing another server store every
  typed password, focused or not; a server switch stores them under the
  server they were typed for (not yet for a server without a host).
- While the selected server has no host nothing is saved and the window says
  a host is needed, because saving would drop the server and forget its
  passwords.
- Invalid values (a bad port or color) are reported and not saved until fixed.
- Removing a server that was already saved asks for confirmation, since the
  removal is saved at once and disconnects it and deletes its passwords.

## D022 — Opt-in IRCv3 features and shared CAP negotiation

**Status:** Accepted

2026-09-27. Newly introduced IRCv3 features require an explicit opt-in per
server and are off for new and migrated settings (settings version 15). The
IRCv3 settings tab is their current home and names the server being
configured. This milestone exposes message tags and server timestamps
(server-time); server-time does not require message-tags, as the protocol
allows. Changes are saved by the existing autosave (D021) and take effect on
the next connection, including reconnects and retries; nothing reconnects.

CAP negotiation is shared infrastructure, not a user-facing feature: SASL
(D010) keeps working with every option off, and with no SASL and no option
the registration traffic is unchanged. Negotiation, tag handling and the
server-time representation are described in `architecture.md` (IRCv3
capabilities and message tags).

The `irc` 1.1.0 dependency (D007) is kept. Audit of the pinned source
(irc-proto 1.1.0 `message.rs`, `line.rs`, `command.rs`, `caps.rs`; irc 1.1.0
`client/mod.rs`): tags are parsed and unescaped as the specification asks,
but duplicates are all kept and `key`/`key=` differ, so `irc-core` normalizes
lookups; the library neither handles nor requests capabilities by itself;
its CAP parser guesses the subcommand position (a nickname such as `new`
confuses it), so replies are read back positionally; the line codec has no
length limit and replaces undecodable bytes with U+FFFD for the whole line,
tags included. The last point is why `message-tags` is not requested on
legacy-encoding connections; a proper fix needs a byte-level transport that
splits tags before decoding, which is out of scope here.

An old server-time never marks a line as replayed history. PR #18 had
treated server-time at least five minutes old as history (no notification or
highlight); that rule was removed when this change merged it, because it
would hide live lines when the local clock runs ahead and because a
timestamp alone does not say a line was replayed. History is still
recognized by the missing user mask and by `chathistory`/`znc.in/playback`
batches, whose references use the normalized tag reader. Bouncer backlog
without a history batch can notify like live traffic.

The `batch` capability was added later as a third opt-in (same object, no
settings version change, off when absent). It extends `ReplayTracker`
instead of adding a second tracker, requests only `batch` (never
`draft/chathistory`, event-playback or multiline, and no CHATHISTORY
requests), and keeps the rules above: only the two history types and their
descendants are history, unknown types never mute, and a timestamp still
proves nothing. Unsolicited batches on connections with the option off keep
the handling they had before (history recognized, framing lines shown);
that is compatibility, not negotiated support. Details and bounds are in
`architecture.md`.

Not included: chathistory requests, echo-message, labeled-response, account
tags, avatars, metadata, reactions, typing indicators and Matrix. Later
features may become default-on or move tabs by changing their preference
default and settings row only; that migration is not implemented.

Sources: [capability negotiation](https://ircv3.net/specs/extensions/capability-negotiation),
[message tags](https://ircv3.net/specs/extensions/message-tags),
[server-time](https://ircv3.net/specs/extensions/server-time),
[batch](https://ircv3.net/specs/extensions/batch),
[chathistory batch type](https://ircv3.net/specs/batches/chathistory),
[SASL 3.1](https://ircv3.net/specs/extensions/sasl-3.1),
[SASL 3.2](https://ircv3.net/specs/extensions/sasl-3.2).
