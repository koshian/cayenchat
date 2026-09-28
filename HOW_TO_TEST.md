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

## Troubleshooting

- `cargo-llvm-cov is missing`: run the install command above; the script
  also looks in `~/.cargo/bin` when it is not on your PATH.
- `LLVM tools are version X but rustc uses LLVM Y`: install tools of
  rustc's LLVM major version, or point `LLVM_COV`/`LLVM_PROFDATA` at them.
- Stale or odd numbers: the script cleans previous coverage data before
  each run; delete `target/llvm-cov-target` to rebuild from scratch.
