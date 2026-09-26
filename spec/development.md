# Development

## General workflow

Before making changes, read the relevant files under `spec/` rather than loading every project document automatically.

Keep changes small and reversible. Avoid unrelated refactors.

## Development environment

Before starting implementation, inspect the available development environment and determine whether the current machine has the tools required to build and run the project.

Project-local dependencies managed by Cargo may be fetched normally.

Do not silently install system-wide development tools or modify the user's machine configuration. This includes tools installed through package managers, platform SDKs, compiler toolchains, system libraries, or similar environment-level dependencies.

If something required is missing:

1. explain in Japanese what is missing and why it is required;
2. give the user the appropriate installation command or procedure;
3. distinguish required dependencies from optional/recommended ones;
4. continue with work that is not blocked by the missing dependency;
5. clearly state when build or runtime verification is blocked.

Do not use `sudo`, Homebrew, winget, Chocolatey, rustup toolchain changes, or similar system-level installation commands without explicit user approval.

Do not claim that macOS or Windows support has been verified unless the application was actually built or tested on that platform.

## Validation

For Rust code changes, run the applicable checks:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

For UI work, also build and run the desktop application on the currently available platform when practical.

Do not claim cross-platform verification unless both platforms were actually tested.

## Documentation

Update only the relevant specification files when behavior or architecture changes:

- `project.md` for product direction/scope
- `architecture.md` for boundaries and data/event flow
- `decisions.md` for lasting technical choices
- `feature-parity.md` for implementation status
- `development.md` for build/test workflow

## Source language

Use English for source identifiers and normally for comments and technical documentation.

User-facing discussion and progress reports should be in Japanese as required by `AGENTS.md`.

## Build and run

```sh
cargo run --locked -p cayenchat-ui
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked
cargo test --workspace --locked
cargo build --locked -p cayenchat-ui
```

Commit Cargo.lock when committing the application. No rust-toolchain override is used:
GPUI's upstream guidance requests current stable Rust; the tested compiler is 1.95.0
(edition 2024). Older compiler support has not been established. Rustfmt and Clippy
must be available for the corresponding checks. Rustup itself is optional when a
working toolchain is already provided by another installation.

### macOS

Use full Xcode with macOS SDK and command-line tools selected via `xcode-select`.
GPUI uses Metal and needs a compatible GPU. `font-kit` is explicitly enabled for
actual glyph rendering: disabling it selects a no-op text backend on macOS.

The workspace uses GPUI's `runtime_shaders` feature, so the separate offline Metal
Toolchain is **optional for this configuration**. If changing to precompiled shaders,
it becomes required. With Xcode 26, install it manually only after approval:

```sh
xcodebuild -downloadComponent MetalToolchain
```

This command modifies the system development environment; the project never runs it
automatically. If Xcode itself is absent, install it via the App Store, launch it and
install the macOS components, then select its developer directory. See the
[GPUI prerequisites](https://docs.rs/crate/gpui/0.2.2) and
[upstream macOS guidance](https://zed.dev/docs/development/macos).

An optional local bundle makes the application discoverable to macOS UI tools:

```sh
sh scripts/bundle-macos.sh
open target/CayenChat.app
```

The helper only builds and copies into ignored `target/`. It neither installs nor
signs/notarizes a distributable app. The normal `cargo run` entry point is portable.
The bundle includes `crates/ui/resources/macos/CayenChat.icns`; Windows embeds
`crates/ui/resources/windows/cayenchat.ico`. Both are derived from `icon.png` and
checked in so normal builds need no image tools. When changing the artwork, run
`python3 scripts/generate-icons.py` with Pillow installed, then rebuild the app.
The macOS beta workflow signs the completed bundle ad hoc and verifies its
resource seal before archiving. This is sufficient to validate bundle integrity
but does not provide Developer ID signing or notarization for Gatekeeper.
GitHub workflows default to a read-only token and check out without persisted
credentials; only the beta `publish` job gets `contents: write` to move the tag and
upload the release. Actions are pinned to commit SHAs (with the version in a
comment) and Dependabot proposes pin updates weekly.
The project license is GPL-3.0-only; the adapted GPUI input file retains Apache-2.0.
Review `THIRD_PARTY_NOTICES.md` and dependency licenses before distributing binaries.

### Windows (not yet verified)

Use stable Rust for an MSVC target, Visual Studio or Build Tools with Desktop
development with C++, matching MSVC/Spectre libraries, and the Windows SDK. Upstream
Zed documents SDK 10.0.20348.0 or later and CMake; its complete application requirements
are broader than this app. Validate the actual standalone GPUI build on a Windows
machine before claiming support. Run from a Developer shell if needed. See
[upstream Windows guidance](https://zed.dev/docs/development/windows).
No Windows target/toolchain or SDK was installed during bootstrap.

### Linux (not yet verified)

The `ui` crate enables GPUI's `wayland` and `x11` features only for Linux. Build
with the same Cargo commands above on a machine with a Vulkan-capable GPU/driver
and the native development libraries required by the chosen window backend.
[Zed's Linux build guide](https://zed.dev/docs/development/linux) is an upstream
reference, though its full application needs more packages than this app.
Neither a Linux toolchain nor a Linux desktop runtime was available here; Cargo
feature resolution was inspected for `x86_64-unknown-linux-gnu`, but compilation
and interaction must be verified on Linux.

GPUI reports renderer, display-server and font failures through the `log` crate.
The app installs a small stderr logger (`crates/ui/src/diagnostics.rs`) that
prints warnings and errors by default; run with `RUST_LOG=info` (or `debug`) from
a terminal to collect details when the window does not appear.

### Linux container check (2026-09-26)

A tester reported many warnings and a failure to start on Linux. In a Podman
`rust:1-bookworm` container (aarch64, rustc 1.98.1) the build emitted the
`float_literal_f32_fallback` future-incompatibility warning from GPUI's
`taffy.rs`; the vendored copy now types those literals as `f32`. The remaining
note concerns `proc-macro-error2`, pulled in by GPUI's `stacksafe` dependency.
Workspace Clippy passed for Linux. Under Xvfb with Mesa lavapipe (plus openbox),
GPUI initialized Vulkan and X11 and both windows were created and managed, but the
root-window capture showed black window contents, so rendering is still
unconfirmed. The reported start failure was not reproduced; Wayland, real GPUs
and interaction still need a Linux desktop.

### Manual settings and UI checks

1. Launch and confirm the separate settings window opens over the four-pane
   chat window. After closing it, reopen it through **CayenChat → 接続設定…**
   or Cmd+, on macOS; use Ctrl+, on Windows/Linux. Verify the settings window
   defaults to `irc.ircnet.ne.jp:6667`
   with TLS off. Open the server drop-down and verify
   `irc6.ircnet.ne.jp` is also offered.
2. Add two servers from the drop-down, assign different hosts and encodings, and
   verify both appear above the built-in choices. Switch among them to check that
   host, port, TLS, and encoding values remain independent. Remove one custom server.
3. Toggle TLS and verify the standard port changes to `6697`. The certificate
   verification option should appear, default to on, and show a warning when
   turned off. Turn TLS off and on again; verification should reset to on.
   Toggle SASL and verify account/password fields appear. Type a dummy password and verify the
   screen masks it. Switch servers and confirm the values follow only their server.
4. Save with **パスワードを保存する** off and inspect the settings file: neither
   password should be present. Turn it on, confirm the plaintext warning, save,
   restart and verify both values reload. Turn it off and verify the saved values
   disappear immediately, before pressing Save. Versions 1 and 2 should migrate.
5. Close settings with its window close button and verify CayenChat stays open.
   Reopen settings from the menu and shortcut. Verify text editing, draft retention,
   and channel navigation in the chat window.
6. During connection or after a failed attempt, verify the full diagnostic trace
   appears without toggling the View menu, even when a channel is selected. It must
   distinguish DNS, TCP/TLS, certificate-verification mode, and registration
   progress, and show outgoing `→` and incoming `←` IRC lines. After registration,
   enable the full server transcript from the View menu. Copy it; PASS and SASL
   credentials must be masked even though normal message bodies are included.
   On a failed connection, confirm the copied transcript includes the final
   disconnect reason shown in the window.

The upper channel-message body can be drag-selected and copied with Cmd/Ctrl+C;
its HTTP(S) links open on a double-click. The lower combined log still uses clicks
for channel switching. Long-draft horizontal scrolling,
native/custom text bindings, general tab traversal, and exhaustive IME/accessibility
behavior remain future work. Chat history does not survive application exit.

### Manual live IRC checks

Use the in-app settings as described in README. For a local plaintext fixture,
choose Custom, set its host and port, and leave TLS off; do not enter credentials.

1. Launch and verify the server row changes from connecting to registered, and the
   configured channel joins. Select the server to inspect registration and errors.
2. Send a channel `PRIVMSG` with Enter and `NOTICE` with Ctrl+Enter; verify the IRC
   peer receives each and the draft clears only after local queue acceptance.
3. Deliver a channel message and a NAMES list from the peer; verify the main log and
   upper-right member pane update. Messages in another channel should mark it unread.
4. Send `/topic new topic`, `/mode +o bob`, `/part`, `/join #test`, and an explicit
   `/raw WHOIS bob`; verify the server receives the expected commands and the
   selected channel is inserted where required. Confirm a malformed command leaves
   the draft with an error.
   Choose Whois from a member's context menu and reply with 311/319/312/317/318;
   verify a raised WHOIS window shows the merged details, Update re-sends WHOIS,
   and an unrequested WHOIS reply only reaches the server log.
5. Disconnect the peer; verify the status becomes disconnected and an unsent draft
   is retained. Reconnect manually from the settings screen.
6. With a local ISO-2022-JP fixture, join `#がが` and exchange Japanese text.
   Verify its encoded comma bytes on JOIN/PRIVMSG and decoded channel
   routing. An unrepresentable emoji must show an error while retaining the draft.
7. Deliver enough channel history on initial connection to overflow the upper
   log, then deliver new messages. The selected channel, server log, and lower
   subwindow should each show their newest line. Scroll upward in one log and
   verify it stays put while new lines arrive; scroll back to the bottom and
   verify following resumes. Long channel/server labels in the lower log must
   remain on one line with an ellipsis.

The deterministic `irc-core` local-server tests cover registration, automatic join,
inbound message/NAMES translation, outgoing `PRIVMSG`/`NOTICE`, flushing `QUIT`,
slash-command normalization, and SASL PLAIN negotiation against a local protocol
fixture. The ISO-2022-JP fixture covers channel names and messages in both wire
directions. The SASL fixture bypasses the production TLS requirement by calling the
private worker directly; external TLS/SASL interoperability and long-running
sessions remain unverified.

### Bootstrap verification (2026-09-24)

On Apple Silicon macOS 26.6.2 with Homebrew Rust/Cargo 1.95.0 and Xcode 26.6:

- `cargo fmt --check` passed.
- `cargo clippy --workspace --all-targets --all-features --offline` passed.
- `cargo test --workspace --offline` passed (two application-state tests: distinct
  logs/identity and invalid selection; bidirectional navigation/wraparound).
- `cargo build -p cayenchat-ui --offline` and the development bundle build passed.
- `cargo tree -p cayenchat-irc-core` confirmed no dependencies, including GPUI.
- Native launch and rendered sidebar, all mock channels, Japanese log text,
  mouse selection, Alt+Down switching across networks, input focus and typing were
  observed. A Japanese/emoji multiline paste appeared as a single-line draft.
- Subsequent automated editing/draft-retention/quit checks were inconclusive because
  the UI automation connection returned stale state and `noWindowsAvailable` errors.
  A one-second process sample showed the live application servicing its normal event
  loop and drawing; it did not establish a deadlock. Full IME composition, draft
  retention through UI interaction, long input, accessibility and Windows remain
  unverified; do not infer these from successful compilation.

Xcode emitted sandbox-related FSEvents/cache diagnostics while linking some tests;
all test processes exited successfully. The separate Metal Toolchain and rustup are
absent; neither blocks this configuration. No system development tools were installed.

### Four-pane and editor correction (2026-09-24)

On the same macOS machine, the updated development bundle visibly rendered the
channel tree, main log, other-channel subwindow, user list, and draft row. Ctrl+Tab
selected #rust with its distinct user list; Ctrl+Shift+Tab wrapped from the first
to the last channel; clicking a subwindow line opened its source channel and the
original draft was retained. In the input, Option+Left moved by word, Command+Left
moved to the line start, and Command+Z / Command+Shift+Z undid/redid an insertion.

This does not establish complete OS text-system behavior. Additional UI automation
became inconclusive after its connection returned `noWindowsAvailable`; in
particular Option+Backspace, exhaustive IME composition, custom macOS key bindings,
accessibility and Windows behavior remain unverified.

### Reference four-pane placement (2026-09-24)

The updated macOS bundle visibly matched the supplied quadrant arrangement:
main log at upper left, user list at upper right, other-channel subwindow at lower
left, and network/channel tree at lower right, with the input between left logs.
Ctrl+Tab and a click in the lower-right tree updated the selected main log, users,
subwindow exclusion, selection highlight and window title. Clicking a lower-left
subwindow message opened its source channel. Windows runtime was not tested.

### Shortcut implementation check (2026-09-25)

On Apple Silicon macOS, `cargo fmt --check`, workspace Clippy, workspace tests,
and the UI build passed with the locked dependencies. The Linux-target dependency
tree showed both GPUI `wayland` and `x11` enabled; Linux and Windows builds/runs
remain unverified. In the updated macOS development bundle, Ctrl+Tab visited #rust
then #random and stopped when no unread channel remained. Ctrl+Shift+Tab and
Option+Shift+Space selected unread channels in reverse. Option+Tab returned to the
previous channel, Command+Down and Command+{ changed active channels, Ctrl+Left
and Command+Option+Right changed servers, and numbered shortcuts selected a
channel or server. Selecting a server showed its offline view; channel navigation
from a server selected a channel in that server. Tab completed `al` to `alice`,
then cycled to `alex`. Ctrl+Enter retained the draft and visibly reported that
NOTICE was not sent. Native interaction was not checked on Windows or Linux.

### Initial IRC connection check (2026-09-25)

On Apple Silicon macOS, `irc` 1.1.0 was integrated behind `irc-core` with rustls and
channel-list features. The workspace compiled and its Clippy and test checks passed.
A local TCP IRC fixture received registration, channel JOIN, a UI-entered `PRIVMSG`,
and a UI-entered `NOTICE`. The native GPUI window visibly showed the incoming channel
message, the NAMES roster, the local send echoes and the registered server marker.
A separate deterministic test confirmed that an explicit core disconnect flushes
QUIT to the peer. External TLS servers, reconnect, SASL, Windows and Linux remain
unverified.

### Settings and command implementation check (2026-09-25)

The app now opens the connection settings screen rather than using startup IRC
environment variables. The updated macOS development bundle visibly showed both
IRCnet presets, the custom server field, TLS-driven standard-port switching, SASL
fields, masked password text, validation feedback, and the four-pane Back action.
After correcting the GPUI punctuation key name, Command+Comma reopened settings
from the four-pane view. No external IRC server or real credentials were used for
this check.

Workspace tests cover settings round-trip without password fields, slash-command
target inference and control-character rejection, CAP/SASL PLAIN success and
failure, and the earlier local registration/message flows. External TLS/SASL
interoperability and Windows/Linux UI behavior remain unverified.

### Separate settings window check (2026-09-25)

On macOS, the development bundle opened the settings window separately at
startup. Closing it with the title-bar close button returned to the still-running
four-pane chat window. The native application menu and Command+Comma reopened
settings. Turning on password saving displayed the native plaintext-warning
sheet; Cancel left the checkbox off. The View menu changed from Show to Hide
diagnostics after toggling it. Workspace formatting, Clippy, and tests passed.
No real credentials or Tiarra endpoint were used in this check.

### TLS certificate verification option (2026-09-25)

The macOS development bundle displayed **証明書の検証: オン** for an existing
TLS-enabled server after migrating its version 3 settings. Workspace formatting,
Clippy, and tests passed. A unit test checks that only a TLS profile with
verification explicitly disabled sets `irc`'s invalid-certificate flag. A live
connection to a self-signed Tiarra server was not exercised.

### GUI TLS provider failure diagnosis (2026-09-25)

A screenshot of a TLS connection to Tiarra showed DNS resolution followed by
"Opening TCP connection and performing TLS handshake without certificate
verification", then an immediate worker panic reported as a disconnect. The
locked GUI dependency graph enables rustls `ring` through GPUI's HTTP client and
`aws-lc-rs` through `irc`; rustls cannot infer a default provider with both
features enabled. `irc` calls `ClientConfig::builder()` before opening the TCP
socket, which accounts for the missing wire or handshake output. `irc-core` now
installs the ring provider before starting its TLS worker. A regression test
checks that a default exists and `ClientConfig::builder()` succeeds with the
workspace's unified dependency features. The final disconnect reason is added
to diagnostic clipboard output. A temporary core probe without credentials
connected to a user-provided Tiarra endpoint with certificate verification off and
reached "TLS transport established"; the probe source was then removed. IRC
registration with credentials and the rebuilt GUI still need manual confirmation.

### Log labels and live scroll follow (2026-09-25)

The lower subwindow now keeps channel and server labels on one line, ellipsizing
each within its column. Both left logs follow their latest line until the user
scrolls upward; the main log keeps this state per selected server or channel.
Channel messages carry a monotonic arrival sequence so the combined subwindow
ends with the newest arrival even when several channels receive messages in one
displayed minute. Workspace formatting, strict Clippy, and tests passed,
including a cross-channel arrival-order test. The macOS development bundle was
rebuilt and launched to its disconnected view. A live initial Tiarra history
burst and manual scroll pause/resume still need visual confirmation.

### Appearance and Windows IME investigation (2026-09-25)

Settings version 5 adds Connection and Appearance tabs. Appearance saves colors,
alternating message rows, and per-pane font families. The channel log uses a
roughly 12-character nickname column and a platform-specific monospaced time
font. Unit tests cover version 4 migration, color validation, URL detection, and
cross-message selection ranges. On Windows, verify Japanese IME toggling with
Microsoft IME, ATOK, and Google Japanese Input; compose, convert, cancel, and
confirm with Enter/Tab in both draft and settings fields, and test surrogate-pair
characters. The app guards Send and completion while GPUI reports a marked
composition. GPUI 0.2.2 predates the merged upstream
[Windows IME keyboard fix](https://github.com/zed-industries/zed/pull/41259),
so the unpatched dependency could still fail to switch modes. The later
language-key audit and focused local patch are recorded below. Windows runtime
was not available for this check.

### Windows language-key handling audit (2026-09-25)

The app's Windows key bindings do not register Half-width/Full-width, Kanji, or
other IME mode keys. The published GPUI 0.2.2 Windows backend instead discards
keydown messages when `parse_normal_key` cannot name a key, skipping
`TranslateMessage`; it also unwraps `VK_PROCESSKEY` into a shortcut candidate.
The [upstream IME fix](https://github.com/zed-industries/zed/pull/41259)
identifies these paths as causes of Japanese/Korean language-key failures,
including the MS-IME Half-width/Full-width toggle. `vendor/gpui` is a local
GPUI 0.2.2 copy with a targeted patch to these paths. A macOS build can check
dependency integration but not compile or verify the Windows event path here.
On Windows 11, test the toggle in both the draft and settings text fields,
followed by composition, conversion, cancellation, Enter, Tab, and Alt+Space.
Compare against the same MS-IME and layout in a native Windows text editor.

### Japanese and English localization (2026-09-25)

Settings version 7 stores System, Japanese, or English. System resolves the OS
locale through `sys-locale`; Japanese uses `ja`, and all other or unavailable
languages use English. UI catalogs live in `locales/`; the executable reads
packaged JSON files at runtime and uses embedded copies when the files are
missing. Check that macOS, Windows, and Debian packages include both JSON files.
For UI verification, switch each language in Connection settings, save, reopen
settings, and inspect the chat window and native menus without reconnecting.
