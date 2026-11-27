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
  # The Windows release check runs exactly when the Windows x86_64 job does.
  local windows=false
  [[ ,$names, == *,'Windows x86_64',* ]] && windows=true
  # The FreeBSD build check runs exactly when the Linux job does.
  local freebsd=false
  [[ ,$names, == *,'Linux x86_64',* ]] && freebsd=true
  if [[ $got != "$names" || $out != *"windows=$windows"$'\n'"gui=$gui"$'\n'"freebsd=$freebsd" ]]; then
    echo "FAIL $what: got matrix '$got', $(tail -n3 <<<"$out" | paste -sd' '); want '$names', windows=$windows gui=$gui freebsd=$freebsd"
    fail=1
  fi
}

all='Linux x86_64,Windows x86_64,Windows ARM64,macOS ARM64'

expect 'no changes' '' false
expect 'documentation' '' false README.md spec/development.md .claude/x licenses/a
expect 'windows script' 'Windows x86_64,Windows ARM64' false scripts/check-windows-subsystem.ps1
expect 'e2e only' '' true scripts/e2e/history_gui.py scripts/ergo-metadata-interop.sh
expect 'windows script and docs' 'Windows x86_64,Windows ARM64' false scripts/check-windows-subsystem.ps1 spec/a.md
expect 'windows script and e2e' 'Windows x86_64,Windows ARM64' true scripts/check-windows-subsystem.ps1 scripts/e2e/a.py
expect 'vendored linux platform' "$all" true vendor/gpui-pre-linux/src/lib.rs
expect 'vendored windows platform' "$all" true vendor/gpui-pre-windows/src/clipboard_data.rs
expect 'vendored macos platform' "$all" true vendor/gpui-pre-macos/src/lib.rs
expect 'removed vendor/gpui path' "$all" true vendor/gpui/src/platform/linux.rs
expect 'unknown path' "$all" true crates/app/src/lib.rs
expect 'unknown with docs' "$all" true README.md crates/app/src/lib.rs
expect 'rename out of unknown path' "$all" true crates/app/src/lib.rs README.md
expect 'workflow' "$all" true .github/workflows/ci.yml

out=$(./ci-jobs.sh --all </dev/null)
[[ $(grep -o '"name"' <<<"$out" | wc -l) -eq 4 && $out == *windows=true$'\n'gui=true$'\n'freebsd=true ]] || { echo 'FAIL --all'; fail=1; }

exit $fail
