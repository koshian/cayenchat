#!/bin/sh
# Measures test coverage of the workspace and lists every test.
#
#   scripts/coverage.sh [extra cargo-llvm-cov test arguments]
#
# Writes (all under target/, nothing is committed):
#   target/llvm-cov/html/index.html   line, region and function coverage per file
#   target/coverage/summary.json      the same totals as JSON
#   target/coverage/test-run.log      the test run's output
#   target/coverage/tests.html        every test: file, line, result, IRCv3 area,
#                                     next to its file's coverage
#
# Needs cargo-llvm-cov (`cargo install cargo-llvm-cov --locked`) and LLVM
# tools matching rustc's LLVM major version. They are found in this order:
# LLVM_COV/LLVM_PROFDATA, rustup's llvm-tools component, Homebrew's llvm.
# Ignored tests (interoperability checks that need a local server) are not
# run; pass `-- --include-ignored` with CAYENCHAT_INTEROP_IRC set to add them.
set -eu
cd "$(dirname "$0")/.."

if ! command -v cargo-llvm-cov >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo-llvm-cov" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "cargo-llvm-cov is missing: cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi

rustc_llvm=$(rustc -vV | sed -n 's/^LLVM version: \([0-9]*\).*/\1/p')
if [ -z "${LLVM_COV:-}" ] && ! command -v rustup >/dev/null 2>&1; then
    for prefix in /opt/homebrew/opt/llvm /usr/local/opt/llvm; do
        if [ -x "$prefix/bin/llvm-cov" ]; then
            LLVM_COV="$prefix/bin/llvm-cov"
            LLVM_PROFDATA="$prefix/bin/llvm-profdata"
            export LLVM_COV LLVM_PROFDATA
            break
        fi
    done
fi
if [ -n "${LLVM_COV:-}" ]; then
    tools_llvm=$("$LLVM_COV" --version | sed -n 's/.*LLVM version \([0-9]*\).*/\1/p' | head -n 1)
    if [ "$tools_llvm" != "$rustc_llvm" ]; then
        echo "LLVM tools are version $tools_llvm but rustc uses LLVM $rustc_llvm." >&2
        echo "Install matching tools (rustup component add llvm-tools, or a matching Homebrew llvm)." >&2
        exit 1
    fi
fi

mkdir -p target/coverage
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --no-report "$@" 2>&1 | tee target/coverage/test-run.log
cargo llvm-cov report --html
cargo llvm-cov report --json --summary-only --output-path target/coverage/summary.json
cargo llvm-cov report --summary-only
python3 scripts/test-inventory.py \
    --log target/coverage/test-run.log \
    --summary target/coverage/summary.json \
    --out target/coverage/tests.html
echo "Coverage: target/llvm-cov/html/index.html"
echo "Tests:    target/coverage/tests.html"
