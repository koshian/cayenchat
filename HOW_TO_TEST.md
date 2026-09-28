# How to test

[日本語](HOW_TO_TEST.ja.md)

This page is for people who want to run CayenChat's tests and look at
their coverage themselves. The rules contributors follow are in
`spec/development.md` (Validation).

## Run the tests

```sh
cargo test --workspace
```

Tests live next to the code they test (`#[cfg(test)] mod tests` at the end
of each source file) and in `crates/*/tests/`. Some tests are ignored by
default because they need a local IRC server; see
[Interoperability tests](#interoperability-tests).

## Measure and view coverage

### One-time setup

1. Install cargo-llvm-cov into `~/.cargo/bin`:

   ```sh
   cargo install cargo-llvm-cov --locked
   ```

2. Make LLVM tools of the same major version as rustc available. Check
   rustc's with `rustc -vV` (line `LLVM version`).
   - Rust from rustup: `rustup component add llvm-tools`.
   - Rust from Homebrew (no rustup): `brew install llvm`. It is keg-only and
     links nothing into your PATH; the script finds it and checks that the
     versions match.
   - Anything else: set `LLVM_COV` and `LLVM_PROFDATA` to the tools' paths.

### Measure

From the repository root:

```sh
scripts/coverage.sh
```

It rebuilds the whole workspace with coverage instrumentation and runs
every test, so the first run takes several minutes. It writes, under
`target/` (never committed):

| File | What it shows |
| --- | --- |
| `target/coverage/tests.html` | Every test: file, line, result, IRCv3 area |
| `target/llvm-cov/html/index.html` | Which lines ran, per file |
| `target/coverage/summary.json` | Coverage totals as JSON |
| `target/coverage/test-run.log` | The test run's output |

Open the reports in a browser, for example on macOS:

```sh
open target/coverage/tests.html
```

```sh
open target/llvm-cov/html/index.html
```

`python3 scripts/test-inventory.py` alone prints test counts per IRCv3
area from the sources, without building anything.

### Reading `tests.html` (what is tested, and where)

- The top line gives the number of tests, how many passed, were ignored or
  failed, and total line coverage.
- **Per crate**: tests and line coverage for each crate.
- **IRCv3 areas**: how many tests cover CAP negotiation and SASL,
  message-tags / server-time / msgid, batch, metadata and so on, and in
  which files. The area is assigned from the file and test name, so treat
  it as a guide. "Ignored" there means a test that needs a real server.
- **Tests by file**: click a file to see its tests with line numbers and
  results. "source" opens the file in the line coverage report.
- The filter box matches test names, files, areas and results (try
  `sasl`, `batch` or `ignored`).
- **Files without tests of their own** lists files of 50 lines or more
  that contain no test. Other tests may still run them; check their line
  coverage.

Test names describe the behavior they check, for example
`capability_names_are_case_sensitive`. Tests that check an IRCv3
requirement quote it in a comment above the test.

### Reading the line coverage report

- The index lists every file with the share of functions, lines and
  regions (sections of code between branches) that ran.
- Click a file to see its source. Red lines never ran in any test; the
  number beside a line is how often it ran.
- Useful places to start: the IRCv3 code in `crates/irc-core/src/`
  (`cap.rs`, `tags.rs`, `replay.rs`, `metadata.rs`).

### What coverage does not tell you

- A line that ran is not a line that was checked. Whether CayenChat
  follows a specification is shown by tests that check that requirement,
  not by the percentage. A file can be close to 100 % and still miss a
  rule.
- Tests written inside a source file count as lines of that file, so such
  files read somewhat higher.
- The desktop UI (GPUI) runs only in headless tests, so UI files read
  lower; drawing, IME and platform behavior are checked by hand (see
  `spec/development.md`).

## Interoperability tests

Some tests talk to a real IRC server and are ignored unless one is given.
Start a disposable Ergo on the loopback interface (it builds a pinned
version in the directory you name; Go is required, nothing is installed):

```sh
scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
```

Then, in another terminal, include ignored tests:

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 scripts/coverage.sh -- --include-ignored
```

or run them without coverage:

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 cargo test -p cayenchat-irc-core -- --ignored --test-threads 1
```

Stop the server with Ctrl-C and delete the directory afterwards.

## GUI end-to-end test (Linux only)

`scripts/e2e/history_gui.py` starts the real `cayenchat` build on a virtual
display, connects it to a local Ergo, and drives it with the mouse and
keyboard like a user. It currently checks channel history (loading older
pages by scrolling, recovering lines missed during a reconnect). It takes
about two minutes.

### What it checks, and how

The application has no test hook; everything is observed from outside.

- **Wire**: a proxy between the app and Ergo records the `CHATHISTORY`
  commands the app sends. It can also hold a reply for a moment and cut
  the link (a real unexpected disconnect).
- **Pixels**: the screen is grabbed from the X server before and after an
  older page is inserted; the main log area must not change by a single
  pixel.
- **Text**: the log is drag-selected and copied with Ctrl+C, and the
  clipboard shows which lines are on screen and in which order.

Screenshots of every step and the app and Ergo logs go to `--out`.

### Requirements

Linux only (the virtual display and input tools are X11 ones). Cargo cannot
declare operating-system packages, so the same list is written here and in
the `gui-e2e-linux` job of `.github/workflows/ci.yml`; change both together.

| Purpose | Debian/Ubuntu packages |
| --- | --- |
| Building the app | `libxcb1-dev` `libxkbcommon-dev` `libxkbcommon-x11-dev` |
| Rendering without a GPU (lavapipe, software Vulkan) | `mesa-vulkan-drivers` (pulls in `libvulkan1`) |
| Virtual display | `xvfb` |
| Mouse and keyboard input | `xdotool` |
| Screen grabs (`xwd`) | `x11-apps` |
| Reading the clipboard | `xclip` |
| Building Ergo | Go 1.26 or later (see below for older Go) |

Python 3 with its standard library only.

```sh
sudo apt-get install libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
  mesa-vulkan-drivers xvfb xdotool x11-apps xclip
```

### Run

```sh
cargo build --locked -p cayenchat-ui
ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
python3 scripts/e2e/history_gui.py --app target/debug/cayenchat \
  --ergo-dir /tmp/cayenchat-ergo --out /tmp/cayenchat-e2e
```

- The second line builds the pinned Ergo and only configures it (the test
  starts it). `ERGO_NO_FAKELAG=1` lifts Ergo's rate limit so the test's
  peer can fill a channel quickly.
- With an installed Go older than 1.26, add `GOTOOLCHAIN=go1.26.4`; Go then
  fetches that toolchain into the work directory.
- The virtual display is `:99`; pass `--display :98` (or another) if it is
  taken.
- The test stops Ergo and the display when it ends. Nothing leaves the
  machine (Ergo listens on loopback only).

### Reading the result

Each check prints `ok: …`, and `passed` at the end means success. A failure
prints `FAILED: …` with what was expected and what was found, and exits
with status 1. In `--out`:

- `01_joined.png`, `02_page_requested.png`, …: the screen at each step, in
  order. `*_selection.png` is the screen right after the drag selection used
  to read the text.
- `app.log`: the app's output; look here when it does not start or exits.
- `ergo.log`: Ergo's log.

CI (the `gui-e2e-linux` job) uploads this directory as the `gui-e2e-linux`
artifact whether the test passed or not.

### Without root (when packages cannot be installed)

The packages can be unpacked instead of installed (this is how the test was
first run, in a container):

```sh
mkdir -p ~/e2e-libs/debs && cd ~/e2e-libs/debs
apt-get download libvulkan1 mesa-vulkan-drivers xdotool libxdo3 x11-apps xclip libxmu6 \
  libxkbcommon-x11-0 libxcb-xkb1
for f in *.deb; do dpkg-deb -x "$f" ~/e2e-libs/root; done
R=~/e2e-libs/root
export PATH="$R/usr/bin:$PATH"
export LD_LIBRARY_PATH="$R/usr/lib/x86_64-linux-gnu"
export LIBRARY_PATH="$R/usr/lib/x86_64-linux-gnu"   # for linking the build
export VK_ICD_FILENAMES="$R/usr/share/vulkan/icd.d/lvp_icd.json"
```

- If `apt-get download` gets a 404 because the package index is older than
  the archive, fetch a newer `.deb` of the same series directly (for example
  from `http://archive.ubuntu.com/ubuntu/pool/main/m/mesa/`).
- Without the `-dev` packages, the linker lacks `libxkbcommon.so` and
  `libxkbcommon-x11.so`; add symbolic links to the `.so.0` files in
  `$R/usr/lib/x86_64-linux-gnu`.
- `ldd target/debug/cayenchat | grep "not found"` lists what is missing.

### Adding checks

- The window is fixed at 1000×700; the pixel area (`LOG_BOX`) and the
  coordinates used for input assume that size.
- Pixel comparison suits "nothing moved", copied text suits "what is shown
  and in which order", and the proxy's record suits "what was sent".
- Wait for the screen with `Display.settle()` (until consecutive grabs
  match) rather than fixed sleeps.
- Check that a new check can fail by breaking the app on purpose for one
  run (for example, resetting the log list on every change). The current
  checks were verified that way with two such breakages.

### macOS and Windows

The proxy, the peer and the assertions do not depend on the operating
system, but grabbing the screen and producing input need other tools per
platform (`screencapture` and synthetic events on macOS, UI Automation on
Windows) and a logged-in desktop session. Not provided yet.

## Troubleshooting

- `cargo-llvm-cov is missing`: run the install command above; the script
  also looks in `~/.cargo/bin` when it is not on your PATH.
- `LLVM tools are version X but rustc uses LLVM Y`: install tools of
  rustc's LLVM major version, or point `LLVM_COV`/`LLVM_PROFDATA` at them.
- Stale or odd numbers: the script cleans previous coverage data before
  each run; delete `target/llvm-cov-target` to rebuild from scratch.
- E2E test fails at `the chat window opened`: the app did not start. See
  `--out/app.log`: `error while loading shared libraries` means a missing
  library; a Vulkan error means checking `mesa-vulkan-drivers` (or
  `VK_ICD_FILENAMES`).
- E2E text checks read nothing (`[]`): the drag selection did not happen.
  Check that `*_selection.png` shows the highlighted selection.
- Ergo build fails with `go.mod requires go >= 1.26`: add
  `GOTOOLCHAIN=go1.26.4`.
