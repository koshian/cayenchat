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
the right. The bottom edge of the draft input is a drag handle that moves the
split between the two logs (10%–90%, a double click restores an even split);
the split is kept for the run only, not saved.

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
cache for faster channel switching; see `vendor/gpui/PATCHES.md`. A local
`vendor/zed-xim` likewise replaces the XIM client's panicking COMPOUND_TEXT
decoding (IBus/Mozc on X11), and `vendor/irc-proto` limits the received line
length. `vendor/irc` drops a channel's user list when we are kicked (#342)
until `Client` goes away (#266). Every vendored copy is temporary; `vendor/README.md` says when to
switch back to an upstream release. Use Rust edition 2024
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
draft word movement. Numbered channel shortcuts follow visible tree order within
the selected server (the server of the selected channel or server row; #580),
counted from 1 for each server, with `0` as the tenth item. A number with no
conversation, or no selected server, does nothing and never reaches another
server. Because Linux desktops commonly reserve
Ctrl+digit for workspaces, a Windows/Linux preference selects Ctrl, Alt or Super
for channel numbers (default Ctrl); server numbers remain Ctrl+Alt. Live messages and join/registration state drive
the unread and active sets; the offline mock retains deterministic fixture state.
An offline mock must not claim to have sent a message or NOTICE.

## D010 — Connection preferences and credentials

**Status:** Accepted

Keep the four-pane chat window open and show connection settings in a separate
window at startup and from the menu. Persist versioned preferences in `CayenChat/settings.json` under the
platform user configuration directory. Offer `irc.ircnet.ne.jp:6667` without
TLS, `irc6.ircnet.ne.jp` and `dev.ircnet.ne.jp` as suggestions, and allow custom host/port and TLS
(since D017 the server list starts empty).
Password saving is per server and off by default; switching it off
immediately removes those stored values. (Where passwords are stored is now
D014; the original plaintext-in-settings storage is superseded.) Require TLS whenever sending
either password; unverified TLS remains possible only after the user switches
off certificate verification for that server. (Since D024 a server password may
go without TLS after an explicit per-server opt-in; SASL still requires TLS.) Support SASL PLAIN through IRCv3
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
per-server choice does not preserve noncanonical incoming ISO-2022-JP byte
variants. Mixed encodings within one connection are supported only for the text
of channel messages (D050).

Connection diagnostics report DNS, transport, and IRC registration stages plus
directional, decoded IRC command/reply lines through core events. The chat window
keeps a bounded in-memory transcript and copies it from the native menu. Known
credential commands are masked, while normal message bodies remain visible.
The terminal disconnect reason is also part of the copied transcript.
The library generates some maintenance traffic internally; PONG and configured
JOIN are included, but the transcript is not a complete byte-for-byte capture.
The trace is visible by default until registration succeeds and after failures;
the View menu controls the full transcript during an established session.
IRC lines of an established session are recorded only while the transcript is
on, so the per-message path does no transcript work otherwise.
GPUI 0.2.2 has no rendered native menus on Windows/Linux, so Ctrl+, opens settings
and Ctrl+Shift+D/L controls the trace there.

References: [RFC 2812 character codes and channels](https://datatracker.ietf.org/doc/rfc2812/),
[IRCv3 UTF8ONLY](https://ircv3.net/specs/extensions/utf8-only),
[WIDE PROJECT Japanese IRC analysis, section 3.4](https://www.wide.ad.jp/About/report/pdf1999/part17.pdf).

## D011 — GPLv3 publication license and app icon

**Status:** Accepted

License the project's original source and user-provided app icon artwork
(`assets/icons/cayenchat.svg`, which replaced the original `icon.png`) under
GPL-3.0-only. Keep the adapted GPUI input source under Apache-2.0, and preserve
GPUI's Apache-2.0 and `irc`'s MPL-2.0 notices and license texts. GPLv2 was
requested initially but is incompatible with the Apache-2.0 code in this app;
the project owner chose GPLv3 instead. The macOS development and beta release
bundles include a generated `.icns`, and Windows embeds an `.ico`, both rendered
from that SVG. The Debian package installs the SVG itself as the hicolor
scalable icon.

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
draft on Linux. Key preferences move to a separate Keyboard settings tab: the
channel-number modifier (D009; Windows and Linux only) and, on Linux, draft
editing keys with Follow GTK (default), Standard and Emacs. Since #591 the tab
is shown on every OS and the shortcut list (formerly the Shortcuts tab, #118)
follows these options, so on macOS the tab holds the list alone. Follow GTK reads `gtk-key-theme` from the XDG desktop
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

The UI reads the store only from a GPUI global that startup installs before
any window opens; a missing global panics instead of opening the system store.
Tests install an in-memory store, and while it is installed, switching
backends opens another in-memory store and the system store probe succeeds
without asking the operating system (2026-10-02, #115).

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

2026-10-10 (#290): native-theme 0.5.7 is a patched copy in
`vendor/native-theme`. 0.5.7 read Windows fonts and metrics at the system DPI,
so its "logical" sizes included the display scale and GPUI scaled them again
(the settings window's text was 1.5 times too large at 150 %). The copy reads
them at 96 DPI, as 0.6.1 does; 0.6.1 itself needs the `gpui-pre` connector.
Switch back when the GPUI question in #265 allows native-theme 0.6.1 or later.

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
draft bindings (Tab nickname completion, Enter send, Ctrl+Enter; the latter
was NOTICE until D067 made the send key a setting) to
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

- (Superseded for server settings by #209 below: typed passwords are stored
  by Save only.) A typed password was stored when its field lost focus,
  closing the window or choosing another server.
- While the selected server has no host nothing is saved and the window says
  a host is needed, because saving would drop the server and forget its
  passwords.
- Invalid values (a bad port or color) are reported and not saved until fixed.
- Removing a server that was already saved asks for confirmation, since the
  removal is saved at once and disconnects it and deletes its passwords.

**Another process (#147).** The window holds a copy of the settings and
writes all of it, so a second process (an installed build beside a Test Build
without `isolated`, or a hand edit) could be overwritten with the copy's older
values. Only the window in front takes edits, so: when the window loses
activation it saves what is pending, and when it is activated again it reads
the file and, if that differs from what this window last saved, rebuilds the
form from it (the shown tab and server stay; nothing is read while a save is
pending or failing, so unsaved edits are not dropped). Passwords are deleted
only for servers this window itself removed, i.e. missing from what it last
saved, never for servers that only the file has. What was read back is also
applied to the chat window (theme, language, shortcuts, servers, image host,
notifications) exactly as a save would, because later saves compare with the
read-back copy and would otherwise find nothing to apply. File watching and
single-instance locks were not added.

**Server settings wait for Save (#209).** The exception to the above: the
server list and everything stored in a server profile (hostname, port,
nickname, channels, SASL, IRCv3 options, password saving, removal) are written
only when **Save** on the Connection tab is pressed (Connect also saves), so
an accidentally chosen server is never kept. Everything else keeps autosaving
and leaves the saved server list and the selected server as it is (an invalid
unsaved port does not stop it). Typed passwords are kept in memory per server
and stored by Save too, not when their field loses focus or another server is
chosen; turning password saving off or removing a server deletes the stored
secrets only on Save. Save stays available after the last server was removed,
and the IRCv3 tab shows it with the note whenever server changes are
unsaved. A note says server changes are unsaved;
leaving the window with them discards them, and a file written by another
process is not read back while they are pending. Adding a server shows only
the suggestions (`PRESETS`: `irc.ircnet.com` first, the default, then every
server on <https://www.ircnet.info/servers>), not the saved servers; choosing
one fills host and port 6667 of the new, still unsaved server, and "Cancel
adding" drops it. Editing lists only saved servers plus "Add a server…".
Removal is confirmed first and, like other server edits, applies on Save.
A failed save (settings file, or deleting a stored secret) leaves the edits
unsaved, for Connect too, so a later Save retries and nothing unsaved reaches
the autosave.

## D022 — Opt-in IRCv3 features and shared CAP negotiation

**Status:** Accepted

2026-09-27. Newly introduced IRCv3 features require an explicit opt-in per
server and are off for new and migrated settings (settings version 15). The
IRCv3 settings tab is their current home and names the server being
configured. This milestone exposes message tags and server timestamps
(server-time); server-time does not require message-tags, as the protocol
allows. Per-server changes are saved with Save (D021) and take effect on
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
tags, reactions, typing indicators and Matrix. Avatars through metadata
came later (D023). Later
features may become default-on or move tabs by changing their preference
default and settings row only; that migration is not implemented.

Sources: [capability negotiation](https://ircv3.net/specs/extensions/capability-negotiation),
[message tags](https://ircv3.net/specs/extensions/message-tags),
[server-time](https://ircv3.net/specs/extensions/server-time),
[batch](https://ircv3.net/specs/extensions/batch),
[chathistory batch type](https://ircv3.net/specs/batches/chathistory),
[SASL 3.1](https://ircv3.net/specs/extensions/sasl-3.1),
[SASL 3.2](https://ircv3.net/specs/extensions/sasl-3.2).

## D023 — User avatars and experimental IRCv3 metadata

**Status:** Accepted (IRC metadata: experimental)

2026-09-27; revised 2026-09-28 to a single switch.

- **Connection settings → Show user avatars** (`ServerProfile::user_avatars`,
  off by default, applies on Save without reconnecting, independent of
  image previews). Revised 2026-10-15: it moved from the application-wide
  `Appearance::user_avatars` to each server, with no shared setting, no
  "follow the common setting" state and no default for new servers. It alone
  decides, per server, whether that server's rows display avatars and
  whether their images are downloaded. The shared image cache and fetcher
  live while any server has it on and are dropped when all are off; turning
  one server off only stops its rows from asking, and loads already started
  may finish because the cache is shared by URL. Off keeps the
  compact layout exactly: no avatar column, no placeholder, no taller rows,
  no lookups, fetches or decodes. With it on, users without an avatar (or
  whose image failed) get a client-drawn default avatar (2026-09-28),
  ported from CayenChat's `defaultAvatar.js`: FNV-1a of the nickname's
  first four UTF-16 code units (as shown, case included) picks one of 8
  Okabe–Ito backgrounds, a figure color from the opposite light/dark set
  (contrast ≥ 4:1), one of 4 calyx hats and 4 eyes, 512 looks in all. It is
  an SVG rasterized by GPUI; it says nothing about identity (a later user of
  the nickname looks the same) and works for any protocol.
- **IRC avatar metadata** has no switch (the first version had a per-server
  "IRCv3 → User avatars (experimental)" option; it confused users, who
  turned it on and saw nothing while the display setting was off, and was
  removed): every connection asks a server that offers `draft/metadata-2`
  for it, and for `batch` as its prerequisite even when the server's batch
  option is off (a server that does not offer metadata gets no batch
  request from this). Receiving references downloads no image; the IRCv3
  tab only holds our own avatar (and, since D025, the separate peer
  avatar option). Files that still carry the old
  `ircv3.metadata` field load without it.
- **Occupancy applies to metadata only** (D058): avatars from CTCP AVATAR
  (D025) are tied to a user mask and the name, not to occupancies.
- **Quick icon setup on the Connection tab** (#593, 2026-10-24). Directly
  under the server selector, the "Avatar settings" disclosure row (state "No icon set" / "Icon set", action "Set up" / "Change", a chevron) unfolds a section that reuses
  the IRCv3 tab's URL draft (`SettingsWindow::settings.avatar_url`), image
  chooser, square selection and upload, so both tabs always show the same
  value. The row is only a fold (#602; an unsaved view state, no stored
  setting): unfolding or folding saves and sends nothing, and folding does
  not remove or stop publishing anything. Folded, one line says whether an
  icon is registered. The section has no "publish" checkbox; **Register /
  Update** (and finishing an upload started in the section) is the explicit
  act, and a notice before it says the URL goes to other users on IRC and
  an uploaded image to the external image host. It writes the existing
  fields only: `avatar_url`, `user_avatars = true` (of that server only; a
  later switch-off is respected until the next registration),
  `ircv3.peer_avatars = true` and `peer_avatar_url`, and tries the
  Metadata publish on the current connection. Editing the field publishes
  nothing. The footer keeps the ways apart: Metadata (published only when
  the server confirmed that URL; "not connected", "unsupported" and
  failures are shown as such) and CTCP AVATAR (needs Save; a reconnect is
  asked for, never done; "sharing" only when the connection answers with
  that URL, otherwise it says the old URL is still answered). The chooser is offered without an image host; its
  setup and re-authentication guidance (the same as for chat attachments)
  gets a button to the Image Upload tab, which has a way back to the same
  server's section, and nothing is uploaded automatically. The Reiwa mode
  checkbox under the section (outside the fold) edits the app-wide
  `appearance.reiwa_mode` shared with the Application tab, and is never set
  by registering.

`user_avatars` was added without a settings version change (like batch),
read as off when absent. On load, a file that still has `appearance.user_avatars`
copies it into every server that has no `user_avatars` of its own and drops
it, so the migration runs once and the old key is never written back; a
missing old value reads as off, and servers added later start off. The display layer is protocol-independent so a
future Matrix client can use it without an external upload provider.

Specification followed: ircv3-specifications `extensions/metadata.md` at
its last change, commit ef205ce (2026-05-07; repository head 9ff58d1,
2026-08-03), and the `avatar` user key of the IRCv3 registry
(ircv3.github.io `_data/metadata_registry.yml`, commit 9480443,
2026-05-21): "URL with an optional `{size}` substitution denoting the size
to load in pixels". ircv3.net itself was unreachable from the build
environment; the sources are the published repositories behind it.
Implemented: capability negotiation (after batch, never with
`metadata-notify`), `METADATA * SUB avatar`, the `METADATA` message,
761/766 for the avatar key, 770–772 and `FAIL METADATA` as diagnostics,
774 with bounded `SYNC` retries, `GET avatar` for ourselves once after
subscribing and for users who join later (bounded, below), and
`SET avatar` with or without a value to publish or remove our own avatar.
Not implemented: LIST, CLEAR, UNSUB, SUBS, `before-connect`, MONITOR-based
updates, 760 in WHOIS, channel avatars and other keys (`display-name`,
`color`, …). Details are in `architecture.md` (IRCv3 capabilities and
User avatars).

**Publishing our own avatar (2026-09-28).** The IRCv3 tab has, per server,
an avatar URL field
and explicit **Send to IRC Server** (IRCサーバに送信; "Publish" in code) /
**Remove from IRC Server** (IRCサーバから削除) buttons. The section has no
explanatory text (by the user's choice, after trying a version with an
exposure warning and hints); the warning that sending shows the URL to
everyone on the network is the Send button's tooltip. Send appears only
when connected with the capability and the typed URL differs from what the
server confirmed; Remove, at the right end in the warning color and asking
for confirmation, only when the server holds an avatar; a request in progress or a
failure is the only status line. The URL is a draft saved
with the server profile (`ServerProfile::avatar_url`, added without a
version change, empty when absent); autosave never publishes it, and it is
never republished on reconnect. Publish sends `METADATA * SET avatar
:<url>` on the server's current connection only when it is registered with
the capability negotiated; Remove sends `METADATA * SET avatar` (never
`CLEAR`, so no other key is touched). One request is outstanding per
connection; success is shown only from the server's `761`/`766` answer,
and `FAIL METADATA` (including `RATE_LIMITED` with its delay), a 20 s
timeout, a disconnect or losing the capability end it as a failure.
Requests carry session-wide identifiers so answers to earlier or abandoned
requests never confirm a later one. What the server holds is tracked
separately from the draft (`app::own_avatar`), from a single `METADATA *
GET avatar` after subscribing and from changes reported for our nickname;
changes made elsewhere are shown, not overwritten. Before sending, the URL
must pass the avatar fetch policy without being fetched
(`media::policy::publishable_avatar_url`: http/https, default port, public
host, no user name or password, no token-like query or fragment
parameter), contain no spaces or controls, be at most 400 bytes (one IRC
line) and be ASCII on legacy encodings. `{size}` is kept as typed. Turning
**Show user avatars** off changes nothing on the server.

The avatar value is a URL because the registry defines it so and metadata
carries only short text (Ergo: 350 bytes of key and value); IRC has no
upload of its own. When an image host is configured on the Image Upload
tab, an image can also be dropped on the avatar section, pasted into the
URL field or picked with **Choose Image…**. It first opens in a square
selection editor (`media::avatar_edit`, decoded off the UI thread with the
preview limits, EXIF orientation applied, kept at most 2048 px a side while
open): the selected square has a handle at each corner to resize it and
can be dragged to move it; when a drop leaves it under half of the
image's shorter side, the view shows it centered in twice its size so it
can be adjusted precisely (otherwise the whole image), **Whole Image**
starts over, and a small preview shows the result; the upload button names the
host and is the confirmation. Only the selected square is uploaded, shrunk to at most
256×256 (JPEG quality 88, or PNG when the image has transparency), so a
full-resolution photo never leaves the computer. PNG, JPEG, GIF, WebP,
BMP and TIFF can be edited; HEIC/AVIF are refused with a hint to export
JPEG. The upload goes through the same `ExternalUploader` as chat images
(`app::attachments::AttachmentFlow`, whose target is generic: a chat draft
or a server profile), and the returned URL replaces that server's avatar
draft if it passes the same checks and is then sent to the server at once
(users expect an uploaded avatar to be in use); if the server is not
connected the draft is kept, a line says it was not sent, and Send
remains available. "Choose Image…", dropping and pasting an image are
offered whatever the server supports (also disconnected, or on a server
without avatar metadata, so the URL can be shared with peers through CTCP
AVATAR); the image is sent to the server at once only when it can receive
an avatar (connected, capability negotiated). When
the current connection asked for avatar metadata (option and batch on when
it started) and registration completed without it being enabled, the
section says that the IRC server does not support avatars; this is an
inference from capability negotiation, not a query, and it is not shown
when the options were only turned on after connecting. A typed URL
is still sent only with the button. Without an image host the URL is
typed. A future
standard upload service (for example if soju's `soju.im/filehost` becomes
an IRCv3 specification) would be another source of that URL; the
settings section would stay as it is.

**Users who join later (2026-09-28).** The draft sends a channel's
metadata to the user who joins, not the joiner's metadata to the members
already there, and Ergo 2.19 follows it, so a later joiner showed no
avatar. A live JOIN of a user who shares no other channel with us and has
no known avatar schedules one `METADATA <nick> GET avatar` after 2 s,
unless the server announces the value meanwhile. Lookups are deduplicated
per user, at most 64 pending (further joiners are skipped with one
diagnostic), at most 8 unanswered, at most two per second, given up after
30 s, retried once after `RATE_LIMITED` (which also pauses all lookups),
cancelled when the user leaves (an answer already on its way is dropped
unless the name joined again first), moved on NICK, and never sent for
replayed JOINs, NAMES or redraws. Everything lives in the connection's
worker, so a reconnect, a removed server or an old connection generation
cannot receive answers. No periodic polling exists.

Interoperability: checked against Ergo v2.19.1 (commit 63c743a) built
from source in a scratch directory with its default configuration on
loopback (`scripts/ergo-metadata-interop.sh`,
`crates/irc-core/tests/metadata_interop.rs`); results and observed wire
behavior are in `development.md`. Ergo implements `draft/metadata-2` by
enabling its `draft/metadata-3` code: it announces other users' changes
as `761`/`766` numerics addressed to `*` instead of `METADATA` messages,
names failures `INVALID_KEY`/`INVALID_VALUE`/`FORBIDDEN` (the draft:
`KEY_INVALID`/`VALUE_INVALID`), answers a removal with `766` even when
the key was not set, sends `774 * *ALL 0` after SUB, limits key plus
value to 350 bytes without advertising `max-value-bytes`, allows 10
changes per 2 minutes, and keeps no metadata for a user without an
account after they disconnect. CayenChat accepts both spellings and treats
a numeric whose first parameter is `*` as a notification, not an answer;
`*ALL` is ignored (not in the draft). The draft does not say how a removal
is announced; a `METADATA` line without a value (as in the earlier
`metadata-notify`) or with an empty value removes the avatar. irc-proto
1.1.0 parses `METADATA` with the old metadata-3.2 client grammar and does
not know 770–774, so arguments are read back positionally. Legacy
encodings accept (and publish) only ASCII avatar URLs.

Identity policy (details in `architecture.md`): avatars are per network and
per occupancy of a nickname, delimited by message sequence numbers, so a
same-named user on another server or a later user of the nickname never
inherits an avatar, and lines from before an avatar was known show none.
Omitting an avatar is preferred to guessing.

Media: avatar URLs go through `cayenchat-media` like previews but are
recognized without a file extension (`policy::avatar_url`); safety checks,
redirects, response and decode limits, background decoding and the static
first frame are shared. `{size}` becomes 128 (`ORIGINAL_PIXELS`, one
fetch whose kept bytes serve every display size). Limits: 2 MiB response,
10 s total, sources up to 4096 px a side and 4.2 MP, decoder allocation up
to 24 MiB, centered square cropped to 32×32 px and shown at 16×16 logical
px. A separate cache (256 records, 2 MiB charged at twice the BGRA bytes,
32 queued, 2 in flight) keeps avatars and previews from evicting each
other; fetches in flight are limited to 3 for avatars and previews
together, and decoding stays one at a time in the process. Avatars do not
use `upload`/`ExternalUploader` or GPUI's URL image loader.

## D024 — Opt-in server password without TLS

**Status:** Accepted

2026-09-28 (issue #23). A bouncer such as ZNC on the user's own LAN or VM
often has no TLS listener and authenticates with `PASS user/network:password`;
D010's TLS requirement made it unreachable, while LimeChat connects to it.
Each server profile gets **Send the server password without TLS**
(`ServerProfile::allow_plaintext_pass`, `ConnectionConfig::allow_plaintext_pass`).
It is off by default and read as off when absent, so it was added without a
settings version change. Turning it on asks for confirmation in a warning
dialog; while on and TLS is off, the settings show a persistent warning. It
only relaxes the server PASS check: SASL PLAIN still requires TLS.

Loopback or private addresses are not allowed automatically: a hostname does
not prove the network is trusted, and plaintext credentials should never be
sent without the user having chosen it.

## D025 — Peer avatars with KVIrc's CTCP AVATAR (URL only)

**Status:** Accepted (experimental)

2026-09-28. Most IRC servers do not offer `draft/metadata-2`, so avatars
(D023) only worked on a few. KVIrc has exchanged avatars between clients
since 3.0 with CTCP AVATAR; CayenChat follows its protocol instead of
inventing a command, so it works between CayenChat users and, where
possible, with KVIrc users.

Wire format (KVIrc 5.2 `doc_ctcp_avatar.html`; source on master,
`KviIrcServerParser_ctcp.cpp` `parseCtcpRequestAvatar`/`parseCtcpReplyAvatar`,
`KviIrcServerParser_numericHandlers.cpp` WHO reply,
`KviIrcConnection.cpp` login and `libkviavatar.cpp` `avatar.notify`/`avatar.query`,
checked 2026-09-28):

- Mark: the realname starts with ETX (0x03), an ASCII digit `0`–`7` and
  SI (0x0F); bit 2 of the digit means "has an avatar" (bits 0 and 1 are
  KVIrc's gender). KVIrc sends `\x034\x0f<realname>` and may add its
  nickname color tag after the mark.
- Query: `PRIVMSG <nick> :\x01AVATAR\x01`. KVIrc sends it when a WHO
  reply shows the mark on a user without an avatar.
- Answer: `NOTICE <nick> :\x01AVATAR <file> <gender>\x01` with `M`, `F` or
  `?`; without an avatar `\x01AVATAR \x01`. `avatar.notify` announces
  `\x01AVATAR <file>[ <size>]\x01` or `\x01AVATAR\x01` by NOTICE to a
  nickname or channel. `<file>` is an `http(s)` URL or a local file name
  that the receiver fetches with DCC GET (spaces as `\040`).

What CayenChat does:

- **IRCv3 → Peer avatars (CTCP AVATAR, experimental)**
  (`Ircv3Preferences::peer_avatars`, per server, off by default, added
  without a version change). It turns on receiving and the possibility to
  share; it needs no capability. Display and downloads still follow only
  **Show user avatars**.
- **Sharing is explicit and separate from the server.** "Share with Peers"
  copies the current draft, after the same checks as publishing
  (`publishable_avatar_url`, one line, ASCII on legacy encodings) and no
  `{size}` (KVIrc would request it literally), into
  `ServerProfile::peer_avatar_url`. Changing the server's own avatar (the
  draft or an upload) to anything but the shared URL stops sharing
  (`stop_sharing_if_avatar_differs`), so peers are not left with the old
  picture; the new one is shared only by choosing "Share with Peers" again.
  "Stop Sharing" clears it, and so does turning the option off, so turning
  it on again shares nothing until chosen. "Send to IRC Server" (metadata)
  is unchanged and independent.
- **Telling askers of a change**: users who sent us a private CTCP AVATAR
  query are remembered per connection (256, only while they share a
  channel with us; dropped on QUIT or leaving, followed on NICK, empty after
  reconnecting). When the shared URL changes or stops, each is told with a
  private `NOTICE <nick> :\x01AVATAR <url>\x01` (`\x01AVATAR\x01` to remove),
  one per two seconds, only the latest state (a user already holding it is
  skipped). Never a channel NOTICE (KVIrc's `avatar.notify` to a channel):
  clients without CTCP AVATAR would show it. Receivers already take a
  private NOTICE from a channel mate over an earlier answer. Sharing first
  started while connected reaches nobody, as no query was invited by the
  realname mark; the reconnect line covers that.
- **Advertising**: only with the option on and a URL shared, the `USER`
  realname becomes `\x034\x0fCayenChat`. The realname is sent at
  registration, so the mark changes only on the next connection; the tab
  says "Reconnect to update the avatar mark in your real name." while the
  connected server's mark (or whether peer exchange is on at all) differs
  from the settings. The URL answered to queries follows Share/Stop at once
  on the current connection (`Connection::share_avatar`).
- **Answering**: a private `\x01AVATAR\x01` from a user is answered with
  `NOTICE <nick> :\x01AVATAR <url>\x01` only while a URL is shared; no
  gender field is sent (KVIrc reads it as unknown). At most one answer per
  user per minute and five per ten seconds. Channel queries, replayed
  history, our own echoes and queries while nothing is shared get no
  answer. We never offer a file.
- **Discovery** is bounded and targeted. NAMES carries no realname and
  `extended-join` is not negotiated. Marks are read from WHOIS (311) and
  WHO (352) replies, including ones the user asked for, and a user who
  speaks live (channel or private message) while sharing a channel with us,
  and about whom nothing is known, is looked up once with `WHO <nick>`.
  Only users showing the mark are queried. No channel-wide WHO, no query to
  unmarked users, no polling. Lookups and queries are deduplicated per user
  until they leave, queued up to 32 (further ones skipped with one
  diagnostic), sent one every 2 s with at most 4 outstanding, given up
  after 30 s, paused 30 s by `263 RPL_TRYAGAIN`, and never re-asked after
  a timeout. Our WHO replies, their end and `401` for our probes stay out
  of the server log; a WHO the user types for a nickname we are looking up
  at that moment is also absorbed (rare, accepted).
- **Receiving**: a CTCP AVATAR NOTICE (answer or announcement) is used only
  from a user present now, as the server names them: to us from someone
  sharing a channel, or to a channel from one of its members; never from
  replayed history, ourselves or a server. The first parameter must be an
  `http://` or `https://` URL (quoted empty, empty, file names, escapes,
  other schemes → the user has no usable avatar and a known one ends); the
  second field (gender or size) is ignored. The URL is untrusted text like
  a metadata value: the media layer applies the avatar URL policy, fetch
  limits, redirects, validation, decoding limits and cache (D023). No DCC
  GET/SEND is sent or accepted and no DCC code exists.
- **Identity** (changed by D058, 2026-10-19): a peer avatar is not tied to
  an occupancy. It is remembered by user mask `nick!user@host` (loosened)
  and per network in memory, and shown on every line of the name, earlier
  ones included. The per-connection tracking (moved on NICK, a lookup in
  flight for the old name absorbed but not used, ended on QUIT or leaving
  the last shared channel) now only decides who is asked. Metadata keeps
  the application's occupancy policy.
- **Metadata wins**: peer pictures are reported as their own event
  (`Event::PeerAvatar`) and metadata's events pass through untouched; the
  application shows the metadata avatar where it applies and the name's
  peer picture otherwise. `irc-core::peer_avatar` only notes metadata, so a
  user with a metadata avatar is not queried.
- CTCP AVATAR traffic makes no chat row, unread mark, highlight,
  notification or preview. With the option off, CTCP AVATAR is never
  answered and is shown like other CTCP (D029), for example
  `CTCP AVATAR request from kv (not answered)`.

- **ISO-2022-JP and the textual mark (#409).** KVIrc's mark needs SI
  (0x0F), which ISO-2022-JP cannot carry (`encoding_rs` receives it as
  U+FFFD and sends `&#65533;`), so a mark may arrive broken or a leading
  `4` may show. (1) On connections in ISO-2022-JP CayenChat does not add
  KVIrc's mark (`ConnectionConfig::advertises_avatar` is false, for `USER`
  and `SETNAME`); the shared URL is still answered to queries, and the
  "reconnect" hint follows the same rule. (2) When receiving, a realname
  that starts with the 9 printable ASCII characters `\x03<digit>\x03`
  (backslash, `x`, `0`, `3`, a digit, backslash, `x`, `0`, `3`; not
  control characters; exact case) with the fours place set in the digit
  (4–7) also counts as "has an avatar", in addition to KVIrc's mark.
  `without_mark` strips either, so CayenChat's own WHOIS and `SETNAME`
  displays do not show it. It is receiving only: CayenChat never adds it;
  a user who wants it types it at the start of the real name (Tiarra:
  `name: \x034\x03Name`). Other clients show the characters, and KVIrc
  does not recognize it; this is a CayenChat-only convention and
  accepted. A realname that happens to start with it costs one query,
  within the existing probe bounds. (3) Discovery by speaking or WHOIS is
  not reliable enough to check a mark by hand (issue #409 report), so the
  WHOIS window shows the user's avatar when one is known and has a "Get
  avatar" button, enabled when the 311 real name carried a mark
  (`WhoisInfo::avatar_mark`) and the server exchanges avatars. It sends
  one CTCP AVATAR query even if the user was asked before; it goes
  through the same queue (spacing, bounds, one query per user), and only
  for users who share a channel with us. D057 adds an invisible mark that
  CayenChat itself sends on ISO-2022-JP.

Interoperability: verified against wire-accurate fixtures written from
KVIrc's source (realname mark, query, `M`-suffixed answer, empty answer,
file-name answer, channel announcement); not tested against a running
KVIrc. irc-proto writes our query as `PRIVMSG <nick> \x01AVATAR\x01`
(no `:` before a single-word last parameter), which is the same message.
Known limits: KVIrc finds a CayenChat user only when a WHO reply shows it
our realname; KVIrc accepts our answer's URL
but applies its own image limits; users who never speak and are not
WHOISed are not discovered; `{size}` is not shared. IRCnet's 352 carries a
server ID before the realname, and bouncers (Tiarra) do not pass 042
(RPL_YOURID) on, so a realname without a mark as it is gets a first word of
the ID's shape skipped and is checked again (on an RFC server a realname
`1ABC <mark>` costs one extra query, accepted). Some bouncers append
`@network` to the name in 315 and 401; it is matched before the `@`. The observations behind these choices (IRCnet's
352, a raw KVIrc mark on IRCnet, how KVIrc and LimeChat read it, bouncers'
realnames) are recorded in Japanese in
`spec/avatar-mark-research-2026-10-19.md`.

## D026 — Protocol-neutral message identity

**Status:** Accepted

2026-09-28. Retained messages carry the application's own identity (the
arrival `sequence`), the source's timestamp and native identifier when it
supplied them, and a provenance (`Live`, `Replayed`, `Requested`), so
history requests, reconnect recovery and echo reconciliation can
recognize overlap. The IRC `msgid` is stored as an opaque
`model::NativeMessageId`, never as the universal identity: most IRC lines
have none, and another backend (for example a future Matrix event ID) has
its own identifier with its own scope. Logs are never re-sorted by
timestamp. Duplicate suppression is per conversation and bounded (512
keys, forgotten with the conversation); a fingerprint fallback only drops
history, never a live line, because it is not collision-free. No
persistent deduplication and no Matrix-specific fields. Details in
`architecture.md` (Timeline items).

## D027 — Recent channel history with draft/chathistory

**Status:** Accepted (experimental)

2026-09-28. A per-server IRCv3 option (off by default, no settings version
change) requests `draft/chathistory` and asks for `CHATHISTORY LATEST
<channel> * min(50, server limit)` when we join a channel. The
specification (ircv3-specifications `extensions/chathistory.md`, commit
9f65105, 2026-05-14) says a client that negotiates the capability should
no longer get automatic playback, so negotiation and requests come as one
feature. CayenChat's own requirements, not the specification's: `batch`
must be acknowledged first (replies are recognized by their batch), and
`server-time` and `message-tags` (UTF-8 only) are requested with it as the
specification's "full support". event-playback, echo-message and
labeled-response are not needed and not requested.

Replies are consumed whole in the connection worker, reported once when
the batch ends, and inserted by the application at the point of the
request (reserved sequences) as requested history, which never notifies,
highlights or marks unread and is not shown in the combined subwindow.
One request at a time, 50 lines asked, 100 kept, 64 channels queued, 30 s
timeout. Older pages, gap recovery on reconnect, private-message history
(TARGETS) and persistence are separate future work. Details in
`architecture.md` (Channel history); Ergo results in
`development.md`.

## D028 — Dedicated private conversations

**Status:** Accepted

2026-09-28. Private messages get their own conversations instead of the
server log. `model::Conversation` gained one concept, `ConversationKind`
(`Channel` or `Private { peer_key }`), so the application and UI work on
conversations and timeline items rather than IRC targets; the server log
stays separate. The peer key is supplied by the protocol adapter (IRC:
RFC 1459 case-mapped nickname) and scoped to one network. NOTICEs open no
conversation (services and bots), NICK renames without ever merging two
conversations, QUIT marks a boundary, accounts are not used, and at most
100 private conversations exist per network. No protocol framework was
added: IRC routing rules live in the UI's event adapter, and a future
backend would create conversations of the same kinds with its own keys.
Details in `architecture.md` (Conversations).

## D029 — Answers to common CTCP queries

**Status:** Accepted

2026-09-28. Until now only CTCP AVATAR (D025) was handled. Other CTCP
requests (`PRIVMSG alice :\x01VERSION\x01`) got no answer, showed up as
raw server lines with control characters, and in channels even became
chat rows. Other clients expect PING, VERSION, TIME and CLIENTINFO.

`irc-core::ctcp::CtcpReplies` (per connection, in the worker) handles every
CTCP PRIVMSG or NOTICE except ACTION, and except AVATAR while peer avatars
are on (`peer_avatar` sees it first):

- **Answered** only when the request is private, live, from a user and not
  our own echo, by `NOTICE <sender>`: PING echoes its argument (not
  answered above 128 bytes or with an inner 0x01), VERSION
  `CayenChat <version>`, TIME the local time
  (`Mon Sep 28 12:34:56 2026 +0900`), CLIENTINFO
  `ACTION [AVATAR] CLIENTINFO PING TIME VERSION` (AVATAR only while peer
  avatars are on).
- **Not answered**: DCC and AVATAR while peer avatars are off, and tags
  that are not plainly spelled. (Amended 2026-10-16, below: USERINFO,
  FINGER, SOURCE, ERRMSG and unknown tags are now answered.) Channel requests (STATUSMSG `@#chan` included) are never
  answered: one request can reach a whole channel of clients, and
  answering is what floods.
- **Shown** as one server line: `CTCP VERSION request from bob`,
  `CTCP TIME request from carol to #test (not answered)`,
  `CTCP VERSION reply from bob: irssi 1.4` (formatting and controls
  stripped, cut at 200 characters). Never a chat row, private message,
  mention, unread mark or notification. Replayed history, our own echoes
  and CTCP to other targets are dropped silently, and requested channel
  history (D027) leaves CTCP other than ACTION out.
- **Rate limit** in the style of D025: requests (answered or only shown,
  channel ones included) at most five per ten seconds in total and two per
  user; past that, one line
  `Too many CTCP requests; ignoring them for now (…)` at most once per ten
  seconds, and nothing else. Replies are shown under a separate
  budget of the same size, so a reply flood cannot stop answers. Together
  with AVATAR answers we send at most ten CTCP NOTICEs per ten seconds.
- **No privacy setting for VERSION.** The `USER` realname is already
  `CayenChat` and visible to everyone through WHOIS/WHO, and the answer
  has no OS, architecture, host or library names, so a switch would hide
  only the version number. TIME reveals the time zone offset, which is the
  behavior users of other clients expect; a setting can be added later if
  asked for.

**Amendment 2026-10-16 (issue 427): more answers, `/ctcp` and `/ping`.**

- **More answers**, same rules (private, live, from a user, rate-limited):
  USERINFO `CayenChat user`, FINGER `CayenChat user` and SOURCE
  `https://github.com/koshian/cayenchat` are fixed public text. There is no
  setting for a custom USERINFO/FINGER text yet, and no OS login name,
  host, terminal or idle time is ever read or sent. ERRMSG echoes its query
  (`ERRMSG <query> :No error`; no query: `ERRMSG ERRMSG :No error`; not
  answered above 128 bytes or with an inner 0x01), and an unknown tag that
  is plainly spelled gets `ERRMSG <TAG> :Unknown query`. Replies are NOTICEs
  and a NOTICE is never answered, so ERRMSG cannot loop. DCC is still
  shown and never interpreted, answered or acted on. CLIENTINFO lists
  `ACTION [AVATAR] CLIENTINFO ERRMSG FINGER PING SOURCE TIME USERINFO
  VERSION`, exactly what is answered.
- **`/ctcp <nick> <command> [arguments]`** sends `PRIVMSG <nick> :\x01…\x01`
  to a user (not a channel). Refused before the wire: an invalid nick or
  tag, ACTION (use `/me`), DCC, AVATAR (follows the peer avatar setting),
  arguments over 200 bytes or with any control character (0x01, CR, LF,
  NUL), and anything while registration is not finished. The reply arrives
  as the usual `CTCP VERSION reply from bob: …` server line.
- **`/ping <nick>`** (and `/ctcp <nick> PING` without arguments) is a CTCP
  PING, no longer the raw server `PING` (`/raw PING …` still sends that).
  The request carries a per-connection random token. A NOTICE from the same
  nick (case-mapped) to us whose argument is exactly that token is shown as
  `CTCP PING reply from bob on <server>: 12.3 ms`, measured with the
  monotonic clock; any other reply (other nick, other token, to a channel,
  a repeat, or after the timeout) is only shown as a plain reply, not a
  measurement. A ping without a reply is reported after 30 seconds
  (`CTCP PING to bob on <server> timed out …`) and dropped; at most one
  ping per nick and eight pings wait at once. Pending pings live in the
  connection's worker, so a disconnect discards them silently.
- **Pacing**: requests we send share the incoming limit (five per ten
  seconds, two per nick); past it nothing is sent and the log says so. All
  of this runs in the worker, so an unanswered ping blocks nothing.

Checked against Ergo 2.19.1 (the pinned local server of D023's interop
check), whose default channel modes include `+C` (no channel CTCP): answers
arrive as sent, and a flood from three users got five answers, at most two
each, one refusal line and no disconnection. Not checked against other
servers' flood limits; ten NOTICEs per ten seconds is below the defaults of
common servers.

**Amendment 2026-10-19 (issue 497): CTCP lines also in the combined log.**
Every CTCP line (requests, replies, round trips, timeouts, refusals and
"not sent" notes; `Event::CtcpLine`, not `Event::ServerLine`) stays in the
server log and is also shown in the combined subwindow as
`[network] text` in the configurable system message color (the former
"channel event" color; same setting key and defaults, only the name
changed). They are still never chat rows, unread marks, highlights or
notifications. `AppState` keeps the newest 200 such lines across networks
for the combined log; a double-click on one opens that network's server log.

## D030 — Older channel history pages on scroll

**Status:** Accepted (experimental)

2026-09-28. With the same per-server chathistory option (no new setting),
scrolling a channel's main log to within five rows of its oldest line asks
for one older page, `CHATHISTORY BEFORE <channel> <reference> <n>`
(specification: ircv3-specifications `extensions/chathistory.md`, master
`9ff58d1`). Only a user's scroll triggers it; nothing is fetched because a
channel is open, and there is no timer. One page per conversation at a
time; `n` is 50 lowered by the server's `CHATHISTORY` limit and by the room
left under the 2,000-line log bound, so the whole history is never fetched
automatically and the bound still holds.

The reference is the oldest identified line near the top; `msgid=` is
preferred over `timestamp=` because it is exact, within the server's
`MSGREFTYPES`. Paging ends for the session on an empty reply,
`draft/chathistory-end`, a page adding nothing new, or a failure, so the
same request is never repeated by scrolling; a new session resets it.

Pages are prepended with sequences counted down from 2^62 while ordinary
messages count up from it: logs stay in ascending sequence order, which
`LogList`, previews and avatars rely on, without renumbering or re-sorting
anything shown. The viewport keeps its top row by sequence and pixel
offset, so row heights above it do not matter. Overlap is removed with a
temporary filter over the top of the log plus a read-only check of the
conversation's recent keys; older pages are not recorded in the
conversation's 512-key filter.

The application API is protocol-neutral: `request_older_history` returns a
request number, the oldest line's native identifier and source time and a
limit; the answer is `insert_older_history(conversation, request, lines,
beginning)` or `older_history_failed`. A Matrix backend would answer the
same calls from its room timeline's back-pagination and ignore the
reference in favor of its own pagination token. Private conversations,
reconnect gap recovery and persistence are separate future work. Details
in `architecture.md` (Channel history).

## D031 — Reconnect gap recovery

**Status:** Accepted (experimental)

2026-09-28. With the same per-server chathistory option (no new setting;
off means no recovery), a channel that was joined when a connection with
history ended asks, on its first join after the reconnect, for `CHATHISTORY
LATEST <channel> <reference> <n>`: the most recent lines after the newest
message received before the cut (specification as in D030).

- **LATEST with a reference rather than AFTER.** Both return lines after
  the reference; they differ only when the gap exceeds one reply. LATEST
  keeps the newest part, which joins up with the live lines the user is
  reading; AFTER would leave the hole next to them. One request per
  channel; no automatic pagination through a long gap. A full reply
  without `draft/chathistory-end` adds one activity line saying messages
  may be missing.
- **Reference.** msgid when the server accepts it, else the server time
  minus 5 s for clock skew (the specification suggests 1–10 s), relying on
  the duplicate filter for the overlap. Never display time or arrival
  order. No reference, no recovery: the join behaves as before.
- **Placement.** Sequences are reserved at the disconnect, so missed lines
  go where the log was cut, before the rejoin and later live lines,
  through the same insertion and duplicate filter as recent history; logs
  are never re-sorted.
- **Lifetime.** The earliest unanswered cut is kept across failed
  attempts; a part, a reset, or a session that joined without history
  drops it. State is one point per conversation in `app` and the list in
  `ConnectionConfig`; the worker keeps it only until each channel's first
  join. No timer.

A Matrix backend would supply missed timeline items through the same
insertion (its sync gap handling decides what is missing); the resume
point's reference is IRC's concern and is not a generic sync token.
Private-message recovery, persistence and echo-message reconciliation are
future work. Details in `architecture.md` (Channel history).

## D067 — Send key for normal messages is a setting; optional NOTICE key (#583)

2026-10-24. Replaces the fixed Enter = `PRIVMSG` / Ctrl+Enter = `NOTICE` of
D020's chat bindings, because people used to Ctrl+Enter as "send" elsewhere
sent NOTICEs by mistake.

- **Setting.** `Settings.send_key` (`enter` default, `ctrl_enter`,
  `double_enter`), on the Keyboard page (shown on macOS too). Files without
  it read as Enter. Applies from the next key press after saving.
- **Exclusive.** Only the chosen operation sends a normal message from the
  chat draft; the other combinations send nothing. Ctrl+Enter stays Ctrl on
  macOS (no Cmd+Enter). `/notice` is unchanged.
- **Optional NOTICE key.** `Settings.notice_key` (`off` default, `ctrl_enter`,
  `alt_enter`), same page. Off means no key sends a `NOTICE`; a chosen key
  sends the draft as one (the `SendNotice` action, bound in `rebind_shortcuts`
  and part of `ShortcutPrefs`), in the chat draft only (other inputs take it
  as "done", the log search as "next"). It also ends a wait of Enter twice.
  Shift+Enter is not offered: the log search uses it for "previous". If it is
  the send key's own press (Ctrl+Enter for both), the send key wins, the
  setting page says so, and the NOTICE key is not bound. Files without the
  field read as off. Added at the maintainer's request after the first
  version had dropped the key altogether.
- **Scope.** Only the chat draft. The wizard, prompts, PART confirmation,
  settings and log search keep their Enter handling (both bindings still
  reach the same handler, which only the draft restricts).
- **Enter twice.** The first Enter on a non-empty, non-composing draft only
  waits (no newline: the draft is one line). A second plain Enter within
  700 ms sends once. The wait is dropped by any other keystroke (a Ctrl+Enter
  included), a change of the text or caret, losing the focus, the window
  being activated or deactivated (the draft keeps its logical focus while
  another window is in front), showing another conversation, a changed
  setting, or the time. The empty draft's hint names the chosen key. Auto-repeat is not a second
  press: not every platform marks repeats, so a press is a repeat when the
  Enter key-up has not been seen since the last press. IME confirmation never
  counts (composing drafts are skipped). State: `double_enter.rs`, checked
  on every render and by a keystroke interceptor; nothing is carried between
  conversations.

## D066 — Local `/help` lists the main commands in the server log (#586)

`/help` is handled in `send_draft()` before any connection check, like
`/charset`; it is never sent to the server. The text (`help_text` in the
locales, current language) is added line by line to the selected server's log
with `append_server_message()`: no selection change, no unread mark, no
notification. Only `/help` with no arguments lists; any argument gives a usage
line. `/helper` and similar are not `/help`. The list is "main commands of
CayenChat", not all IRC commands (unknown `/commands` are still sent raw), and
there are no topics, key listings or search. With no server configured the
normal "not connected" error is shown.

## D065 — Jump to the latest message: shortcut and log button (#546)

**Status:** Accepted

- **One action.** `scroll_to_latest` is an entry of `shortcuts::ACTIONS`, so
  the settings tab, saving, conflict warnings and reset work as for the
  navigation actions (#114). Its `Target::ScrollToLatest` is a `ui` action
  (`ScrollToLatest`) handled by `ChatWindow`; `cayenchat_app::Command` and
  the application state do not know it. Defaults: Ctrl+End on Windows/Linux
  (the draft binds plain End only), Cmd+J on macOS (Cmd+Down is the next
  active channel; MacBooks have no End).
- **One scroll.** The shortcut and the button call
  `ChatWindow::scroll_main_log_to_latest`, which syncs the selected
  conversation's `LogList` and calls `LogList::scroll_to_end`. The list then
  follows the bottom again; scrolling up releases it as before. Only that
  conversation's list is touched, and not the draft, caret, focus, selection
  or search. It does nothing while the quit question, the leave question or
  the member prompt is open; the settings window is another window, and a
  recording key press is consumed by it. #545 uses the same function.
- **Button.** A 28 px round button in the main log's lower right, left of the
  scrollbar strip, drawn only while `LogList::is_behind_latest` (the scroll
  distance is more than 1 px short of the maximum, so a partly visible last
  row counts as behind and a log that fits never does). It reads the list's
  state of the previous layout when the pane renders; the pane already
  redraws on scroll, scrollbar drag and chat state changes, so there is no
  per-frame work of its own. It takes its own mouse down, so a click does not
  move focus to the log. Tooltip: the action's name and its current keys.
  The sub log has none.

## D064 — Log position when an unread conversation is selected (#545)

**Status:** Implemented

`Settings::unread_log_position` (Application page), `previous` by default and
added without a version change, so older files read as `previous`.

- **`previous`:** the position the conversation was left at, as before.
- **`latest`:** the main log scrolls to its end once and follows the bottom
  again (`LogList::scroll_to_end`, the state of a never-scrolled list).
- **When:** only if the selection changes to a channel or DM that is unread at
  that moment, by any means (Ctrl+Tab, toolbar, tree click, number keys,
  notification). `ChatWindow::dispatch_keeping_focus` notes the unread
  conversations before `AppState::select_channel` clears the mark. Read
  conversations, the server log and the combined log always keep their
  position. A search or notification jump to a message wins over it.
- It is a single scroll at selection: scrolling up afterwards, new messages,
  CHATHISTORY and preview row changes behave as usual. When the unread mark is
  cleared is unchanged.

## D063 — Errors of the chat window are dialogs, never a line in the draft row (#528)

**Status:** Accepted

A long error in the draft row took the room of the text field, so the draft
could not be edited. Nothing is drawn in the draft row now besides the field
and the upload progress. Every error and warning that used to be there (a
failed or refused send, connection, disconnect, WHOIS, join/part, member
command, settings and order saving, start-up notices) opens a modal dialog
with a Close button (`window.prompt`, `notices.rs`).

- **Opened from events.** `post_notice` queues the notice and opens it from a
  spawned task on the window handle, so a failure that has no `Window` at hand
  and a failure found while drawing alike never create a dialog in a draw.
- **One at a time, bounded.** A notice equal to the open or a waiting one is
  dropped; other notices wait (at most 8, then dropped and logged) and open one
  after another. Any other dialog of the chat window (upload confirmation,
  disconnect-all) counts as open too, so it is never replaced. A notice a
  background process raised (the selected server's connection ending) is also
  dropped for 30 seconds after the same text was closed, so failing retries do
  not reopen it; what the user triggered is shown every time.
- **Closing changes nothing.** The draft, caret, selection and undo history are
  untouched, nothing is sent, retried or selected. When nothing has the focus
  afterwards, the draft of the selected conversation gets it; a form the user
  is working in keeps it.
- **Member prompt.** `prompt_feedback` is the error line inside the nickname,
  topic, mode, join and similar prompt; it stays there and opens no second
  dialog. Settings windows keep their own validation lines.
- **Server replies 417 and 400.** `irc-core` reports them as
  `Event::CommandRejected` next to the `ServerLine`, so the server log keeps
  them. The window opens a dialog (server, code, command for a 400, the
  server's text without formatting codes) only as the answer to something the
  user said from the draft: a message in a channel or query, or `/me`. That one
  place in `send_draft` records the time per server (at most 8). A 417, or a
  400 whose command is PRIVMSG or NOTICE, within 10 seconds of it opens one
  dialog and uses that send up. Every other reply stays in the log: commands
  (`/msg`, `/raw`, `/names`, CTCP and the like), JOIN (also the one resent
  with a channel key), PART (also the leave confirmation), the member menu and
  automatic traffic. There is no guessing of the command a line is sent as.
  A line refused locally by the 512-byte check was never sent, so no 417
  follows it.
- **Uploads** keep their dialogs (#481) through the same queue; progress and
  Cancel are unchanged.

## D062 — Auto Phrase: BudouX phrase wrapping for Japanese (#537)

**Status:** Accepted (experimental)

- Experimental option `experimental.auto_phrase`, off by default (reset with
  the tab). Off, nothing is analysed and layout is exactly as before.
- Display only. The text, selection, copy, search offsets, links, stored
  logs and what is sent are untouched; no characters are inserted.
- Model: BudouX v0.9.3 `ja.json`, embedded unchanged in
  `crates/model/data/` (Apache-2.0, update steps in its `README.md`; the
  license text is `licenses/BUDOUX-APACHE-2.0.txt`, shipped in every
  package, and `THIRD_PARTY_NOTICES.md` names it). `model::phrase` reimplements the scorer (not copied code)
  and a test compares splits with the official `parser.py` output.
- Mechanism: `phrase::joins` lists byte offsets between two Japanese
  characters where BudouX has no phrase boundary. They reach GPUI through
  `StyledText::with_wrap_joins` (a patch in `vendor/gpui`, see its
  `PATCHES.md`) and the wrapper does not use those offsets as wrap
  opportunities. A line with no other opportunity breaks anywhere as
  before, so long words and URLs still fall back safely. ASCII, URLs and
  emoji around Japanese are never touched. Not every phrase starts a line:
  the wrapper only prefers phrase boundaries.
- Scope: channel and direct-message log rows. Server, activity and
  sub-log rows are unchanged. Only the message's natural-language
  stretches are analysed: the sender prefix, links, link chips and nick
  highlights are excluded (each remaining stretch is analysed on its own,
  offsets mapped back to the whole row). Offsets are cached per row (text,
  prefix length and protected ranges; 512 entries, `phrase_wrap.rs`); the
  offsets are part of GPUI's wrapped-line cache key.
- Japanese detection is conservative: a text is analysed only if it
  contains kana, so Chinese and kanji-only text keep the normal wrapper, and
  joins only join kana, kanji, CJK punctuation and full-width/half-width
  katakana forms (not half-width Hangul). Mixed Chinese/Japanese text is
  analysed as Japanese; kanji-only Japanese text is not analysed.
- Limits: no kinsoku (line-start/end prohibition) handling beyond what the
  model implies; other languages' models are future work.

## D061 — First-run setup wizard (#521)

**Status:** Accepted

- **When.** Only when startup found no saved server: no settings file, or one
  that was read but holds no server (the wizard also reaches a user who saved
  only general settings). A file that cannot be read (damaged or from a newer
  version) is not a first run, so an existing user is never interrupted. The
  wizard stands in for the settings window that used to open for an empty
  configuration. It is drawn in the chat window (`setup_wizard.rs`) and is
  the same on every OS. Cancelling ("Not now", Escape) writes and connects
  nothing; with no server saved it opens again at the next start.
- **Three choices**, quick setup preselected and marked as recommended:
  quick setup; manual setup, which closes the wizard and opens the
  connection settings; import, which is the File menu's import (D059).
  While the import is picked, read or confirmed the wizard is hidden; if it
  ends without a server (cancelled or failed) the choices are shown again.
- **Quick setup** shows the fixed server (`irc.ircnet.com`, the first
  preset), ISO-2022-JP and `#CayenChat`, and asks only for a nickname. Only
  "Connect with these settings" writes anything: it checks the nickname with
  the checks of a connection, adds one ordinary server profile (a new
  server's defaults for port, TLS and passwords; `#CayenChat` as an enabled
  auto-join entry; connect on startup) to the settings file, applies it
  through `apply_settings_change` and connects through `connect_profile`.
  There is no connection path of its own. The `USER` username is the chosen
  nickname and the real name stays at the built-in default; nothing comes
  from the operating system. The save runs in the background, adds to the
  file's current content (never replaces it; a file that cannot be read at
  that point is left untouched, nothing is connected and the error is
  shown), and a failed save leaves the
  settings and connections as they were with the error shown. A connection
  that cannot even start leaves the saved profile and offers the connection
  settings; a connection that fails later is shown by the normal log and
  reconnect behavior.
- **Not done:** no menu entry to run the wizard again (the settings window
  and File → Import stay the ways in once servers exist).

## D060 — Numeric character references for text ISO-2022-JP cannot hold (#522)

**Status:** Accepted

- Per-server `ServerProfile::iso2022jp_char_refs`, on by default (files
  without it read as on), a checkbox below the encoding on the server's
  settings page. It is kept for every encoding but acts only when the text of
  a message is sent in ISO-2022-JP: the server's encoding, or the channel's
  `/charset` send override (which decides, either way). The worker reads the
  value of the connection's start, so a saved change applies from the next
  connection (`apply_servers` updates `active_config` for reconnects).
- `Connection::pinned` converts after it fixes the channel's encoding, so
  the converted text is what is checked (encodability, 512 bytes, the length
  once relayed (#330)), queued and written, even if the override changes
  while the message waits. Over the limit the message is refused as before;
  nothing is cut or split.
- Only the text of `PRIVMSG` / `NOTICE` (`send_message`, `send_private_message`,
  `/msg`, `/notice`) and the description in `/me` is converted. Each Unicode
  scalar value ISO-2022-JP cannot hold becomes `&#<decimal>;` on its own, in
  order (ZWJ and variation selectors included; no normalisation). Other CTCP
  texts, nicknames, channel names and every other command are unchanged and
  keep the strict check. Credentials sent to services (`is_service_secret`:
  `IDENTIFY`, `REGISTER`, ...) are never converted and keep the strict check;
  a comma-separated target list counts if any recipient is such a service.
  Off, a message with such a character is refused.
- `Outgoing::Message.display_text` follows the converted text, so the
  confirmed line and the log show what was sent; the input box keeps the
  original and is cleared only on success. Receiving (D043) is independent:
  with it on, the log draws the characters back with the sent reference in
  the hover bubble.

## D059 — Exporting and importing the settings file, without credentials (#520)

**Status:** Accepted

2026-10-21. The File menu (CayenChat menu on macOS) has "Export Settings…"
and "Import Settings…", and the Application page has the same two buttons.
They move `settings.json` between computers; `cayenchat_storage::transfer`
does the work, `settings_transfer` draws the questions in the chat window
(like the quit dialog, Cancel focused first).

- **Export** copies the saved settings (the file, else the window's copy) in
  the current versioned format to a file the user picks (default name
  `cayenchat-settings.json`), replacing it whole or not at all, user-only
  permissions. The settings file is not touched. Secrets cannot get in:
  they are not in `Settings` (D014; the legacy plaintext fields are never
  serialized) and nothing reads the credential store. A notice before the
  save dialog says what the file does contain (server, channel, nick and
  real names).
- **Import** replaces the whole configuration; no merging, no selection of
  servers (future work). The file must be a regular file of at most 1 MiB,
  an object with a supported `version` (older ones go through the normal
  migration, a newer one is refused with the settings left alone), a server
  list (at most 256 servers) and valid values. A list that loading would
  quietly trim (a server without id or host, a repeated id, port 0) is
  refused, as is a damaged version 1 file (no or unknown `server`, a custom
  server without host, port 0), so an import never restores less than the file lists. It is read in the background
  and only then confirmed: what is replaced, that every connection is
  dropped, that passwords are not carried over, how many servers connect on
  startup (none connects now), and that another OS's settings may not fit.
- **Where each server connects (#552).** The file is untrusted, and a
  display name can read "IRCnet" while the host is another machine that will
  receive the passwords typed after the import. The confirmation therefore
  lists the imported servers one by one with the real host and port, next to
  the display name when there is one (`name → connects to host:port`), and a
  bold warning line for each of: no TLS, TLS without certificate
  verification, and permission to send the server password without TLS. The
  first 16 servers are always listed; beyond them only servers with such a
  warning are, and the rest are counted ("and N more"). Names and hosts are
  made safe where they are drawn, not trusted from the import check:
  controls and line breaks become spaces, bidirectional controls are
  neutralized (`model::display`), and a value is cut at 64 characters.
  What the import accepts is unchanged.
- **Credentials.** Every imported server gets a fresh `custom-<uuid>`
  profile id (the selection follows), so a credential-store entry of this
  computer can never be taken for an imported server's, even when ids
  happened to match; plaintext passwords of old files are dropped, not
  migrated. The credentials of the servers being replaced are left in the
  store (not deleted, so restoring the backup still finds them); they are
  unused entries until removed by hand or by a later cleanup.
- **Machine-specific values** stay as this computer has them: the
  credential backend, the debug log file, and the conversation log folder
  (the choice to log is imported).
- **Off the UI thread.** The export write and the import's backup (synced
  to disk), replacement and reload of the current file run in the
  background while the dialog shows a notice and ignores answers; the
  settings window cannot be opened meanwhile. The running state is changed
  only after the file was replaced. The confirmation's text scrolls inside
  the window height so its buttons stay reachable in a small window. One
  transfer runs at a time from the moment a file picker opens: a `reading`
  flag, separate from what the dialog shows, keeps another import or export
  and the settings window out until the pick and read finish, so a slow
  read cannot replace a running write's dialog. While an export's save
  dialog is open its notice is replaced by the working notice, so it cannot
  be cancelled and a late outcome of the dialog cannot close another
  transfer's state.
- **Applying.** The backup copy `settings-backup-<milliseconds>.json` is
  written next to the file first (a failure stops the import), then the
  file is replaced atomically. The result is applied to the running window
  through the path a settings-window save uses
  (`ChatWindow::apply_settings_change`), so the window and the file agree;
  the open settings window is closed first, since it would save its older
  form over the import. Sessions of the replaced servers are closed;
  nothing connects, `connect_on_startup` counts at the next start. Settings
  read only at startup (the Linux display system) apply then. A failed
  import shows the error, with the current settings unchanged.
- **Not included:** credentials and encrypted backups, `channel-order.json`,
  `window.json`, history, caches and logs.

## D058 — CTCP AVATAR pictures tied to a user mask, without occupancies (#510)

**Status:** Accepted (changes the identity rules of D025; D023's occupancy
policy stays for IRCv3 metadata)

2026-10-19. The first line of a speaker whose avatar was looked up stayed
on the default icon after the answer, because an avatar belonged to an
occupancy of the name and an occupancy began at the line after the avatar
was learned (D023). A reconnect or a rejoin under another name also
started the exchange again. The maintainer decided that CTCP AVATAR, a
stop-gap that lets clients show avatars before servers carry them in
IRCv3 metadata, need not be handled that strictly.

- **Key.** A CTCP AVATAR picture is tied to `nick!user@host` of its sender
  and the network, in the shape KVIrc uses for its avatar cache (KVIrc's
  `KviIrcMask::NickCleanUserSmartNet`; read for its behavior, no code
  copied, D042): the nickname under the server's case mapping, the
  username without leading ident marks (`~ ^ = + -`), and the host
  loosened — a host name of three or more labels becomes `*.` plus the last
  two (`host1.example.org` → `*.example.org`), IPv4 keeps two numbers
  (`203.0.*`), IPv6 the first four groups (`2001:db8:1:2:*`), a cloak
  containing `/` and shorter names are kept; all case-insensitive. Another
  nickname (`bob_`) is another key and is asked again, as in KVIrc.
  `irc-core::avatar_mask`.
- **Memory only, bounded.** The table is shared with the connection's
  worker (`ConnectionConfig::peer_avatar_cache`), kept for the application's
  lifetime across reconnects of a network, at most 512 masks, the least
  recently used first. It is never written to disk (KVIrc keeps 30 days):
  it holds other people's data.
- **Learning and using.** A CTCP AVATAR NOTICE updates its sender's mask
  (no picture removes it), as does a NICK for the new name and mask. Someone
  seen with a remembered mask — a JOIN, a live message — gets the picture
  without a WHO or a query; otherwise discovery is as in D025. A lookup of
  ours finding no mark drops the name's and the mask's picture. "Get
  avatar" in the WHOIS window still queries.
- **Display.** The worker reports `Event::PeerAvatar { nickname, url }`.
  `app::avatars` keeps one picture per name and network (2,048 names, the
  least recently set first), and message rows and the member list ask for
  the name's picture whatever the message's sequence. It survives QUIT,
  leaving and disconnects, and the last known picture is used when nobody
  holds the name. A metadata avatar still takes precedence where its
  occupancy applies. Lines already shown change when the picture is learned.
  Removing or resetting a server drops the names' pictures.
- **Accepted problems.** Another person using the same name shows the
  previous one's picture on their lines, and on past lines of the previous
  one, until a lookup says otherwise (a lookup finding no mark clears it).
  People sharing a mask (a bouncer, or the same cloak and nick) share a
  picture. A name's picture can outlive its owner. These cannot be solved
  with CTCP AVATAR, which is why IRCv3 metadata, handled with occupancies,
  is the right way to carry avatars.
- Not done: persistence; any change to metadata.

## D057 — Invisible mark in the realname on ISO-2022-JP (#490)

**Status:** Accepted

- Adds to the "ISO-2022-JP and the textual mark" paragraph of D025, which
  stays as it was: KVIrc's mark is still never sent on ISO-2022-JP and the
  textual `\x034\x03` is still receive-only. Observations are in
  `avatar-mark-research-2026-10-19.md`.
- The mark is the four raw control codes `02 16 16 02` (bold on, reverse on,
  reverse off, bold off; `peer_avatar::INVISIBLE_MARK`). Checked on IRCnet
  (WHO 352 and WHOIS 311 return the realname as sent) and in LimeChat for Mac:

  | Leading bytes | IRCnet | LimeChat WHOIS | LimeChat stability |
  | --- | --- | --- | --- |
  | `03 '4' 0F` (KVIrc's) | passed through | `4` shows | became unstable |
  | `03 '4' 03` | passed through | `4` shows | fine |
  | `02 02` | passed through | nothing shows | fine |

  KVIrc's mark cannot be sent: KVIrc wants U+000F as the third character after
  decoding, and in ISO-2022-JP only a raw 0x0F makes it, which is out of the
  standard; LimeChat retries a failed conversion byte by byte from the end of
  the line, which unsettled it (SO, 0x0E, has the same problem). A colour code
  (0x03) is not used because LimeChat's WHOIS dialog shows its digits. Bold and
  reverse are bytes ISO-2022-JP may carry, hold no digit, and do nothing for
  clients that format, since they are switched back at once. Four bytes, with
  the reverse pair between, make a name that does not begin so by chance.
- With ISO-2022-JP, peer avatars on and a shared URL, `USER` and `SETNAME`
  start the realname with the mark (`ConnectionConfig::marks_avatar_invisibly`).
  Any mark the user typed is replaced, never doubled. Other encodings keep
  KVIrc's mark.
- No switch (#490, #500): other clients show nothing for the mark, and the
  only cost is four bytes of the realname limit. It is treated like KVIrc's
  mark, which is always sent on UTF-8. A `peer_avatar_invisible_mark` left
  in an old settings file is ignored as an unknown field.
- Receiving: a realname starting with the mark always means "has an avatar"
  (it holds no digit). `without_mark` strips KVIrc's, the invisible and the
  textual mark, in any order and number, so CayenChat's own WHOIS and
  `SETNAME` displays show none of them.
- It applies only from the next connection (IRCnet has no SETNAME).
- KVIrc users: on ISO-2022-JP KVIrc does not find a CayenChat user by itself
  (it does not read this mark). A CTCP AVATAR query from KVIrc is answered as
  before, and CayenChat still queries a KVIrc user who has KVIrc's mark.
- Through a bouncer the mark CayenChat adds does not reach the server,
  because the bouncer sends its own configured realname in `USER` (and raw
  control codes are hard to write into its settings): write the textual mark into the bouncer's realname for
  that network (Tiarra `name:` in the network block; soju
  `network update <network> -realname '\x034\x03Name'`).
- The "reconnect" hint compares the connection's mark (KVIrc's or invisible,
  `ConnectionConfig::marks_avatar`) with what the settings now give.
- `SETNAME` is checked against the connection's mark, and only when an
  avatar is shared (the mark is fixed per connection, like `USER`).

## D056 — Regain operator status whenever alone without it (#478)

**Status:** Accepted

- Widens the scope of D055: its "only a channel the previous connection had
  us as operator in" is replaced by "any joined channel where the roster holds
  only us, without operator status". Reports showed D055 did not fire: the
  record lived only in memory (lost on restart), had a 15 minute window, and a
  long sleep lets the server drop the ghost before we reconnect.
- Nothing is carried between connections any more (`carried`, `CARRY_WINDOW`,
  `Event::OpChannels`, `ConnectionConfig::regain_ops_in` removed).
- The loop guard moves from "once per loss" to once per channel and
  connection, so networks where rejoining gives no operator status
  (services-managed channels, IRCnet's channel delay) are rejoined once per
  connection, not repeatedly. The channel is eligible again after we were seen
  as operator or after the user parted and joined it by hand.
- Accepted: a deliberate `/deop` that leaves us alone is undone after 5 s, and
  on such networks each connection costs one rejoin per matching channel; the
  per-server `regain_ops` switch (on by default) is the way out. The channel
  modes must be known (324) before acting, with no deadline. No reason is
  shown for a channel that was not tried.
- Everything else of D055 stays: the restored topic and `imnpst` flags, the
  `MAX_TOPIC` limit, the messages, `MAX_TRACKED`.

## D055 — Rejoin to regain operator status after a dropped connection (#444)

**Status:** Accepted (scope widened by D056)

- On by default, per server, switchable in the server settings. Only for a
  channel where the previous connection, which ended unexpectedly, had us as
  operator; a general "alone and opless" trigger was left out because it would
  also fire on a deliberate `/deop`.
- Parting an empty channel destroys it. The topic and the plain flags
  `imnpst` are put back by us once operator status returns (decided in the
  issue); keys, limits and the ban, exception, invite and reop lists are not
  restored, since restoring them from stale knowledge is riskier than the loss,
  and the setting's text says so.
- One attempt per loss, never retried; failure is shown and left to the user.
  `!` and `+` channels are excluded.
- Details in `architecture.md` (Regaining operator status).

## D054 — QUIT before sleep, reconnect on wake (#445)

**Status:** Accepted

- A connection that merely goes quiet in sleep leaves a stale session on the
  server until its ping timeout (nickname still taken, channel status lost
  on reconnect, "ping timeout" shown to others). So when the OS announces the
  sleep, every registered connection sends `QUIT` with the sleep message and
  is closed; ones still connecting are closed without QUIT.
- The ended sessions are marked `asleep`: not a manual disconnect, and no
  retry is scheduled. The wake-up reconnects exactly those (after the existing
  3 s `WAKE_GRACE`) and from then on the usual retry schedule applies. Servers
  the user disconnected are never touched.
- The sleep request travels in `QuitSignal` like `shutdown()`, with the
  message, not through the command or event queues: a full queue cannot hold
  back the QUIT or the 1 s limit. A session waiting to retry after an error
  is marked `asleep` too and its timer invalidated, so it cannot connect just
  before the machine sleeps; the wake-up reconnects it.
- A connection still registering is closed by the worker without QUIT (the same
  signal), so a full queue cannot keep its socket open either.
- macOS gets the notice from IOKit (`IORegisterForSystemPower`) on a dedicated
  thread with its own `CFRunLoop`, which waits for the UI (at most 1.5 s) and
  then calls `IOAllowPowerChange` once; the main thread is never blocked and no
  run loop is nested. `NSWorkspaceWillSleepNotification` is not used, as
  holding its handler does not delay the sleep. Wake stays
  `NSWorkspaceDidWakeNotification`.
- D053's check after waking stays as the fallback for sleeps the OS does not
  announce (forced sleep, Linux without logind).
- Accepted limitation: if the sleep is announced but the machine never reports
  a wake-up (a cancelled sleep without a wake notice on the platform), the
  servers stay disconnected until the user reconnects them; the clock check
  catches only real suspensions. Bouncers keep the upstream connection on
  `QUIT`, so they are unaffected.
- `quit_message` and `sleep_quit_message` are copied into the live
  connection's config when the settings are saved, not only at connect time,
  so a sleep message edited after connecting is the one sent. Saving does
  not validate them against the live connection, so each is checked (line
  breaks, encoding, wire length) before it is copied; an invalid one leaves
  the previous value in place, which keeps the reconnect's `validate()`
  passing. The sleep QUIT checks again just before sending and falls back to
  the normal message, then the default. A normal QUIT and the default PART
  still use the values from connect time (separate matter).
- Details in `architecture.md` (Waking from sleep).

## D053 — Verify connections after waking from sleep (#437)

**Status:** Accepted

- The OS's wake-up notice is the main signal (macOS, Windows, Linux with
  logind); a wall-clock jump check is the fallback for missed notices and for
  systems without logind. Both only say "woke up"; `ui/src/wake.rs` has no
  knowledge of connections.
- A woken connection is not presumed dead. `Connection::check_alive` sends an
  IRC `PING` and gives the server 15 s to send any line; only silence ends the
  connection, through `Event::Disconnected` and the existing reconnect
  schedule. No new reconnect or backoff state exists.
- Writes made during the check are held in the worker rather than written into
  a possibly dead link, and dropped with a server line if the link is lost.
- Details in `architecture.md` (Waking from sleep). Real wake-ups on macOS,
  Windows and Linux are confirmed by a person before merging (platform code),
  then on the rolling beta.

## D052 — Channel keys (`+k`) asked for on 475 and kept in the credential store (#414)

**Status:** Implemented

A JOIN is sent without a key unless one is saved. `475 ERR_BADCHANNELKEY` for a
JOIN this client sent (manual, or the auto-join of the connection) opens a
dialog naming the server and channel, with a masked key field and a "remember"
checkbox that starts on. Only 475 asks for a key; 471, 473 and 474 do not.
Pressing Join sends `JOIN #channel key`. The key is saved to the credential
store (`connection/<profile-id>/channel-key/<channel>`) only when the server
reports our own JOIN, never on sending or on a refusal; with the checkbox off
nothing is saved, and a saved key that had just been refused is deleted. A wrong
key reopens the dialog (no automatic retry); Cancel does nothing. A store that
cannot save is reported in the server log and the status line, and the JOIN
stands. Saved keys are used by manual joins (`/join #a,#b` without a key, the
menus and prompts) and, at connection start, by the auto-join list; `/join
#channel key` is sent as typed and never saved.

- **Tracking.** `ServerSession::key_joins` (at most 256) holds the JOINs
  awaiting an answer with the channel name as requested; names are compared
  under the CASEMAPPING in force when the answer arrives, so a mapping announced
  after registration (`001` then `005`) still matches. It is cleared on
  registration and disconnect, so a late answer of an old connection opens no
  dialog. Auto-join channels are added when the connection registers.
- **Off the UI thread.** Reading saved keys for a manual `/join`, saving or
  deleting a key after our JOIN, and refreshing the running connection's keys
  after the auto-join list changes run in background tasks; the results are
  applied only if the session still belongs to the same profile. The keys of a
  connection started from saved settings are read while its configuration is
  built, like the passwords it already reads there.
- **Remember choice.** A dialog reopened after a refused typed key keeps the
  checkbox the user chose for that attempt.
- **Storage name.** The channel is lowercased by ASCII only, because the
  server's case mapping is unknown when a connection starts; a server that folds
  more finds no saved key and asks for one.
- **No leaks.** The key is not in the settings file, logs, the transcript
  (`JOIN #c [redacted]`), `Debug` output or the input history (`/join` with a
  second word is never recorded). A key must be one IRC parameter: no
  whitespace, controls or commas, and not starting with `:`.
- **Limits.** Keys are not enumerable, so removing a server profile or switching
  the credential backend does not remove or move them; a missing key is
  asked for again. The auto-join manager has no key field.

## D051 — Channel modes in the window title (#413)

**Status:** Implemented

The title of a selected, joined channel starts with its mode letters,
`[+snt] #example (12) @ Network: topic — CayenChat`. Server and private
conversation titles are unchanged.

- **Queried by `irc-core` on every own JOIN** (auto-join, `/join`, menu,
  rejoin after reconnect) with `MODE <channel>`. `RPL_CHANNELMODEIS` (324)
  sets the channel's letters; later `MODE` lines change them. `Event::ChannelModes`
  carries the letters; a 324 is not a `ChannelActivity` and is not logged as a
  mode change. The existing key redaction of logs and the wire transcript is
  unchanged.
- **Letters only.** Arguments (a key, a limit) are never read, so they cannot
  reach the title whatever the server's `CHANMODES` says. Letters of `CHANMODES`
  list type A and of `PREFIX` (default `beI` and `ov`) are not channel flags
  and are left out; every other letter is shown in the order learned, once.
- **Unknown until stated.** Until the 324 arrives the title shows no modes
  (not `[+]`); a known empty set shows no brackets. A `MODE` before the 324 is
  not applied (no baseline), and history replay does not change the modes.
  Own JOIN, PART, KICK and disconnect discard the state. The state is kept per
  connection and folded channel name in `irc-core`, and per conversation
  (`Conversation::modes`) in the app, so equal names on two servers never mix.
- A reply is not awaited: a server that never answers just leaves the modes out.
- **Bounded.** At most 512 channels per connection are tracked; a join beyond
  that keeps unknown modes (no prefix) and is reported once as a diagnostic.
  Queries wait in a queue of at most that size, and at most four are
  unanswered at once: the next goes out on a `324`, a PART/KICK of the asked
  channel, or after 10 seconds of silence (a timer runs only while queries
  wait). A join burst therefore sends four queries, not one per join, however
  fast or slow the server is; a channel that never answers keeps unknown
  modes. A `CASEMAPPING`
  change folds the tracked names again (names that collide fall back to
  unknown), and queued or sent queries for names no longer tracked are dropped, so the queue stays within the table.

## D050 — Per-channel text encodings with `/charset` (#396)

**Status:** Implemented

The server profile's encoding is the server's default. A channel may override
it for the text of its messages, per direction (`decode` for received text,
`encode` for sent text), with the local command `/charset` (WeeChat syntax:
`/charset`, `/charset <enc>`, `/charset decode|encode <enc>`, `/charset reset`,
`/charset help`). The command is never sent to the server; it works only in a
channel tab. The override is stored in the profile (`channel_charsets`, keyed
by channel name, compared with the network's case mapping) and has no settings
page. There is no re-decoding of lines already shown.

- **Only the text changes.** The vendored codec (`irc-proto::charsets`) finds
  the target and the text of a `PRIVMSG`/`NOTICE` from the ASCII structure of
  the bytes and converts the text with the channel's encoding; tags, prefix,
  command and the target keep the server's encoding, so channel identity,
  JOIN targets, `!` safe channels and `#がが` on ISO-2022-JP do not change.
  CTCP ACTION is part of the text. Other lines that carry text (TOPIC, PART,
  KICK reasons, topic replies) use the server's encoding.
- **Wire bytes are checked as sent.** The 512-byte and relayed-prefix (#330)
  checks and the unrepresentable-character check encode the line with the same
  function the codec uses, so they see the real bytes.
- **A queued message keeps its encoding.** The connection handle fixes the
  channel's `encode` encoding in the message when it accepts it (length,
  relayed prefix, representability). The worker checks with that encoding and
  pins it to the exact line for the codec, which uses the pin once when it
  writes the line. A `/charset` change while messages wait therefore affects
  only later messages. `UTF8ONLY` outranks a pin: a message accepted in another
  encoding before the announcement is refused by the worker afterwards, a line
  already queued for the codec is skipped (not written), and the connection
  reports how many were not sent.
- **Configured names are not merged early.** The overrides handed to a
  connection are all kept until `CASEMAPPING` is announced, so names that only
  the default mapping equates (`#a[` / `#a{` under `ascii`) stay separate.
- **UTF8ONLY.** After `005 UTF8ONLY`, only UTF-8 overrides apply and `/charset`
  refuses others; a legacy server encoding still ends the connection.
- **One list.** `TextEncoding::ALL` (15 encodings: UTF-8, ISO-2022-JP,
  Shift_JIS, EUC-JP, EUC-KR, Windows-1251, KOI8-R, KOI8-U, IBM866, ISO-8859-5,
  x-mac-cyrillic, Windows-1254, Windows-1252, GBK, Big5) feeds the server
  settings, `/charset` and its help. Aliases (`CP949`, `Windows-949`, `CP866`,
  `MacCyrillic`, …) are input names only and map to the canonical one; an
  unknown name is an error, never UTF-8. `ISO-8859-1` is not offered or
  aliased: `encoding_rs` reads it as Windows-1252, which is a different table.
- **Limits.** At most 256 overrides per server; entries read from a settings
  file with an unusable name or no override are dropped.

## D049 — Direct input for ASCII-oriented fields (#379)

**Status:** Implemented (Windows, Wayland, macOS); not verified on real
desktops yet

While a field that mostly takes ASCII has focus, the window's IME is detached.
The IME mode is never read or written back: when focus moves to another field
the IME is attached again and the OS/IME returns to its own state, so the
OS-wide settings stay untouched.

- Fields: nickname, username, host, port, auto-join channels, SASL account and
  the color codes (settings and the color picker's `#RRGGBB`). Not: real name,
  display name, QUIT/PART messages, the chat input, passwords.
- It is an aid, not validation: pasted text, full-width characters and existing
  values are accepted as before.
- `TextInput::prefer_direct_input` marks a field (off by default, never set by
  `new_settings_field`). It answers the vendored GPUI `prefers_direct_input`;
  GPUI attaches the IME only when `accepts_text_input && !prefers_direct_input`.
  `accepts_text_input` has upstream's meaning ("is a text field"), so
  `prefers_direct_input` is CayenChat's own and the only difference from
  upstream. `Window::draw` asks after each frame and calls
  `PlatformWindow::update_ime_enabled` only when the answer changes.
- Windows: `ImmAssociateContextEx(hwnd, NULL, 0)` after completing a
  composition; `IACE_DEFAULT` to attach again. It runs after the frame, because
  completing the composition needs the window. Wayland: text-input
  disable/enable, applied only while that window has the keyboard focus (the
  text-input is shared by the seat; another window only records its wish, and
  `enable` on focus enter honors it). macOS: `allowedInputSourceLocales` of the
  window's own view limited to Roman input sources, as for a password field;
  applied at once, and a missing input context is not cached as done, so the
  next frame asks again. X11/XIM: no per-field switch; typing works as before.
- A composition still open when focus moves to such a field (Windows) is ended
  without delivering the commit: the handler registered by then belongs to the
  new field, and a password composition must never land there. The old field
  already shows the preedit text as marked text; `TextInput` drops the mark
  when it renders unfocused, so the text stays in the field it was typed in.

## D048 — Confirm/Cancel button order per OS (#378)

**Status:** Implemented

Left to right: Windows and Linux "confirm, Cancel"; macOS "Cancel, confirm".
The confirm button keeps its specific label ("Quit", "Delete", …).

- **Native prompts** (`Window::prompt`) always pass `[confirm, Cancel]` and
  the OS lays them out: the Windows task dialog shows custom buttons left to
  right, `NSAlert` puts the first button at the right edge, and the Linux
  fallback prompt stacks them top to bottom. Answer 0 is the confirming
  action. Read from the vendored GPUI sources; not run on Windows or macOS.
  The first button is also the default for Enter (Windows task dialog without
  `nDefaultButton`, AppKit's Return), so these prompts are only for actions
  where Enter on the confirm button is acceptable.
- **The quit question is not native.** Enter must keep cancelling, which a
  native prompt cannot do together with "confirm on the left" (the first
  button is both). It is drawn in the chat window (`quit_dialog.rs`) with
  `button_order::row`; the focus starts on Cancel, so Enter, Space and Escape
  cancel, and quitting takes a click or Tab then Enter. Covered by UI tests
  (keys, clicks, button positions for both orders). The box sits near the top
  of the window (like a browser dialog), is at most 360 px wide and as narrow
  as its text; a detail longer than one line wraps at a fixed width, because
  the shrink-to-fit box measured it as one line and let it overflow (#387).
- **Rows drawn by the app** (member and nickname prompts, upload token,
  avatar editor) list the confirm button first and are wrapped in
  `button_order::row`, which reverses the row on macOS only, so source and
  tab order are the same everywhere. GPUI is not changed.
- New confirm/Cancel pairs follow both rules.

## D047 — Toolbar (#368)

**Status:** Accepted

A toolbar of icon buttons sits below the menu bar on every OS (`toolbar.rs`),
shown by default; View → Show/Hide Toolbar flips it and saves
`Settings::show_toolbar` (added without a version change; older files read as
shown). The maintainer's decisions on #367 and #368: one button connects or
disconnects according to the state, reconnect is a button of its own, and join
and leave share one button whose icon follows the state; the rest use the first
candidates on #367.

- **Buttons:** connect (`plug-zap`) or, while connecting, connected or waiting
  for a retry, disconnect (`unplug`) in the same place; reconnect
  (`refresh-cw`), enabled once the selected server has been connected in this
  run and while it has no live connection | join or leave (`log-in` / `log-out`) | change topic
  (`message-square-text`, the second candidate chosen on #367), change mode
  (`sliders-horizontal`), change my nickname (`user-pen`) | next unread channel
  (`chevrons-down`) | WHOIS (`user-search`), upload image (`image-up`) | on
  Windows only, keep on top (`bring-to-front`, a toggle drawn pressed in while
  on, whose tooltip says on or off) and settings. The next notification
  (#369) is added with that action.
- **No new behavior:** a button calls the handler the menu item or shortcut
  calls (connect, disconnect, reconnect, settings, `Command::NextUnreadChannel`)
  or the helper the context menus use (join/part). WHOIS asks about the peer of
  a private conversation or the one member chosen in the member list. Keep on
  top saves `always_on_top` and applies it as the settings checkbox does; an
  open settings window shows the new value. Topic, mode and nickname (#370)
  open the shared member prompt in the middle of the window, like the log's
  right-click menu and the server menu: the topic starts from the current
  topic and the nickname from the current one, all selected; only the
  confirm sends `/topic`, `/mode` or the nickname change and a cancel sends
  nothing, so a mode string with a channel key is never echoed. In 1.0 the
  mode is a plain string input; checkbox mode editing is a separate issue.
- **Upload:** the button first checks the upload service as a drop does. When it
  is not configured or needs its account again, the same prompt with the way to
  the settings appears and no file dialog opens. Otherwise the dialog opens,
  offering only the extensions of `ImageFormat::FILE_EXTENSIONS` (the avatar
  picker too), and the file goes the dropped file's way (content check, size,
  confirmation). Where a dialog cannot filter (some Linux portals) that check
  refuses other files. A failed upload, a cancelled one, and a
  file refused before upload (not an image, over the size limit, unreadable,
  another upload running) (#481) are shown in a modal dialog with a Close
  button instead of a note in the draft row, so they cannot linger there; it
  stays until closed, and closing changes nothing else. An expired
  credential still offers to reconnect.
- **Unavailable:** a button that cannot act is dimmed and ignores clicks (for
  example Reconnect before the server was ever connected; join, leave and WHOIS off a channel
  or while not registered; topic and mode off a public channel; topic, mode
  and nickname while not registered; next unread with nothing unread).
- **Performance:** availability is the pure `toolbar::State`, recomputed when
  the chat window notifies; the bar is its own `AnyView::cached` view that
  redraws only when that value (or the theme) changes. Tooltips are built on
  hover, so a changed shortcut or language needs no redraw.
- **Tooltip:** the action's name and its current keys (navigation shortcuts as
  the user set them; Settings has its fixed key).
- **Icons:** Lucide SVGs per D039, compiled in through an `AssetSource` and
  drawn as a mask in the theme text color, so light and dark both work.
- **Layout:** the bar is 34 px high and is inside the window content, so the
  window size and pane sizes saved for #57 are unchanged; the panes just get
  that much less height, like with the menu bar.

## D046 — Optional confirmation before quitting (#349)

**Status:** Implemented

`Settings::confirm_quit` ("Confirm before quitting", Application page), off by
default and added without a version change, so older files read as off.

- **Asked:** only for the user's own quit requests: the `Quit` action (menu,
  Cmd/Ctrl+Q) and closing the main window (× or Alt+F4, via
  `on_window_should_close`). Closing the settings or WHOIS window is not a quit.
- **Never asked:** SIGTERM/SIGHUP/SIGINT (`signals.rs`) and session end from the
  OS call `cx.quit()` directly and do not pass through the question.
- **Before the answer nothing happens:** no QUIT, no layout save, no log flush.
  The window stays open, `quit_prompt_open` suppresses a second question, and
  Cancel restores the focus that was in the window. "Quit" calls `cx.quit()`,
  so `on_app_quit` runs the usual shutdown once.
- The question is drawn in the chat window (`quit_dialog.rs`, D048), not by
  `Window::prompt`: focus starts on Cancel, so Enter, Space and Esc cancel on
  every OS and the buttons follow the OS order.
- **Where it is asked:** the settings window has no Quit handler of its own;
  the application-wide handler (`request_quit_anywhere`) opens the question in
  the chat window and brings it to the front, so it is not hidden behind the
  settings window. The chat window lives until the application quits, so
  closing the settings window while the question is open cannot strand it.
- **macOS Dock "Quit":** reaches `applicationShouldTerminate:`, which asks
  `App::on_quit_request`; the answer "Quit" calls `cx.quit()`, which is not
  asked again. System shutdown/restart/log out, signals and `cx.quit()` are
  never asked. Written from the AppKit documentation; not run on macOS.

## D045 — Menu structure per OS (#337)

**Status:** Accepted

The maintainer approved this on the issue (2026-10-13). One `app_menus()`
definition serves the native macOS menus and the Windows/Linux in-window bar;
only the parts that differ are split with `cfg`.

- macOS: CayenChat (Settings… Cmd+,, Services, Quit Cmd+Q), Connection,
  Edit, View, Window (Minimize Cmd+M, Zoom). Window is marked with
  `SystemMenuType::Window` (a vendored GPUI patch) instead of matching the
  name "Window", so its title can be translated (ウインドウ) and AppKit still
  adds the window list and other system items.
- Windows/Linux: File (Settings… Ctrl+,, Quit Ctrl+Q), Connection, Edit, View.
  No application-named menu and no Window menu.
- "Connection settings…" is now "Settings…" everywhere; it opens the whole
  settings window and appears once.
- Connection offers Connect, Disconnect and Reconnect. Connect applies to a
  selected server not yet connected in this run (or when there is no server,
  where it opens settings); Reconnect to one that has been; Disconnect while a
  connection or scheduled retry exists. Availability is the registration of
  the action handler on the chat and settings windows, which both menus read
  through `is_action_available`, so the menus are not rebuilt on state changes.
- About and Help are out of scope.

## D044 — Opt-in conversation logs in files (#313)

**Status:** Accepted

The maintainer approved this specification on the issue (2026-10-13).

- **Settings** (`Settings::conversation_log`, added without a version change):
  `channels` and `private`, both off by default and independent, one setting
  for all servers, and `directory` (default `<data dir>/CayenChat/logs`).
  Turning a log off stops the writing; existing files are never deleted. The
  Application page says that private conversations end up in files. No
  secret, wire line, server-log line or replayed history (`replayed`, CHATHISTORY, bouncer
  playback) is written, nor a failed send or CTCP other than ACTION. A send is
  written once it is known to have gone out: at once without `echo-message`,
  when the echo confirms it with it.
- **Path.** `<folder>/<network>/channels/<channel>/<YYYY-MM-DD>.log` and
  `<folder>/<network>/private/<nick>/<YYYY-MM-DD>.log`. The network is the
  server's display name (else its host); servers that share it get the last
  eight characters of their profile ID (`Libera (9c1d04ab)`; generated IDs
  all start with `custom-`, so the start cannot tell them apart). Channel and nick are
  folded with the server's CASEMAPPING (private conversations use the same
  RFC 1459 key as the conversation list). The date is the local date of the
  line's time. Existing files are appended to.
- **Safe names** (`storage::chatlog::path_component`, the same on every OS):
  `/ \ : * ? " < > |`, controls, `%` and `~` become `%XX` (UTF-8 bytes); a name
  of only dots, a trailing dot or space and the first letter of a Windows
  device name (`CON`, `NUL`, `CONIN$`, `CONOUT$`, `COM1`…, `COM¹`…`COM³`)
  are escaped too; an empty name is
  `%00`; a component over 200 bytes is cut and ends in `~` and 16 hex digits
  of the SHA-256 of the whole name, so long names with the same start (and
  network names whose ID suffix would be cut) stay apart; no other name
  keeps a `~`, so a cut name never equals another name. The path is a join
  of escaped components below the folder, so a server cannot write outside
  it; the folder, its parents and the directories below it may be symbolic
  links the user made (only the log file itself is not followed).
  The file is opened without following a symbolic link (`O_NOFOLLOW`, and a
  check on other platforms) and without waiting on a pipe (`O_NONBLOCK`),
  `0600` in `0700` directories. Before the first write to a file the opened
  file must be a regular file and, on Unix, neither it nor the directories
  below the folder may be accessible to group or others; otherwise nothing
  is written and the failure says which mode to set (`chmod 600` / `700`).
- **Line format.** A new file starts with `# cayenchat-log 1`. Every other
  line has four tab-separated columns: time (ISO 8601, local offset,
  milliseconds; the server's `time` when negotiated, else receipt), kind,
  nickname, text. The text has IRC formatting removed, tabs turned into
  spaces and other control characters (newline included) into U+FFFD; the
  nickname's whitespace is replaced the same way, so a line always has four
  columns. Readers skip lines starting with `#` and kinds they do not know.

  | Kind | Nick (3rd) | Text (4th) |
  | --- | --- | --- |
  | `msg` | speaker | message |
  | `action` | speaker | the `/me` text |
  | `notice` | sender | text |
  | `join` | who joined | empty |
  | `part` | who left | reason or empty |
  | `quit` | who quit | reason or empty |
  | `nick` | old nick | new nick |
  | `kick` | who kicked | `<target> <reason>` (just the target without a reason) |
  | `topic` | who changed it | the new topic or empty |
  | `mode` | who changed it (`server` when the server did) | `+o <nick>` or `-o <nick>`; likewise `v` |

  Joins, parts, kicks, quits and nick changes are logged per channel the
  client has; a quit or nick change of a peer is logged in their private
  conversation. Of live channel `MODE` changes (#357) only `o` and `v`
  changes are logged, one `mode` line per changed nick; a `MODE` with several
  of them gives several lines. Every other mode and its argument (keys, bans,
  exceptions, invites, limits, flags) is left out, so no key or host mask
  reaches a file. The arguments are paired with the modes again in irc-core
  (the parser's table is wrong for `-l`); a `MODE` with a letter whose
  argument count is not universal (unknown modes, and `h`, `a`, `q`, whose
  meaning depends on the server's `PREFIX`), a missing or a surplus argument
  is not logged at all, which is an accepted limitation. Server-originated
  live `MODE` lines are logged with actor `server`; `MODE` inside a history
  batch, channel mode queries and replies (`324`) are not logged. Refused
  joins are not logged.
- **Writing.** `ChatLogger` (storage) has one thread. `log` is `try_send`
  into a 4096-record queue; a full queue drops the record and counts it
  (`Status::dropped`). The thread keeps up to 32 files open, flushes and
  `sync_data`s every second and at exit, so a crash loses at most the last
  second; only files written to since are synced. Nothing on the UI thread
  waits for the disk: turning a log off or moving the folder `retire`s the
  logger (the thread drains its queue, flushes and releases the folder on its
  own). The next logger on the same folder waits for it in its own thread
  for at most 10 seconds before taking the lock, and says so if that fails;
  loggers on other folders do not wait, and a waiting logger that is retired
  with nothing queued ends at once, so a stuck disk cannot pile up threads
  (the UI remembers at most 8 stopped loggers). Only quitting waits, for at
  most 2 seconds. A record is written only if
  its kind of log is on when it is written, and a held own message of a kind
  that is turned off is dropped.
- **Failure and sharing.** The first I/O error is kept (`Status::failure`)
  and shown once as a line in every server tab and permanently on the
  Application page; the file is reopened by the next record. The thread
  calls a `Notify` callback when it first fails or finds the folder locked,
  which wakes the chat window, so the notice does not wait for an IRC event.
  The thread takes an exclusive lock on `<folder>/.cayenchat-log.lock`
  (opened for writing, not append-only: Windows cannot lock an append-only
  handle, a regular file only, never waiting on a pipe; `File::try_lock`, so
  `storage` needs Rust 1.89): a second
  CayenChat on the same folder writes nothing and says so
  (`Status::locked`).
- **Own messages.** `OutgoingAccepted.action` carries the text of an ACTION
  (`/me`) so it is logged as `action` and not guessed from the `* …` shown;
  `OutgoingConfirmed.text`, when the server changed the text, replaces the
  held text, and `OutgoingConfirmed.action` does so for an ACTION.
- **Not in scope.** Reading logs back, a viewer, search, SQLite, migration
  of other clients' logs, compatibility with their file names or formats.
- **Cost.** Off, a line costs one `Option` test and a flag test. On, the UI
  thread builds one `Record` and queues it. See `performance.md`.

## D043 — Decoding HTML numeric character references for display (#320)

**Status:** Accepted

- Startup & Behavior option `appearance.decode_char_refs` (on by default since #523; it was off
  when added, and a saved explicit `false` stays off):
  `&#x1F352;` and `&#127826;` in received text are drawn as the characters.
  Display only: the stored text, search, sending and the wire are unchanged.
  Named references (`&amp;`) and tags are never interpreted.
- Only well-formed references (`;` terminated, at most 8 digits) naming a
  valid scalar value are decoded. Controls, line and paragraph separators,
  bidirectional controls, BOM, noncharacters, surrogates and out of range
  values stay as written, as do references inside URLs. At most 64 runs per
  message are decoded. Adjacent references form one run.
- It reuses the presentation mapping of compact URLs (`compact_urls.rs`):
  one drawn text with spans mapping drawn and original byte positions, so
  links, highlights and selections keep working on original offsets.
- A run has a hover bubble with the original text and one "copy original"
  button. It has no decoration of its own: it is drawn exactly like the same
  characters typed directly, with no background (#524 dropped the neutral
  background #320 gave it, which showed as a grey box behind emoji). Selection,
  search matches and keyword highlights still shade it. There
  is no button for the converted characters; selecting and copying copies the
  characters shown. Short URL bubbles and these bubbles share the hover
  machinery; they never overlap because references inside URLs are not decoded.
- The channel log and the combined log both draw it (the combined log is
  text-only, without the bubble). Notifications and search use the original.

## D042 — MPL-2.0 core, GPL-3.0 user interface (#275)

**Status:** Accepted

The GPUI-free crates `model`, `irc-core`, `storage`, `media`, `upload` and
`app` are licensed under MPL-2.0. The `ui` crate (the GPUI front end) stays
GPL-3.0-only, with `crates/ui/src/input.rs` under Apache-2.0. Everything
else in the repository that no crate's own license covers (scripts,
specifications, assets, CI) also stays GPL-3.0-only. Vendored crates keep
their upstream licenses.

Rationale: other people must be able to build and publish their own user
interfaces on the core, including through app stores whose terms conflict
with the GPL. LGPL was rejected for this: LGPL-3.0 requires installation
information on user products such as phones, and LGPL-2.1 forbids further
restrictions, as GPLv2 does. MPL-2.0 keeps file-level copyleft, so changes
to core files are still shared. MPL-2.0 is applied without the
"Incompatible With Secondary Licenses" notice, so the GPL front end can
combine with the core.

Consequences:

- Core crates must not depend on `ui` or on GPL-only code, and code from
  elsewhere is not copied into them. `cargo deny check licenses` enforces the
  dependency side.
- Moving code from `ui` into the core relicenses it under MPL-2.0. That is
  possible because contributions to GPL parts are also granted under
  MPL-2.0 (`CONTRIBUTING.md`); at the time of this decision, the maintainer
  holds the rights to all existing code.
- Releases up to 0.9.x remain GPL-3.0-only as published.

## D041 — Reordering the channel tree (#237)

Status: implemented.

The tree order is also the order of channel navigation and of the numbered
channel shortcuts (D009), so the user can change it.

- **Scope.** Channels move only within their own server, and private
  conversations only among themselves (they stay after the channels). Nothing
  moves between servers. Servers are reordered one place at a time with
  Move up / Move down in the server's context menu (no dragging: the
  distance is too large); this reorders the saved profiles, so it is also the
  order in the settings window.
- **Drag.** Pressing a channel row and moving more than GPUI's drag threshold
  starts a drag (a plain click still selects); there is no long-press mode.
  While dragging, every row shows the digit that numbered shortcut reaches
  (1–9, then 0 for the tenth; none beyond) and the row under the pointer
  shows an insertion line. Dropping puts the dragged row into the target's
  place. The same moves (up, down, to top, to bottom) are in the channel and
  private-conversation context menu, so dragging is not the only way.
- **Saving.** The order is in its own file, `channel-order.json` beside the
  settings (as `window.json`, D037: it never races the settings autosave and
  a damaged file reads as nothing saved). It maps a server profile ID to
  channel names, lowercased ASCII, and keeps the names of parted channels so
  a channel joined again returns to its place; they stay anchored after the
  channel that preceded them. Channels it does not list go to the end in
  join order. The file is refused unparsed beyond 1 MiB and bounded in
  entries on load, the remembered names per server are capped in memory too,
  and entries of removed profiles are dropped on the next save. Tests write
  nothing without an explicit path. A joining channel finds its place by a
  rank table and a binary search, not by sorting the group, so joining a
  thousand channels stays linear.
- **Settings window.** A server move is written to the settings file at
  once; a settings window with unsaved edits takes over the file's server
  order (keeping its other edits) before it saves, so it cannot put the old
  order back.
- **Tree list.** Row keys follow the chosen order and need not ascend, so the
  tree syncs with `LogList::sync_unordered`, which replaces only the span
  that changed and keeps the top row.
- **Auto-join.** The auto-join list and the order of JOIN commands are not
  affected.
- **Not done.** Reordering with shortcut keys (the context menu is the
  non-drag way), and moving servers by dragging.

## D040 — Standard keys in the channel tree and member list

Status: implemented (#132).

The channel tree and the member list take focus when clicked (`tree_focus`,
`members_focus`) and then understand Up/Down (previous/next), Home/End,
PageUp/PageDown (`ui::list_keys`); the tree also has Left/Right (to the server
of a channel / to a server's first channel) and Enter (open the selected row
and return to the draft). The keys are handled by the focused list only, never
as shortcuts, so the draft keeps every character and its Emacs-style editing.
Modified keys are ignored. Whenever the selection changes by any route (the
channel shortcuts, tree keys, a click), the next render of the tree scrolls the
new row into view with the minimum movement (`tree_revealed`, #477); a row
that is not laid out yet is approached in steps over a few frames, since the list
only knows the height of laid-out rows; a row
already visible and manual scrolling are left alone. Letters (K/J, N/P) are not aliases: K/J were
dropped in #289 because a key handler that consumes a letter keeps the first
romaji of an IME composition from reaching the input method (macOS). In the tree, moving selects as
it goes and focus stays on the tree, and a click on a row now leaves focus on
the tree instead of the draft (Enter or a click in the draft returns). In the
member list the keys move the one chosen member. Not done: expand/collapse
(the tree has no collapsed state), Shift-extended member ranges, type-to-find,
and the global server/channel keys without arrows, which are separate issues.

The chosen members are dropped when the member list loses focus (a click
elsewhere in the window, or the window becoming inactive), because a choice can
be acted on all at once and must not linger unseen (#294). The log's text
selection and the channel tree's active row are unchanged.

In a channel the member list shows our own row first (#318), matched by the
network's current nickname under RFC 1459 case mapping with the role prefix
ignored and the prefix kept in the row. The others keep the `sorted_members`
order and the roster itself is not reordered: `member_selection::Participants`
borrows it and maps display index to roster index, so drawing copies nothing
and rows, clicks, ranges and keyboard movement share one order. When we are not
listed, nothing is added.

Typing while the main log, the member list or the channel tree has focus (after
a click that started a selection)
moves focus to the draft and types the character there (#289), so input is never
accepted by a pane that cannot hold it. Keys with Ctrl/Alt/Super keep their
meaning (Ctrl+C copies the selection). The log has no key handler for text:
plain characters and IME composition both reach the window as input-method
calls, so each of these panes registers an input handler (`EntityInputHandler` on
`ChatWindow`) that stands in for the selected draft. It forwards every call
(queries, replace, mark, unmark, candidate position) to the draft, so ranges and
the composition state are the draft's, and moves focus there when text changes.
The candidate window position is computed from the draft's own painted bounds,
not the forwarding pane's.
Handling characters in a key-down listener is avoided because the platform
(macOS) would then skip the input method for the first key.

## D039 — UI icons come from Lucide, vendored one SVG at a time

**Status:** Accepted

New small UI icons (link, menu, status marks) are taken from
[Lucide](https://lucide.dev/) (ISC License) unless Lucide has nothing suitable;
only then another source or a hand-drawn icon is considered.

- Only the SVG files in use are copied into `assets/icons/ui/<lucide-name>.svg`,
  unchanged apart from what theming needs. No icon library is a dependency and
  no icons are added ahead of use. One meaning gets one icon.
- They are drawn through GPUI's SVG support (`ImageFormat::Svg`); stroke colour
  must work in both light and dark themes.
- Every vendored icon is listed in `assets/icons/ui/README.md` (name, upstream
  URL, Lucide version) and the upstream LICENSE (ISC plus the MIT text for
  Feather-derived icons) is kept verbatim in `licenses/LUCIDE-ISC.txt`,
  referenced from `THIRD_PARTY_NOTICES.md`. This is separate from the app icon
  `assets/icons/cayenchat.svg` and from CayenChat's own GPL-3.0-only licence.

## D038 — Settings: category list on the left, one reset per tab

Status: implemented (#134, #135).

The horizontal tabs wrapped onto two rows once there were nine of them, which
broke the joined tab/body border. The settings window now has a fixed-width
category list on the left (Experimental apart at the bottom) and the selected
category's page on the right, scrolling by itself; the window opens wider
(900 px, minimum 760 px). `SettingsTab` and the pages are unchanged. The list
takes focus (a tab stop) when a category is clicked; Up/Down move through it
and select as they go, except while a shortcut key is being recorded.

Every page ends with a "Restore Defaults" button at the bottom right, in the
same place on every tab (Japanese: 「すべて元に戻す」, the wording of the old
Shortcuts button, as the owner asked on #135). The rule is one tab, one reset
over exactly what the tab shows; the button is disabled while all of it
already has its default (`settings_reset.rs`, comparing the form's snapshot
with `Settings::default()`). A reset is saved and applied by the ordinary
autosave. It always asks first ("everything on this settings page returns to
its default"), because the button sits where a click by mistake is easy. It
is drawn in the warning color (text and border) as a destructive action, and
dimmed instead when disabled.

- **Startup & Behavior** (Japanese: 起動と動作; #192): how the app itself
  behaves, right after Connection (never first, so it is not called General).
  It holds the language, login startup (#153), restoring the window layout,
  the Linux display (Linux only) and menu bar auto-hide (Windows and Linux
  only), and (#221) the channel log's image link previews, user avatars,
  Reiwa-mode layout and compact long URLs, which are behavior switches
  rather than looks (they are still stored in `appearance`). Its reset
  covers all of these except login startup: that is the
  system's registration, not a value in `settings.json`, so a reset never
  changes it and the button's disabled state ignores it; the page's intro
  says so. The language is reset too (to System, which switches the UI
  language at once), as the owner decided on #192. "Start automatically
  when the app starts" is per server and stays on Connection.
- **Appearance** (theme, colors, alternating rows, fonts): the saved color palette is the user's own data
  and is kept.
- **Keyboard**: channel-number modifier (Windows/Linux), text key theme
  (Linux), and every shortcut override cleared. The per-action "Reset" stays;
  the old Shortcuts page's "Reset all" is folded into this button (#591).
- A setting moved between pages moves between resets with it;
  `each_page_reset_covers_its_own_items_and_no_others` fails when a page's
  reset leaves another page's items changed or its own not default.
- **Notifications**, **Experimental**: their defaults (keywords emptied,
  logging off and no file).
- **IRCv3**: the selected server's opt-ins only (it is per server, and the
  page names it); peer avatar sharing stops with it. Our own avatar URL is
  data, not an option, and stays.
- **Image Upload**: provider None. The account token is a credential and is
  kept; Disconnect stays its own button.
- **Connection** has a single ordinary push button at the bottom right instead
  of separate Connect and Disconnect buttons (owner's request on #135, a
  layout matter; #178 corrected an over-literal toggle switch). It reads
  "Connect" while the edited server is not connected and "Disconnect" while it
  is connected, connecting or waiting to retry, in the same place. Pressing
  Connect connects as before (and closes the window); pressing Disconnect
  disconnects. No switch or checkbox is used. Back stays at the left.
- **Connection** and **Credentials** have no reset: the former holds the user's
  servers, the latter moves saved secrets between stores.

## D037 — Window position, size and pane sizes are remembered

Status: implemented (issues #57 and #64).

The main window reopens where it was left, with its pane sizes, unless the
user switches it off ("Restore window position and size at startup",
`restore_window_layout`, on by default, added without a settings version
change).

- **Own file.** The layout is in `window.json` beside `settings.json` (a test
  build's own directory), not in the settings: it is written on every move or
  resize, so keeping it apart means it never races the settings window's
  autosave, and a damaged or unknown-version file reads as "nothing saved"
  instead of blocking startup. It holds the window's normal rectangle in
  logical pixels in desktop coordinates (negative on a display left of or
  above the primary), whether it was maximized, the right column's width, the
  member list's height and the logs' split. Writes go through a temporary
  file and a rename.
- **Displays.** At startup the saved rectangle is used only where it still
  overlaps a connected display: the display showing most of it decides, the
  window is shrunk to that display and moved onto it as far as it fits, and
  never below the minimum window size. With no overlap (a monitor unplugged,
  a resolution change) the window opens in its usual centered place.
  On Windows that display is also passed as the window's display: GPUI
  otherwise creates the window on the primary display and replaces bounds
  off it with a centered default size, so a window left on another monitor
  came back on the primary (#125). macOS and X11 report every display at the
  origin, so the display is not named there. With displays at different
  scale factors the position may still be off, because GPUI converts the
  bounds with the scale of the display the window is first created on; this
  was not verified on Windows.
- **When.** Moves and resizes, pane drags, closing the window and quitting the
  application (Cmd+Q does not ask the window to close) write it, after a 0.5 s
  pause for moves and drags. The window's bounds are noted as they change, so
  quitting can write without a window at hand. Turning the setting on writes
  the current layout at once; turning it off stops writing and reading but
  keeps the file.
- **Trying it.** A test build starts empty on every launch, so it cannot show
  a restore. `CAYENCHAT_TEST_DIR=<absolute path>` makes it use and keep that
  directory (settings, credentials file and `window.json`), started from the
  shell so that it inherits the variable (`open` does not pass it on), e.g.
  `CAYENCHAT_TEST_DIR=$HOME/cayenchat-preview "CayenChat Test.app/Contents/MacOS/cayenchat"`.
  The directory is created readable only by the user; one that already exists
  is refused (on Unix) when other users can read it, because the test build's
  credentials file lives there (`chmod 700` it).
- **Not remembered.** Full screen returns to the normal rectangle. The work
  area (menu bar, taskbar) is not known to GPUI, so a window may start under
  the taskbar edge the OS then adjusts.
- Tests never write the real file: the path is supplied at startup and
  without one nothing is written.

## D036 — Server-confirmed sending: echo-message and labeled-response

Status: implemented (IRCv3 standard capabilities, one opt-in).

Specifications read: `echo-message` (the server MUST send our PRIVMSG/NOTICE
back with the final text; clients may show a local line first and replace it)
and `labeled-response` (needs `batch` and `message-tags`; a `label` of at most
64 bytes, not reused until answered; single replies carry the label, several
are a batch tagged on its start, no-reply commands get a labeled `ACK`; a
message to ourselves is labeled only on the echo).

- **One preference.** "Server-confirmed sending" (`Ircv3Preferences::
  confirmed_sending`, per server, default off, no version change) requests
  `echo-message`, `server-time`, and on UTF-8 connections `message-tags`,
  `batch` and `labeled-response` (the last only after both are enabled;
  dropped when `batch` goes). Legacy encodings get the echo alone: tags are
  UTF-8. Off: registration and sending are unchanged.
- **Optimistic line stays.** `Outgoing::Message` still produces
  `Event::OutgoingAccepted` at once; with `echo-message` it carries a
  `local_id`, and the application remembers (conversation, sequence) for it
  (`ServerSession::pending_sends`).
- **Matching** (`irc-core::echo::Echoes`): with labels, the echo, `ACK`,
  4xx/5xx numeric or `FAIL` carrying our label, or any of them inside a
  batch whose start carries it (of type `labeled-response` or, as the
  specification allows, an existing type covering the whole reply). Labels are `c` plus a per-connection counter in
  base 36, so one is never reused. An unlabeled own-nick message is another
  client's and is shown as before. Without labels: the oldest pending message
  to the same casemapped target with identical text and kind; a message the
  server rewrote cannot be recognized and appears as a new line.
- **`labeled-response` coming and going** (`CAP DEL`/`NEW` on a live
  connection, `echo-message` staying): a line carrying a label is always
  matched, so late replies to earlier labeled requests still settle them.
  While the capability is on, an unlabeled echo matches only messages sent
  without a label. Once it is off, it matches every pending message,
  including those sent with a label: they are converted to the target-and-text
  match, so their echoes are not shown twice and they are not marked failed
  later. A labeled message the server rewrote cannot be matched after the
  withdrawal and is reported unconfirmed when its 60 s expire.
- **Confirmation** (`Event::OutgoingConfirmed`): the final text replaces the
  local one only when it changed and the local line is the text itself (not a
  CTCP ACTION or a redacted service secret); msgid and time are stored and the
  message goes into the duplicate filter, so later history repeating it is
  dropped.
- **Failure.** `Event::OutgoingFailed` (labeled error, `FAIL`, no answer in
  60 s, the capability withdrawn) or a connection that ends with the message
  pending sets `Message::delivery_failed` (drawn in the warning color) and,
  for a server reply, adds a server-log line. A later confirmation clears it.
- **Bounds.** At most 32 pending messages per connection (more are sent but
  not tracked), 16 open labeled batches, 64 pending ids in the application.
  A timer runs only while messages are pending; their failure is reported at
  60 seconds even if the server sends no further lines.
- **Self-messages.** With labels only the labeled copy is the echo; the
  delivered unlabeled copy still appears as a message from us, as it did
  before (two lines, literally what the server sent).
- **Not done.** No delivery marker for unconfirmed-but-not-failed lines
  beyond the failure colour; `TAGMSG` echo; label use for other commands;
  no live soju/Ergo check.

## D035 — Live account tracking: account-notify, extended-join, WHOX

Status: implemented (IRCv3 standard capabilities; opt-in because of the WHOX
traffic).

- **One preference.** "User accounts" (`Ircv3Preferences::accounts`, per
  server, default off, no version change) requests `account-notify` and
  `extended-join` when offered. Nothing else changes with it off.
- **State.** `irc-core::accounts::Accounts` in the connection's worker:
  casemapped nickname → account, real name and the channels shared with us;
  an entry exists only for a user we share a channel with and know something
  about. At most 4096 users, real names cut at 256 bytes; a reconnect starts
  empty. The application mirrors it from `Event::UserAccount` /
  `UserAccountForgotten` (per network session, cleared on disconnect) and uses
  it only to complete WHOIS.
- **Lifecycle.** extended-join JOIN and `ACCOUNT` (login, change, logout `*`)
  update it; PART, KICK, QUIT drop users who share no channel; NICK moves the
  entry (forgotten + reported under the new name); our own PART or KICK
  removes the channel from everyone; a republished member list removes users
  who left unseen. Same nickname on two servers: separate sessions. An entry
  with neither an account nor a real name (for example after `ACCOUNT *`) is
  dropped, so it does not use one of the 4096 slots.
- **Late WHOX replies.** The reply describes the channel as it was when the
  query went out. Nicknames that leave it meanwhile (PART, KICK, QUIT, or a
  nick change away from the name) are remembered for that one query and their
  354 lines are ignored, whether or not the first NAMES was published yet; a
  rejoin, or the new name after a nick change, is accepted. The set lives and
  dies with the query.
- **Initial state.** account-notify only reports changes. After our own JOIN,
  with `WHOX` in ISUPPORT (the token may carry a value, `WHOX=...`; it is
  recognized by name), one `WHO #chan %tnar,<token>` at a time (rolling
  token 1–999, at most 64 queued, 15 s without an end-of-WHO drops it, no
  timer: checked on the next line) fills account (`0` = none) and real name of
  those present. The reply (354/315 of our token) is not a server line. The
  plain WHO of the specification has no account without WHOX, so servers
  without WHOX only learn accounts from later `ACCOUNT`/JOIN lines. This
  channel-wide WHO is why the feature is opt-in (D025's "no channel-wide WHO"
  concerned avatar lookups).
- **Private conversations.** Still keyed by casemapped nickname per network
  and session; this state does not feed them. Nick-based until a redesign.
- **Not done.** No display beyond WHOIS; `away-notify`; realname from
  extended-join is not offered to CTCP AVATAR marks.

## D034 — CHATHISTORY TARGETS and missed direct messages

Status: implemented (draft/chathistory, experimental, same per-server opt-in).

Specification (current draft): `CHATHISTORY TARGETS <timestamp=…>
<timestamp=…> <limit>`; the reply is a `draft/chathistory-targets` batch of
`CHATHISTORY TARGETS <nickname | channel> <timestamp>` lines, matched on each
target's latest message; timestamps only.

- **Trigger.** Once per connection, when registration completes with
  chathistory negotiated. No polling, no user action. `from` is when the
  session's connection was first lost since it last registered (kept in the
  UI's `ServerSession`), at most a week back, a day when unknown (the first
  connection of a run); `to` is now plus 5 minutes.
- **Bounds.** Limit 16 (lowered by the server's `CHATHISTORY`), at most 64
  reply entries kept, at most 16 peers asked, the shared 64-request queue and
  one outstanding request at a time. A failed or unanswered TARGETS request
  ends silently with a diagnostic note.
- **Selection.** Nicknames only, without `!`/`@`, once per casemapped name,
  newest first. Channels are ignored: joined channels ask on join, and a
  channel the bouncer has but we have not joined stays undiscovered.
- **History of a peer.** `LATEST <nick> * 50`; reply lines are kept when the
  peer wrote them or we wrote to the peer. The peer's conversation is created
  when the request is sent (it exists because the server named it), keyed by
  the IRC-casemapped nickname per network, so the same nickname on two
  servers stays separate.
- **Quiet.** Discovery and its history use provenance `Requested`: no unread,
  highlight or notification.
- **Placement.** Same reservation as channel history (D027, D031): the
  block sits where the request was made. A known conversation that already
  received a live line meanwhile shows the missed lines after it.
- **Not done.** Private conversations have no resume points (repeat
  `LATEST *` only); no account-aware identity; no live soju/Ergo check.

## D033 — account-tag

Status: implemented.

`account-tag` (IRCv3 standard) is requested whenever the CAP negotiation runs
for another reason (SASL or an opt-in extension) and the server offers it,
without its own switch and never on a legacy encoding (tag values are UTF-8;
the same reason `message-tags` is not requested there). It never starts a
negotiation itself.

- **What is kept.** Only the `account` tag, as `Event::ChannelMessage` /
  `PrivateMessage` / `HistoryMessage::account` and then
  `MessageMeta::account` / `Message::account` (`model::ServicesAccount`:
  non-empty, no whitespace or controls, at most 128 bytes; `*` and empty are
  "none"). No other tag is stored. Our own echoes carry none.
- **Scope.** An account names an identity only inside the network of the
  conversation that holds the message; it is never compared across networks
  and no map from nickname to account exists, so nothing can go stale.
- **Cost.** One thin pointer per retained message (`Message` grew from 96 to
  104 bytes; the name is on the heap only when present).
- **Display.** None in log lines. WHOIS already shows the services account
  (330). History and replayed lines carry the account their server tagged.
- **Not done.** Account changes between messages are visible only as
  different values on later messages; live tracking is account-notify.

## D032 — Real name setting and IRCv3 `setname`

Status: implemented.

Each server profile has a `Real name` (`ServerProfile::realname`, added
without a settings version change; files without it read as empty). Empty or
blank means the built-in default `CayenChat`, so existing profiles advertise
the same identity as before. It is separate from the nickname, the `USER`
username (ident) and the SASL account.

- **Clean separation.** The setting and `ConnectionConfig::realname` hold
  only what the user typed. KVIrc's avatar mark (D025) is added on the wire
  by `ConnectionConfig::wire_realname` for `USER` and by the worker for
  `SETNAME`; the mark of a connection is fixed for its lifetime, so `SETNAME`
  keeps the registered mark and the avatar tab's "reconnect to update the
  mark" behavior is unchanged. A confirmed `SETNAME` is shown without the
  mark.
- **Capability.** `setname` (IRCv3 standard) is requested whenever the CAP
  negotiation runs for another reason (SASL or any opt-in extension) and the
  server offers it. It never starts a negotiation on its own, and gets no
  switch: it changes nothing until the user edits the real name.
- **Change while connected.** The normal settings autosave applies the new
  value: `SETNAME` is sent when the connection has `setname`; the outcome is
  one server line (changed / not supported, "used from the next connection" /
  rejected with the server's `FAIL SETNAME` description). There is no second
  setting. At most four requests wait for an answer; the counter is the only
  new state. Values with line breaks or that exceed the 512-byte line in the
  connection's encoding are refused before sending. Length and content limits
  beyond that are the server's (`FAIL SETNAME INVALID_REALNAME`).
- **Not tracked.** Other users' `SETNAME` messages are consumed silently; the
  remote realname is not stored (workstream for extended-join/WHOIS).

## Text input scrolling

`TextInput` is a single line drawn by GPUI. When the text is wider than the
box it scrolls horizontally (`scroll_x`, computed in `prepaint`) so the caret
stays visible, i.e. the end of what is being typed; painting is clipped to the
box, and mouse and IME positions add `scroll_x`.

In the dark theme the caret uses the text color in every text input, including
the native settings-style ones, because blue and the OS theme's caret color are
hard to tell from the dark input backgrounds (issue #219). In the light theme
the caret is blue, or the OS theme's caret color for native settings-style
inputs.
