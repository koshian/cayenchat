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
On Linux without a desktop session, look at it under Xvfb ("Visual checks under Xvfb").

Changes that affect event handling, retained state, rendering or resource
limits (for example multi-server connections or image display) must be
compared against the baseline with the procedure in `spec/performance.md`
(`scripts/perf/` and the ignored `perf_baseline` UI test).

Do not claim cross-platform verification unless both platforms were actually tested.

### Test coverage and test inventory

Step-by-step instructions for people are in `HOW_TO_TEST.md`
(`HOW_TO_TEST.ja.md`). `scripts/coverage.sh` measures coverage of
`cargo test --workspace` with
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) and lists every
test:

```sh
cargo install cargo-llvm-cov --locked   # once, into ~/.cargo/bin
scripts/coverage.sh
```

It writes `target/llvm-cov/html/index.html` (line, region and function
coverage per file, with annotated sources) and `target/coverage/tests.html`
(every test with its file, line and result, grouped by file next to that
file's coverage, counted per crate and per IRCv3 area, with a filter box;
`scripts/test-inventory.py` builds it from the sources, the run log and the
JSON summary). The IRCv3 area of a test is assigned from its file and name
(`AREA_BY_FILE`, `AREA_BY_NAME`), only as a navigation aid.

The LLVM tools must match rustc's LLVM major version. With rustup, use
`rustup component add llvm-tools`; without it (Homebrew's Rust on macOS,
whose standard library already includes the profiler runtime), the script
uses Homebrew's `llvm` (`brew install llvm`, keg-only, nothing linked) and
checks the version. Ignored tests (the interoperability checks, which need
a local server) are not run unless `-- --include-ignored` is passed.

Coverage counts the `#[cfg(test)]` modules inside source files as lines, so
percentages of files with inline tests read somewhat high, and GPUI
rendering code is mostly exercised by headless UI tests only. Coverage says
which code ran, not whether a specification is checked; conformance still
needs tests named after the requirement they check.

GitHub Actions CI builds and tests the whole workspace on Linux x86_64, Windows
x86_64, Windows ARM64 and macOS ARM64. The Windows jobs also run the ignored
system credential store probe against Credential Manager. CI does not exercise
GUI interaction, IME, drag and drop or clipboard images.

On pull requests, CI runs only the jobs the changed paths can affect
(`scripts/ci-jobs.sh`): documentation (`*.md`, `spec/`, `.claude/`,
`licenses/`) runs nothing, GPUI's per-platform sources under
`vendor/gpui/src/platform/{linux,windows,mac}` run only that platform (Linux
also runs the GUI end-to-end test), and the E2E scripts run only the GUI
end-to-end test. Any other path runs every job, so a new file that matters to
several platforms is covered until the script names it. Pushes to master always
run every job. The paths come from `git diff` of the checked-out pull request
merge commit against its first parent (renames count as both paths), so they
always match the commit the other jobs build; `scripts/test-ci-jobs.sh` checks
the selection and runs in the same job. Platform-specific code under `crates/` is not detected by path
and runs everywhere.

UI tests enable GPUI's `test-support` through a dev dependency. The menu regression
test renders the Linux/Windows in-window menu on every test host, including macOS,
and exercises the first frame and Alt reveal/hide without a display server or
saved user settings. This catches dispatch-tree initialization failures; it does
not replace validation on an actual Linux/Windows desktop.

## Documentation

Update only the relevant specification files when behavior or architecture changes:

- `project.md` for product direction/scope
- `architecture.md` for boundaries and data/event flow
- `decisions.md` for lasting technical choices
- `feature-parity.md` for implementation status
- `development.md` for build/test workflow
- `performance.md` for the performance baseline, measurement procedure and
  resource limits

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
The desktop UI requires Rust 1.94 (`crates/ui/Cargo.toml`) for the pinned
`native-theme-gpui` 0.5.7 connector. Other workspace crates retain Rust 1.88
(`rust-version` in the workspace manifest): GPUI 0.2.2
uses `let` chains, stabilized in 1.88, without declaring its own minimum, and
several locked dependencies (zbus 1.87, image/icu/encoding_rs 1.88) declare it
too. Debian's stock rustc 1.85 therefore cannot build; use backports or rustup.
Tested compilers are 1.95.0 (macOS) and 1.98.1 (Linux container). Rustfmt and Clippy
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

For testing a branch without touching real settings or saved passwords:

```sh
sh scripts/bundle-macos.sh --test-build
open "target/CayenChat Test.app"
```

This builds an optimized `CayenChat Test.app` with the `test-build` feature
(`cargo build --release -p cayenchat-ui --features test-build` elsewhere).
Every launch creates a new, empty settings directory under the system
temporary directory (printed to stderr, `$TMPDIR/cayenchat-test-*`) for
`settings.json` and the local credential file, files system-store secrets
under the service `CayenChat Test Build`, and shows `[test build]` in the
window title. Settings made in one launch are gone at the next.

Image previews fetch from the network in normal builds. To check them
without contacting any host, build with the `preview-fixture` feature and
point it at a directory of images; links to
`https://images.cayenchat.test/<file>` are then read from that directory and
every other link fails without a request (D018):

```sh
cargo build --release --locked -p cayenchat-ui --features preview-fixture
CAYENCHAT_PREVIEW_FIXTURE_DIR=target/perf/preview-images target/release/cayenchat
```

`scripts/perf/run_baseline.py --previews` generates such images and sends
matching links from the load fixture. Never ship a `preview-fixture` build.

The helper only builds and copies into ignored `target/`. It neither installs nor
signs/notarizes a distributable app. The normal `cargo run` entry point is portable.
The bundle includes `crates/ui/resources/macos/CayenChat.icns`; Windows embeds
`crates/ui/resources/windows/cayenchat.ico`. Both are rendered from the canonical
`assets/icons/cayenchat.svg` and checked in so normal builds need no image tools.
The version lives only in the workspace manifest (`[workspace.package] version`,
currently 0.9.0). The macOS bundle helper reads it with `cargo metadata`, and
`crates/ui/build.rs` generates the Windows VERSIONINFO resource (FileVersion and
ProductVersion) from it; the Windows x86_64 CI job checks that the built exe
reports the manifest version.
When changing the artwork, run `python3 scripts/generate-icons.py` with Pillow
and `rsvg-convert` (librsvg) installed, then rebuild the app.
The macOS beta workflow signs the completed bundle ad hoc and verifies its
resource seal before archiving. This is sufficient to validate bundle integrity
but does not provide Developer ID signing or notarization for Gatekeeper.
GitHub workflows default to a read-only token and check out without persisted
credentials; only the beta `publish` job gets `contents: write` to move the tag and
upload the release. Actions are pinned to commit SHAs (with the version in a
comment) and Dependabot proposes pin updates weekly.

To let a reporter try a change before it is merged, run the manual
`Test Build` workflow (`.github/workflows/test-build.yml`) from the Actions
tab or with
`gh workflow run test-build.yml -f ref=<branch or SHA> -f platform=windows-x86_64`
(`all`, `windows-x86_64`, `windows-arm64`, `linux-x86_64` or `macos-arm64`).
It packages that ref like the beta does and keeps each package as a workflow
artifact for 7 days, named `cayenchat-test-<short SHA>-<platform>` (`.zip`, or
`.deb` on Linux) and downloaded as that file; the macOS bundle is
`CayenChat Test.app`. The `.deb` is the package `cayenchat-test`, with
`Conflicts: cayenchat-ui` (the regular package's name, from the crate); the
regular package declares
`Conflicts: cayenchat-test`. The workflow adds the `test` variant to
`crates/ui/Cargo.toml` itself, so refs branched before it existed can be
packaged. Both packages install the same paths (`/usr/bin/cayenchat`, the
desktop file and `/usr/share/cayenchat/locales`), so they cannot coexist:
`sudo apt install ./<file>.deb` asks to remove the other one instead of
silently overwriting it (`dpkg -i` just fails). `isolated=true` builds with
the `test-build` feature, so the reporter's own settings and passwords are
not used (suffix `-isolated`).
It only has a read-only token and publishes nothing, so the beta is
unaffected. Link the run and the commit SHA in the PR or issue; downloading
an artifact needs a signed-in GitHub account. Both it and
`beta-release.yml` call the reusable `package.yml`, which holds the build and
packaging steps (`test: true` selects the Test Build names, the `cayenchat-test`
package and `CayenChat Test.app`), so a packaging change is made once. The
beta's artifacts stay archived and named after the platform for `publish`.
`publish` uploads `dist/*` over the rolling `beta` release and then deletes
assets that are not in `dist/` (for example a previous version's `.deb`), so the
release only holds the latest build and is never empty if an upload fails.

#### Changes that need a person's confirmation

Most changes are verified without a person: tests, CI on every platform,
and, for anything the GUI shows or how it reacts, a run on Linux under Xvfb
("Visual checks under Xvfb" below). Once those pass, a PR is merged even
though nobody has looked at it on macOS or Windows; how it looks and feels
there is checked afterwards on the beta, and what is found becomes a new
issue. "It may look different on macOS or Windows" alone does not hold a
merge.

A person's check before merging is needed only when the change depends on
what Xvfb on Linux cannot exercise:

- platform-specific code: `cfg(target_os = ...)` branches, the macOS and
  Windows backends, packaging for one platform;
- input methods (IME) and keyboard layouts;
- the desktop around the app: monitor layouts, display scaling and HiDPI,
  native file dialogs, the system credential stores, notifications, starting
  at login, drag and drop from other apps;
- anything the issue itself asks a person to confirm.

Changes that only run after they are merged, such as workflows triggered by
pushes to `master`, tags or releases, cannot be tried on the PR branch and
are an exception: review them, run what can run from the branch (for
example `workflow_dispatch` on the PR branch), merge without waiting for a
person, then check the first real run on `master` and fix forward.

When a person's check is needed before merging, do not just ask them to
wait for the beta. In this order, every time:

1. **Get the review to pass first.** Read every review and comment on the PR
   and answer each point (a fix, or a reason for not making it), then ask for
   a re-review. A review that approves "except for the real-machine check" has
   not passed until its suggestions are dealt with. Nobody is called before
   this: a person would test code that is still going to change, and the build
   would have to be made again. Building with `Test Build` early only to see
   that it packages is fine; do not announce it.
2. **Check what Xvfb can show first** ("Visual checks under Xvfb" below):
   run the change on Linux under Xvfb, look at it, and write in the PR what
   was verified that way (with the screenshots that matter) and what is left
   for a person. Ask a person only for what is left. When nothing is left,
   no one is called.
3. Build with `Test Build` for the platform they use, from the PR branch
   (before the merge) or from `master` when it is already merged by a person.
   Use `isolated=true` when their saved settings and passwords must not be
   touched; leave it off when the check needs their existing settings (for
   example restoring a saved window position).
4. Comment on the issue, mentioning the reporter with `@name`, with the run
   link, the commit SHA, the artifact name, how to start it (unzip and run;
   a signed-in GitHub account is needed; kept for 7 days), and what to look
   at. Ask for the environment details that matter (OS version, display
   scale, IME), say what was already checked under Xvfb and what is still
   unverified.
5. Take the report on the issue, and rebuild with `Test Build` for each new
   commit that needs checking again (and, if the change was reviewed again,
   only after that review has passed too). An LLM reviewer never merges a PR
   that needs this check before the report arrives; only a person may decide
   to merge without one.

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

Windows release builds set `windows_subsystem = "windows"` in the executable
crate, so launching the distributed exe does not create a console. Debug
builds keep the console. See the [Rust subsystem attribute](https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute).

On all platforms, **Settings → Experimental → Save debug logs (stderr)**
selects an append-only log file through the native save dialog. The switch
and path are saved and applied immediately, including in release builds.
Enabling without a selected file opens the dialog; Cancel leaves it off.
Disabling restores the original stderr destination. The logger switches to
debug while enabled and retains its credential/IRC/HTTP target filters.
Existing logs are preserved; users should turn logging off after diagnosis
and review the contents before sharing them. There is no log rotation.

Manual checks: select a path with spaces and Japanese characters, enable
logging, change its path while enabled, disable it, and relaunch to verify
persistence. Check the active destination label and appended session marker.
Cancel the picker and try an unwritable destination: preferences must not
claim a successful save, and an already active log must continue receiving
stderr. On Windows, launch the release exe directly from Explorer and verify
that neither enabling nor disabling logging opens a console. Inspect the
PE header with `dumpbin /headers cayenchat.exe` (subsystem Windows GUI).
The isolated subprocess tests in `ui::diagnostics` cover direct stderr,
logger output, panic hooks, failed switches, append/re-enable and restoration;
a Windows-only test also starts with a null standard-error handle. The Windows
x86_64 CI job builds release and checks the PE subsystem is Windows GUI.

### Linux (not yet verified)

The `ui` crate enables GPUI's `wayland` and `x11` features only for Linux. Build
with the same Cargo commands above on a machine with a Vulkan-capable GPU/driver
and the native development libraries required by the chosen window backend.
[Zed's Linux build guide](https://zed.dev/docs/development/linux) is an upstream
reference, though its full application needs more packages than this app.
On Debian/Ubuntu the link step needs `libxcb1-dev`, `libxkbcommon-dev` and
`libxkbcommon-x11-dev` (a missing one fails as `cannot find -lxkbcommon-x11`),
plus `libwayland-dev`, `build-essential` and `pkg-config`; at runtime `libvulkan1`
and a Vulkan driver such as `mesa-vulkan-drivers`. This set built in the Debian
bookworm container below.

The Debian package (`cargo deb`, configured in `crates/ui/Cargo.toml`) installs
`assets/linux/cayenchat.desktop` to `/usr/share/applications/` and the canonical
SVG `assets/icons/cayenchat.svg` to
`/usr/share/icons/hicolor/scalable/apps/cayenchat.svg`, so desktop menus list
CayenChat with `Icon=cayenchat`. dpkg triggers of the desktop and icon-cache
packages refresh their caches; no maintainer script is needed. Verify with
`dpkg-deb -c target/debian/*.deb` and
`desktop-file-validate assets/linux/cayenchat.desktop`.

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

The tester's log then showed the actual failure on Wayland: when the XDG desktop
portal reported the color scheme, GPUI's Wayland client called each window's
appearance callback while holding its own `RefCell` borrow. The app's appearance
observer calls `App::window_appearance()`, which borrows the same state, so the
app panicked with "RefCell already borrowed". The vendored Wayland and X11
clients now release the borrow before notifying windows. Linux Clippy passes;
the fix still needs confirmation on the tester's Wayland desktop, including a
light/dark switch while the app is running.

With the window up, the tester's Debian machine kept reconnecting to IRCnet.
A Linux-container probe of `irc-core` against `irc.ircnet.ne.jp:6667` received
`020 Please wait` and then nothing until the 30-second registration timeout;
with a longer limit the 001 welcome and MOTD arrived after about 32 seconds
(the server waiting out its ident lookup). The registration timeout is now 90
seconds. Dropped (not rejected) inbound port 113 on the client network causes
this delay.

### Manual settings and UI checks

1. Launch and confirm the separate settings window opens over the four-pane
   chat window. After closing it, reopen it through **CayenChat → 接続設定…**
   or Cmd+, on macOS; use Ctrl+, on Windows/Linux. With new settings (the
   test build always starts with new settings) verify the server list says no
   servers are registered, the main log explains how to add one, and the
   drop-down offers IRCnet, IRCnet (IPv6), IRCnet (dev) and another server.
2. Add IRCnet and a blank server from the drop-down, assign different hosts and
   encodings, and verify both are listed in the order added. Switch among them
   to check that host, port, TLS, and encoding values remain independent.
   Remove one; remove the other and verify the form returns to the empty state.
3. Toggle TLS and verify the standard port changes to `6697`. The certificate
   verification option should appear, default to on, and show a warning when
   turned off. Turn TLS off and on again; verification should reset to on.
   Toggle SASL and verify account/password fields appear. Type a dummy password and verify the
   screen masks it. Switch servers and confirm the values follow only their server.
4. With **パスワードを保存する** off, type passwords, leave the fields and inspect
   the settings file: neither password should be present. Turn it on (no prompt
   with system storage). While a password field keeps focus it must stay as
   typed; leaving it (or closing the window with the field still focused)
   stores it. Verify the fields become empty with the "saved" placeholder, the settings
   file still has no password, and Keychain Access (macOS), Credential Manager
   (Windows) or Seahorse/`secret-tool search service CayenChat` (Linux) shows
   `connection/<id>/…` entries (on macOS, one `secrets` item whose data lists
   those names). On macOS, after rebuilding, connecting should ask for the
   login password once however many passwords are saved, and opening settings
   afterwards should not ask again. Restart with startup connection on and verify
   they are used. Turn saving off and verify the entries disappear immediately.
   A version 10 settings file with saved passwords should lose them from the
   file on the next start and gain the store entries.
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

7. In **資格情報の保存先**, verify the system-store status line. Choose the local
   file, confirm, and verify `credentials.json` (mode `0600` on Unix) holds the
   saved entries while the system store no longer does; switch back. On a Linux
   session without a Secret Service, the status must say it is unavailable and
   the local file must still work.
8. In **画像アップロード**, select ImgBB, connect with a test account's API key,
   and paste a screenshot (`Cmd+Ctrl+Shift+4`) into a channel draft: the
   confirmation must name ImgBB,
   Cancel must leave the draft unchanged, and Upload must show progress and then
   insert an `https://i.ibb.co/…` link without sending. Drop a PNG on the
   draft row for the same flow; drop a text file and two files for the refusals.
   Paste plain text to confirm ordinary paste. Disconnect the account and paste
   again for the reconnect guidance; select None for the configure guidance.
   Use a wrong key for the authentication-failure prompt. On macOS, drag a
   HEIC photo from Photos and a screenshot thumbnail onto the draft row; both
   must reach the confirmation, and `$TMPDIR/gpui-file-promises/` must be
   empty afterwards.

9. Image previews, with a `preview-fixture` build (above) and a local load
   fixture started with `--image-every 2`: in **外観**, **画像リンクのプレビューを表示する**
   is off for new settings. Turn it on: image links in the selected
   channel show a thumbnail below the text (an outlined box while loading),
   the link text stays, a double-click on the link or the thumbnail opens it,
   drag selection and copy still cover only the text, and the combined log
   and server log show no images. Scroll up and down while images load and
   switch channels; the log must not jump or reset, and at the bottom it must
   keep following new lines. Turn previews off: the thumbnails and
   their space disappear at once and no further link is read (the fixture
   directory's access times do not change).
10. User avatars, with a `preview-fixture` build and a local IRC fixture
   that offers `batch` and `draft/metadata-2` (never a public server): in
   **外観**, **ユーザーのアバターを表示する** is off for new settings and is
   the only avatar switch; the IRCv3 tab has no metadata option (its
   **ピア間のアバター** option is D025's, step 12). Connect (the
   transcript shows `CAP REQ batch` and `CAP REQ draft/metadata-2` even with
   **メッセージのバッチ** off) and have the fixture send
   `METADATA <nick> avatar * :https://images.cayenchat.test/<file>`: nothing
   is downloaded and the layout is unchanged while the Appearance setting is
   off. Turn it on: 16×16 images appear before nicknames in the channel log
   and the member list, rows keep their height and the nickname stays
   readable while images load or fail; the combined log, server log and
   activity lines stay text-only. Scroll while images load, switch channels
   and servers; the log must not jump and must keep following at the
   bottom. Change a user's nick and reuse the old nick from another client:
   the old lines keep the old image, the new user's lines show none until
   their own avatar arrives. Users without an avatar, and those whose image
   failed, show a default avatar whose colors, hat and eyes match
   `defaultAvatar.js` for the same nickname. Turn the setting off: the
   column disappears at once and no further image is read.
11. Own avatar, with the same setup against a disposable local server (for
   example the pinned Ergo below; never a public server or real account):
   in **IRCv3**, the section
   **このサーバーでの自分のアバター** shows the URL field and no explanatory
   text. Type a URL and wait for autosave: nothing is sent (the transcript
   shows no `METADATA * SET`). While disconnected neither **IRCサーバに送信**
   nor **IRCサーバから削除** is shown. Connected, **IRCサーバに送信** shows
   the waiting line, then disappears (the field matches what the server
   confirmed) and **IRCサーバから削除** appears; another client in the
   channel receives it, and our own lines show the image when avatars are
   displayed. Edit the URL: **IRCサーバに送信** reappears; send again; then
   **IRCサーバから削除** (right-aligned in the warning color; hovering
   **IRCサーバに送信** shows the exposure warning as a tooltip): a
   confirmation dialog appears, Cancel sends nothing, and 削除 sends only
   `METADATA * SET avatar`. A URL with `user:pass@`,
   `?token=` or a private host is refused before sending; an over-long URL
   is refused by Ergo (`INVALID_VALUE`) and more than 10 changes in 2
   minutes show the rate limit with its delay. Turning **ユーザーのアバターを
   表示する** off does not change what the server holds. Reconnect: the
   draft stays, nothing is republished, and the buttons follow what the
   server kept. A user joining the channel after us gets an avatar in the
   member list about 2 s after joining. With an image host set up on
   **画像アップロード** (a test account), drop a PNG on the section, paste a
   screenshot into the URL field and use **画像を選択…** (also a large
   phone photo, which must appear upright): each opens the square editor;
   the corner handles resize the square and dragging inside moves it
   (also when the mouse leaves the view); dropping it below half of the
   image shows it centered with the area around it at twice its size,
   growing it again shows the whole image, **全体に戻す** resets, and the
   result preview follows; Cancel uploads nothing; **… にアップロードして
   送信** shows progress, puts the host's URL into the field and sends it
   (if the connection ends during the upload, a line says it was not
   sent; while disconnected, or on a server without avatar metadata, the
   image only fills the field, still with **画像を選択…**, drop and paste;
   connected to a server without `draft/metadata-2`, the section says
   **この IRC サーバーはアバターに対応していません。**), and the uploaded image is at most 256×256. A HEIC file is refused with the
   export hint.
   Switch servers during an upload: the URL goes to the server it was for.
   With no host set up there is no **画像を選択…** and no drop target; an
   image paste says to set up image upload.
12. Peer avatars (D025), against a disposable local server without
   `draft/metadata-2` (never a public server), with a raw client or KVIrc
   as the peer: **ピア間のアバター (CTCP AVATAR、実験的)** is off for new
   settings; with it off, `\x01AVATAR\x01` from the peer is never answered
   and shows `CTCP AVATAR request from <peer> (not answered)` (D029), and the `USER` line ends in `CayenChat`. Turn it on
   and type a URL: autosave shares nothing (no **ピアに共有中** line; a
   query from the peer gets no NOTICE). **ピアに共有** (tooltip: the
   exposure warning) shows **ピアに共有中: <url>**, the peer's query is now
   answered with `NOTICE <peer> :\x01AVATAR <url>\x01`, and, while
   connected, **実名欄のアバター表示を更新するには再接続してください。**
   appears until reconnecting; after reconnecting the `USER` line ends in
   `\x034\x0fCayenChat`. Editing the field or uploading an image does not
   change the shared URL; a URL with `{size}`, credentials or a private host
   is refused. From the peer, set the realname to `\x034\x0f…` and speak in
   the channel: the transcript shows one `WHO <peer>`, then one
   `PRIVMSG <peer> \x01AVATAR\x01`; answer
   `NOTICE <me> :\x01AVATAR https://images.cayenchat.test/<file> M\x01`: with
   **ユーザーのアバターを表示する** on, the image appears; nothing shows in
   the chat, server log, unread marks or notifications. A file-name answer
   (`\x01AVATAR me.png 2048\x01`) removes it and sends no DCC. Speaking
   again asks nothing more; a user without the mark is never queried;
   joining a large channel sends no WHO. Change the peer's nick: the avatar
   follows; quit and reuse the nick from another client: none. **共有を停止**
   and turning the option off stop answering at once.

The upper channel-message body can be drag-selected and copied with Cmd/Ctrl+C;
its HTTP(S) links open on a double-click. The lower combined log uses double-clicks
for channel switching. Long-draft horizontal scrolling,
native/custom text bindings, general tab traversal, and exhaustive IME/accessibility
behavior remain future work. Chat history does not survive application exit.

### Manual live IRC checks

Add the server in the in-app settings. For a local plaintext fixture,
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

8. Run two local fixtures on different ports (for example
   `python3 scripts/perf/irc_load_server.py --port 16671 --control-port 16672`
   and `--port 16673 --control-port 16674`), add both as servers with
   different nicknames and the same channel name, and mark both to connect
   at startup. Restart: both must register and join, each channel log and
   roster must stay with its server, and the tree must show every saved
   server (servers not used yet without a status mark). Disconnect one from
   its context menu; the other keeps receiving. Remove one in settings and
   confirm the prompt; it disappears with its channels. Add a server, fill it
   in without connecting, and connect it from the tree: with a nickname saved it
   connects, otherwise settings open on it.

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
For UI verification, switch each language in Connection settings, reopen
settings, and inspect the chat window and native menus without reconnecting.

### Linux performance tuning (2026-09-26, issue #5)

Channel switching and typing were sluggish on Debian (Radeon, Wayland). Measured
in a Debian container, cosmic-text shaping cost 0.1–0.3 ms per new line, so a
switch reshaped about 50 visible lines. The vendored GPUI now caches shaping per
word (bounded), log lists splice only changed rows, the combined log rebuilds
only on new messages or selection changes, IRC events are awaited instead of
polled every 50 ms, QUIT/NICK republish only affected rosters, and the log
panes, user list and channel tree are cached views that typing does not redraw.
A GPUI test asserts that draft input leaves the panes cached. On the Debian
machine, channel switching became noticeably faster, and typing plus pane
updates (messages, switching, selection, scrolling, hover, appearance changes)
behaved correctly.
Follow-up: the channel tree is virtualized too, and message times are stored
as minutes (`TimeOfDay`), shrinking each retained message from 88 to 64 bytes
plus one fewer heap allocation. Not done: formatting wire diagnostics lazily and
sending roster deltas instead of full NAMES snapshots on JOIN/PART in large
channels. The 2026-09-27 baseline and remaining candidates are in
`performance.md`.

### Window close error logs on Windows (2026-09-26)

Closing a window (typically the settings window shown at startup) printed
`window not found` twice and two "invalid window handle" HRESULTs
(`0x80040102` from `RevokeDragDrop`, `0x80070578` from `DestroyWindow`). All
four came from GPUI's Windows teardown, not the app: the deferred drop task ran
on an HWND that `DefWindowProcW` had already destroyed, and platform callbacks
fired for a window GPUI had already removed. The vendored GPUI now checks
`IsWindow` first and drops the removed-window callback errors quietly (see
`vendor/gpui/PATCHES.md`). macOS `cargo check`, Clippy and tests pass; the
Windows build and runtime were not verified.

### Credential storage and image upload (2026-09-26)

On Apple Silicon macOS (rustc 1.95.0), `cargo fmt --check`, workspace Clippy
and workspace tests passed. Automated coverage: credential store get/set/delete,
backend migration, unavailable backend without fallback, local file round trip
with `0600`/`0700` permissions and re-tightening, sanitized errors, XDG path
rules, redacted `Debug` output, settings version 11 migration (username from
nickname, plaintext passwords moved to the store or to the local file when the
system store is unavailable, never re-serialized), USER/NICK/PASS/SASL on the
wire in local `irc-core` fixtures, multipart construction, ImgBB reply
mapping, the attachment state machine, and GPUI tests for text paste, image
paste, drop, confirmation/cancel, configure/reconnect guidance, failed upload
and link insertion without sending, using a fake uploader and an in-memory
credential store. An ignored test (`-- --ignored`) probes the real system store:
available on macOS; in a Debian bookworm container without D-Bus it reported
"no system credential store was found" (Unavailable). In that container
(aarch64, rustc 1.98.1) the non-GUI crate tests and the 30 UI tests passed, and
workspace Clippy was
clean apart from the existing `proc-macro-error2` future-incompatibility note.
No real ImgBB upload, Photos drag, Windows build, Secret Service desktop (GNOME Keyring or
KWallet) or GUI interaction was exercised.

### Inline image previews (2026-09-27)

On Apple Silicon macOS 26.6.2 (rustc 1.95.0), `cargo fmt --check`, workspace
Clippy with all features and workspace tests passed with the lockfile.
Automated coverage: settings version 14 migration and round trip (off by
default); candidate recognition, including ImgBB links and refused schemes,
credentials, ports and local/private addresses; address classification;
the HTTP loader against a local fixture server (headers sent, non-image and
SVG responses, error statuses, declared and streamed size limits, redirect
limit and re-checked targets, the production policy never contacting
127.0.0.1 or `localhost`, timeout and cancellation); decoding of small and
large PNG/JPEG/GIF/WebP, malformed and oversized content and the decoder
allocation cap; the cache (deduplication, bounded queue and in-flight
loads, byte-budget eviction, bounded records and retries, disabling while
loading, stale completions, removed rows); and GPUI tests for no loading
while off, loading once per link when switched on live, local echoes,
typing reusing the panes, disabling during a load, removed conversations and
a removed server. The GUI check and measurements are in `performance.md`.
Selection/copy, link opening by double-click and channel switching with
previews were not exercised in the real window (no input automation was
available); Windows and Linux were not run.

### User avatars and IRCv3 metadata (2026-09-27)

In the Linux x86_64 cloud container (rustc 1.94.1), `cargo fmt --check`,
workspace Clippy with all features and workspace tests passed with the
lockfile. The container lacked `libxkbcommon-dev` and
`libxkbcommon-x11`; their Ubuntu packages were unpacked into a scratch
directory and used through `LIBRARY_PATH`/`LD_LIBRARY_PATH` without
installing anything. Automated coverage: default-off settings, migration of
files without the fields, per-server independence and the batch-dependency
warning; CAP ordering (metadata only after batch ACK, never
`metadata-notify`), NAK, DEL of either capability, NEW, SASL together and
legacy encodings; a local IRC fixture for SUB after 001 and before JOIN,
avatar values, updates, removal, the metadata batch, deferred SYNC, NICK,
PART, QUIT, withdrawal and no server-log lines; bounded syncs and users;
occupancies across nickname changes, reuse, reconnects and server removal;
extensionless avatar URLs with the same safety checks, square crop and
limits; GPUI tests for no lookups or fetches while off, live on/off with
deduplicated, visible-only loads, unchanged row heights, the shared fetch
limit with previews, typing reusing the panes and a removed server.

GUI check: the `preview-fixture` release build ran under Xvfb with Mesa's
llvmpipe Vulkan driver (also unpacked into a scratch directory), an
isolated `HOME` with a prepared settings file (the `test-build` feature
always starts from empty settings, so it could not be preseeded) and a
local IRC fixture offering `batch` and `draft/metadata-2`; `xdotool` drove
the settings window and `xwd` captured windows (captures only show content
after an input event triggers a frame). Verified: the IRCv3 row, its hint
and the batch warning; live on/off from the Appearance tab with autosave;
avatars in log and member rows at the same row spacing as off; scrolling
up during a burst and following at the bottom; a renamed user keeping the
avatar, the old nickname's earlier line keeping the old image and a new
user of that nickname showing none until their own avatar arrived. Not
verified: macOS and Windows, a real IRC server, IME, HiDPI sharpness, and
whether no image file is read while off (the file system's `relatime` made
access times inconclusive; the GPUI tests cover it).

### GUI end-to-end test (Linux)

`scripts/e2e/history_gui.py` runs the real `cayenchat` binary on a virtual
X display against the pinned Ergo, with a local proxy between them, and
checks channel history as a user sees it. It adds no test hook to the
application; everything is observed from outside:

- **wire**: the proxy records the CHATHISTORY commands CayenChat sends
  (LATEST on join, nothing older until scrolled, BEFORE with a new msgid
  per page, none repeated after the beginning, LATEST with a msgid after a
  reconnect), holds the answer to the first BEFORE, and cuts and refuses
  the link to cause a real unexpected disconnect;
- **pixels**: the main log pane is grabbed from the X server while that
  answer is held and again after it was inserted; the two must be
  identical (rows wrapping to two lines make the heights vary);
- **text**: the log's own drag selection and Ctrl+C, read back with
  `xclip`, show which lines are on screen and in which order (the first
  line of the channel after paging; A, the missed B and C, our rejoin,
  then D after the reconnect, each once).

Screenshots of every step and the app and Ergo logs go to `--out` (default
`target/e2e`). The throwaway `HOME` the app runs with is deleted when the
run ends, pass or fail. Two
deliberate breakages were checked to fail it: resetting the log list when
rows are inserted above (the pixel check fails) and placing recovered lines
where the request was made instead of at the cut (the order check fails).

```sh
cargo build --locked -p cayenchat-ui
ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
python3 scripts/e2e/history_gui.py --app target/debug/cayenchat --ergo-dir /tmp/cayenchat-ergo --out /tmp/cayenchat-e2e
```

Required packages, running without root, reading the output and adding
checks are in `HOW_TO_TEST.md` ("GUI end-to-end test"). Operating-system
packages cannot be declared in Cargo; that list and the `gui-e2e-linux` job
in `.github/workflows/ci.yml` name the same packages and change together.
It takes about two minutes. CI runs it in the `gui-e2e-linux` job and
uploads the screenshots. It is Linux/X11 only: the driving tools are X11
ones. The same scenario on macOS or Windows would need a logged-in desktop
session and platform tools (for example `screencapture` and synthetic
events on macOS); the proxy, peer and assertions would carry over.

### Visual checks under Xvfb (Linux)

Much of what used to be left for a person to look at can be seen by the
developer (an LLM included) on a virtual display. `scripts/e2e/gui_session.py`
keeps the real app running under Xvfb between commands: `start`,
`click X Y`, `type`, `key`, `scroll`, `drag`, `move`, `focus`, `wait`,
`shot [--crop x0,y0,x1,y1 --scale N]`, `clipboard`, `opened`, `windows`, `quit` and
`restart-app` (both with Ctrl+Q, as a user quits; a restart says so when it
had to terminate the app instead, which skips what quitting saves), `stop`.
The user's settings and passwords are never touched: the app's HOME, XDG
directories and runtime directory are in the session directory, passwords
go to the session's local file even when `--settings` names a copy of real
settings with the system store, and the desktop's D-Bus session (whose
Secret Service holds the user's passwords under the same service name) is
not passed on. Without that session GPUI cannot reach the desktop portal and
opens links with `xdg-open` (or `gio` and the like); the session puts
stand-ins for them first on the app's `PATH` that only append the URL to
the session's `opened.txt`, so no browser starts and `opened` prints the
URLs opened since it last ran. `start` refuses a non-empty directory it did not make and
clears only its own entries when a session is started again;
`scripts/e2e/test_gui_session.py` checks both, without a display (CI runs it
in the `gui-e2e-linux` job). Each input command prints the
path of a PNG taken once the screen has settled; an LLM reads it as an
image. `scripts/e2e/gui_session.py --help` lists the options, and the
`gui-check` skill (`.claude/skills/gui-check/SKILL.md`) is the step-by-step
guide with the known pitfalls. It shares `Display` with the end-to-end test
and reads Xvfb's framebuffer file (`-fbdir`), so it needs only Xvfb and
xdotool (xclip for `clipboard`), a Vulkan driver (lavapipe works) and at
least one font (`--fonts` lends a directory when the system has none).
A real server comes from the local Ergo playground
(`scripts/e2e/manual_history.py`) with `start --server`.

Use it for every change to what the GUI shows or how it reacts, before
asking anyone ("Changes that need a person's confirmation", step 2), and
record in the PR what was verified and what was not. It can show: layout
and wording in both languages and themes, settings screens and their
controls, menus, drop-downs and dialogs drawn by the app, mouse and keyboard
behavior (ASCII), focus moves, scrolling, which URL a clicked link opens
(`opened`), what is written to the settings
file, what survives `restart-app`, and connected behavior against the
local Ergo. It cannot show: macOS and Windows, Wayland, a real GPU, display
scaling and HiDPI sharpness, multiple monitors, IME and non-ASCII typing,
native file dialogs, the system credential stores, notifications, drag and
drop from other apps, and how smooth motion feels (screens are stills).
Which of these must be checked by a person before merging, and which on the
beta afterwards, is in "Changes that need a person's confirmation". Coordinates from one screenshot go stale after the
layout changes, and judging an image is not a pixel comparison: a lasting
check belongs in a test.

First run (2026-10-03, on Debian trixie without system fonts or the X
`-dev` packages, unpacked into a scratch directory): the debug build
started under Xvfb with lavapipe; adding IRCnet from the drop-down, the TLS
switch moving the port to `6697` with certificate verification shown on,
typing a nickname (autosaved to the session's settings file), reading it
back through the clipboard and keeping all of it across `restart-app` were
all seen in screenshots. The run also found that the app panicked on
startup without servers, fixed separately.

### IRC metadata interoperability (Ergo)

Independent server: Ergo v2.19.1 (tag commit
`63c743a70644f0f19109508ade8580bf37f7d23d`, 2026-08-04). Its source
(`irc/caps/defs.go`, `irc/metadata.go`, `irc/handlers.go`) implements
`draft/metadata-2` by also enabling its `draft/metadata-3` code, and the
default configuration enables metadata. Run it isolated, with a scratch
directory for the source, Go caches, database and configuration, a
loopback-only plaintext listener, no TLS and no accounts:

```sh
scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
```

It needs `git`, Go (1.26.4 was used, `GOTOOLCHAIN=local`, vendored
dependencies) and `awk`; nothing is installed. Then, in another terminal:

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 cargo test --locked -p cayenchat-irc-core --test metadata_interop -- --ignored --nocapture --test-threads 1
```

The tests use CayenChat's `Connection` as the client under test and a
minimal raw client for the other users, with per-run nicknames and
channels and example.com URLs (nothing is fetched). Stop the server with
Ctrl-C and delete the directory afterwards.

### Recent channel history (2026-09-28)

The same pinned Ergo serves the chathistory check (its default
configuration keeps in-memory channel history and advertises
`CHATHISTORY=1000`, `MSGREFTYPES=msgid,timestamp` and `draft/chathistory`):

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 cargo test --locked -p cayenchat-irc-core --test chathistory_interop -- --ignored --nocapture
```

Result against Ergo v2.19.1 on loopback (macOS, debug build; passed):
`CAP REQ message-tags`, `server-time`, `batch`, then `draft/chathistory`
after batch's ACK; one `CHATHISTORY LATEST <channel> * 50` per joined
channel, the second only after the first reply ended; a reply of at most
50 lines ending with the newest line, with msgid and server-time; a live
line afterwards arriving as an ordinary live message and no history line
arriving as live. Observed: Ergo returns our own JOIN (and other joins) as
`HistServ` PRIVMSGs ("<nick> joined the channel") counted within the
limit when event-playback is not negotiated, so a channel nobody spoke in
still returns those lines; and its fakelag lets the filling client send
about two lines a second (the test waits up to 60 s). Empty replies, FAIL,
malformed/unended/nested batches, bounds, timeouts and stale replies are
covered by fixtures and unit tests, not by Ergo.

### Older channel history pages (2026-09-28)

`chathistory_interop.rs` also has
`older_channel_history_pages_against_a_real_server` (same command as
above; `--test-threads 1` keeps the output readable). Result against the
pinned Ergo v2.19.1 on loopback (Linux container, debug build; both tests
passed): after `LATEST`, `CHATHISTORY BEFORE <channel> msgid=<oldest> 50`
returned exactly the older lines (and HistServ join lines) and its batch
carried `draft/chathistory-end`; a following
`BEFORE ... timestamp=<oldest>` returned an empty batch with the same tag;
no page line arrived as live traffic. Ergo 2.19.1 needs Go 1.26 while the
container had 1.24.7, so it was built as `scripts/ergo-metadata-interop.sh`
does but with `GOTOOLCHAIN=go1.26.4`, which downloads that toolchain into
the scratch `GOPATH` only. Headless UI tests there linked against a
libxkbcommon-x11 extracted into a scratch directory (`LIBRARY_PATH`,
`LD_LIBRARY_PATH`) rather than installed. Scrolling in a real window was
not exercised; the viewport behavior is covered by the headless GPUI test
with rows of different heights.

### Reconnect gap recovery (2026-09-28)

`chathistory_interop.rs` also has
`reconnect_recovers_missed_lines_against_a_real_server`. Against the
pinned Ergo v2.19.1 on loopback (Linux container, debug build; all three
tests passed): a first session saw line A live and quit; a peer sent B
and C; a new connection with A as its resume point sent
`CHATHISTORY LATEST <channel> msgid=<A> 50` and got exactly B and C (plus
HistServ quit/join lines) with `draft/chathistory-end`, and a later live
line arrived live; a third connection resuming by timestamp only got A
again (the 5 s skew allowance; the application drops it as a duplicate)
followed by B, C and the later line. Reconnect timing inside the UI, the
limit-reached note and duplicate merging are covered by unit and headless
UI tests, not by Ergo.

### Own avatar publishing and later joiners (2026-09-28)

On Apple Silicon macOS (rustc 1.95.0), `cargo fmt --check`, workspace
Clippy with all features and workspace tests passed with the lockfile.
Automated coverage: per-server draft persistence and files without the
field; no publishing from autosave or from turning display off; explicit
publish/remove with server-confirmed outcomes, rewritten values,
`KEY_NOT_SET` on removal, rejections under both draft and Ergo names,
rate limits with and without a delay, timeouts, repeated clicks, capability
loss, disconnects and stale answers; answers addressed to `*` or changes
made elsewhere never confirming a request; command injection and length
limits; URL policy (credentials, token-like parameters, blocked hosts);
later joiners' lookups with the pause, deduplication, bounds, spacing,
timeouts, `RATE_LIMITED` retry, `INVALID_TARGET`, departures, rejoin
before the answer, NICK and replayed JOINs; a GPUI test driving a real
`Connection` against a local fixture through `ChatWindow`; and no chat
rows, lookups or fetches from metadata events while display is off.

Interoperability against Ergo v2.19.1 (both ignored tests passed):
negotiation (`CAP REQ batch`, then `CAP REQ draft/metadata-2`), `SUB`
answered by `770` and `774 * *ALL 0`, our `GET` answered in a `metadata`
batch with `766 <nick> <nick> avatar`; initial delivery of an existing
member's avatar in a `metadata` batch after our JOIN (before NAMES) as
`761 * <nick> avatar * <url>`; publish and change answered with `761 <nick>
<nick> avatar * <url>` and relayed to the other member as `761 * <nick>
…`; removal answered with `766 <nick> <nick> avatar :Key deleted` and
relayed as `766 * <nick> …`; another member's change relayed as `761 *`;
a user joining after us not announced (our `GET` found the avatar);
NICK moving it and QUIT ending it; a new user of the same nickname
without avatar answered with `766` and shown none; a 350-byte value
refused with `FAIL METADATA INVALID_VALUE avatar :Value is too long`; the
11th change within 2 minutes refused with `FAIL METADATA RATE_LIMITED
<nick> avatar 113`; after reconnecting Ergo reported no avatar (no
account) and CayenChat sent no `SET`. See D023 for the discrepancies with
the draft this handles.

Real GUI check (macOS, debug build with `preview-fixture`, isolated
`HOME` and a prepared settings file with the local-file credential
backend, connected to the Ergo above, `https://images.cayenchat.test/…`
avatars read from a scratch directory): the chat window showed the
existing member's avatar in the member list after joining, and a user
who joined afterwards got theirs about 2 s later through the lookup; that
user's message sent right after joining showed no avatar, as the identity
policy requires. Only window captures were possible: this session had no
permission to send input (macOS Accessibility was not granted, and was not
changed), so the settings window, Publish/Remove feedback and the display
toggle were not exercised in the real window; the GPUI tests above cover
their logic, and manual step 11 lists the check. Windows and Linux were
not run.

### Peer avatars with CTCP AVATAR (2026-09-28)

On Apple Silicon macOS (rustc 1.95.0), `cargo fmt --check`, workspace
Clippy with all targets and features, and workspace tests passed. The
performance suite was not run: no rendering or event-path code outside
the opt-in worker state changed. Automated coverage: the realname mark
(digits `0`–`7`, bit 2, KVIrc's color tag after it, malformed marks);
CTCP parsing (tag case, optional closing 0x01, KVIrc's `M`/`F`/`?` field,
`avatar.notify`'s size, empty and `""` answers, file names with `\040`,
Windows and Unix paths, `file:`/`ftp:`, controls, legacy encodings);
shareable URLs; answers only while sharing, per-user and total rate
limits, no answer to channel, replayed, self or server queries; answers
and announcements only from users present now; discovery bounds (queue,
spacing, outstanding, timeouts, no re-ask, `263`), our WHO/315/401
consumed and others shown; NICK, QUIT, PART and our own PART; stale
lookups after NICK; metadata precedence, fallback and reset; settings
defaults and files without the fields; explicit Share, draft edits and
uploads not changing it, Stop and turning the option off; the reconnect
line; avatar fetch limits with declared, missing, understated and
overstated lengths; and two fixture connections through the real worker
(one KVIrc-format exchange end to end, one with the option off) plus a
GPUI test through `ChatWindow` (autosave shares nothing, Share answers at
once, turning the option off stops answering).

Interoperability: the fixtures reproduce KVIrc's lines as its source
writes them (see D025); no running KVIrc was available in this
environment (it is not installed and was not built), so interoperation
with KVIrc itself is an assumption from its source, not a tested result.
No GUI run was made for this change; manual step 12 lists the check.

### CTCP answers (2026-09-28)

On Apple Silicon macOS, `cargo fmt --check`, workspace Clippy with all
targets and features (`-D warnings`) and workspace tests passed. The
performance suite was not run: only the worker's handling of CTCP lines
changed. Automated coverage (`irc-core::ctcp` and one fixture connection
through the real worker): PING with and without an argument and without
the closing 0x01, VERSION, TIME with a fixed clock, CLIENTINFO with and
without peer avatars; USERINFO, DCC, AVATAR with peer avatars off, channel
requests, server-prefixed requests, oversized PING and odd tags shown but
not answered; replies shown with formatting stripped and long text cut;
replayed, own and misaddressed CTCP dropped silently; ACTION and plain
text left to chat; STATUSMSG targets; controls stripped from shown names;
per-user and total limits, one refusal line per window even when other
users are still answered, the reply budget kept separate; and through the worker, private VERSION and
PING answered in order while a channel TIME is not, ACTION and chat still
channel rows, no private rows.

Real server: `crates/irc-core/tests/ctcp_interop.rs` (ignored by default)
runs CayenChat's connection against the pinned Ergo 2.19.1 from
`scripts/ergo-metadata-interop.sh`, with raw clients as peers:

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:PORT cargo test --locked -p cayenchat-irc-core --test ctcp_interop -- --ignored --nocapture
```

It clears Ergo's default `+C` channel mode, checks the four answers as
received by the peers, a channel VERSION and a USERINFO left unanswered,
and a six-request flood from each of three peers: five answers in total,
at most two per peer, one refusal line, and chat still arriving without a
disconnection. It passed on 2026-09-28. The first run found that the
refusal line came back after another user was answered; it is now limited
to once per window.

Manual check in the app, against a disposable local server with a raw
client as the peer: `PRIVMSG <me> :\x01VERSION\x01`, `…PING 123…`, `…TIME…` and
`…CLIENTINFO…` each get a NOTICE and a `CTCP … request from <peer>` line in
the server log; `PRIVMSG #chan :\x01VERSION\x01` shows
`… to #chan (not answered)` and gets nothing; ten quick requests get at
most five answers (two per sender) and one "Too many CTCP requests" line.
No GUI run was made for this change.

### Native settings appearance checks (D019)

The regression tests in `ui::settings_theme` render macOS Sonoma, Windows 11,
Adwaita and KDE Breeze presets in light/dark modes on GPUI's test platform.
This validates mapping, rendering, input retention and click callbacks; it
is not native Windows/Linux runtime validation.

On each desktop, check the Connection, Appearance, Notifications, Image Upload
and Credential Storage tabs. Verify native fonts/fills, readable light/dark
text, input focus borders, TLS/SASL/certificate and notification check states,
password masking, save/connect/back behavior and large-font layout. Set the
app to Light while the OS is dark and vice versa; the explicit app choice must
win. Reopen settings after changing only the OS accent or font. Without a
working desktop portal the UI must remain interactive and fall back to the
platform preset or app colors. No data-model or authentication behavior changed.

Tab/Shift+Tab move between settings text fields and wrap (D020); Tab in a
chat draft still completes nicknames. Known limits: widgets are GPUI drawings,
not embedded OS controls; buttons, checkboxes and selectors are not focusable
and accessibility is unchanged. New Windows/Linux native
reader behavior requires real desktop testing. System-only accent/font changes
are not watched continuously; reopening settings refreshes them.

### Server-confirmed sending checks (D036)

Unit tests: label uniqueness and size, labeled echo with a rewritten text,
ACK, error numeric, FAIL, labeled batch, unlabeled matching by target and
text, self-messages, bounds, expiry and its next deadline (`echo.rs`); CAP
requests and dependency order, legacy encodings, off (`cap.rs`); fake-server
flows for
labeled echo/batch/error, echo-only and no-echo servers; app confirm/fail and
duplicate handling; UI in-place confirmation, failure mark, disconnect with
pending messages. No live Ergo/soju run was made.

### User account tracking checks (D035)

Unit tests (`accounts.rs`): extended JOIN with account, with `*`, plain JOIN,
ACCOUNT login/change/logout, PART/KICK/QUIT/own PART cleanup, NICK with IRC
casemapping, republished member lists, WHOX (ISUPPORT gate, queue, token,
consumed reply, timeout), memory bounds. CAP request tests, a fake-server test
of the whole flow and of the feature being off, a UI test of the mirror and
WHOIS completion. No live Ergo/soju run was made.

`cargo clippy --workspace --all-targets` fails in `crates/app` tests on master itself.

### CHATHISTORY TARGETS checks (D034)

Unit tests (`history.rs`): reply parsing and batch framing, unknown/known and
case-duplicate peers, channel targets, invalid names, bounds (64 entries, 16
peers, queue), FAIL and timeout, both directions of direct-message lines.
Fake-server test: TARGETS → JOIN's LATEST → the direct message's LATEST →
`ChannelHistory` with no live events. UI test: quiet conversation creation,
overlap dropped, one notification only for the live line. Existing fixtures
now expect the TARGETS request after registration. No live Ergo/soju run was
made; `cargo clippy --workspace --all-targets` fails in `crates/app` tests on
master itself.

### account-tag checks (D033)

Unit tests: tag parsing (`*`, empty, absent), account changing between
messages, history lines with the tag, CAP request rule (with negotiation,
UTF-8 only), the UI metadata mapping and `ServicesAccount` bounds; the
`Message` size guard is now 104 bytes. `cargo clippy --workspace
--all-targets` fails in `crates/app` tests (`while let`) on master itself. No
live Ergo/soju run was made for this change.

### Real name and SETNAME checks (D032)

Unit and fake-server tests cover: default/explicit/marked wire realname,
`USER` registration, SETNAME accepted, without the capability, and rejected by
`FAIL SETNAME`, the CAP request rule, settings migration and persistence, and
the reconnect configuration. `cargo clippy --workspace --all-targets`
currently fails in `crates/app` tests (`while let` lint) on master itself,
unrelated to this change. No live Ergo/soju run was made for this change.

### X server loss (2026-10-06, issue #196)

Killing the Xvfb of a running session left the app at 100% CPU, repeating
"error while polling for X11 events" (166 GB of log in one case). The vendored
GPUI now stops its event loop on an X11 connection error, so the app quits
through its normal quit path (`vendor/gpui/PATCHES.md`).
`scripts/e2e/xserver_loss.py --app target/debug/cayenchat` starts a session,
kills only its Xvfb and asserts that the app exits and `app.log` stops
growing.
