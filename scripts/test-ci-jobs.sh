#!/usr/bin/env bash
# Checks the job selection of scripts/ci-jobs.sh against known path sets.
set -euo pipefail

cd "$(dirname "$0")"
fail=0

# expect <description> <expected matrix names, comma separated> <expected gui> <paths...>
expect() {
  local what=$1 names=$2 gui=$3 out got
  shift 3
  out=$(printf '%s\n' "$@" | ./ci-jobs.sh)
  got=$(grep '^matrix=' <<<"$out" | { grep -o '"name":"[^"]*"' || true; } | sed 's/"name":"//;s/"//' | paste -sd, -)
  if [[ $got != "$names" || $out != *"gui=$gui" ]]; then
    echo "FAIL $what: got matrix '$got', $(tail -n1 <<<"$out"); want '$names', gui=$gui"
    fail=1
  fi
}

all='Linux x86_64,Windows x86_64,Windows ARM64,macOS ARM64'

expect 'no changes' '' false
expect 'documentation' '' false README.md spec/development.md .claude/x licenses/a
expect 'linux platform' 'Linux x86_64' true vendor/gpui/src/platform/linux/x11/client.rs
expect 'windows platform' 'Windows x86_64,Windows ARM64' false vendor/gpui/src/platform/windows/a.rs
expect 'windows script' 'Windows x86_64,Windows ARM64' false scripts/check-windows-subsystem.ps1
expect 'mac platform' 'macOS ARM64' false vendor/gpui/src/platform/mac/a.rs
expect 'e2e only' '' true scripts/e2e/history_gui.py scripts/ergo-metadata-interop.sh
expect 'linux and docs' 'Linux x86_64' true vendor/gpui/src/platform/linux.rs spec/a.md
expect 'linux and mac' 'Linux x86_64,macOS ARM64' true vendor/gpui/src/platform/linux.rs vendor/gpui/src/platform/mac/a.rs
expect 'unknown path' "$all" true crates/app/src/lib.rs
expect 'unknown with docs' "$all" true README.md crates/app/src/lib.rs
expect 'rename out of unknown path' "$all" true crates/app/src/lib.rs README.md
expect 'workflow' "$all" true .github/workflows/ci.yml

out=$(./ci-jobs.sh --all </dev/null)
[[ $(grep -o '"name"' <<<"$out" | wc -l) -eq 4 && $out == *gui=true ]] || { echo 'FAIL --all'; fail=1; }

exit $fail
