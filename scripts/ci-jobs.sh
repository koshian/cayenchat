#!/usr/bin/env bash
# Chooses the CI jobs a pull request needs from the paths it changes, read one
# per line from stdin, and prints them as GitHub step outputs: `matrix`, the
# build-and-test platforms as JSON, and `gui`, whether the Linux GUI end-to-end
# test runs. With --all (pushes to master) every job runs.
#
# A path the cases below do not name runs everything. Documentation runs
# nothing; GPUI's per-platform sources, which only compile for that target, and
# scripts used by one job run only that platform or job.
set -euo pipefail

linux=false windows=false macos=false gui=false
all() { linux=true windows=true macos=true gui=true; }

if [[ ${1-} == --all ]]; then
  all
else
  while IFS= read -r path; do
    case $path in
      '' | *.md | spec/* | .claude/* | licenses/*) ;;
      vendor/gpui/src/platform/linux*) linux=true gui=true ;;
      vendor/gpui/src/platform/windows* | scripts/check-windows-subsystem.ps1) windows=true ;;
      vendor/gpui/src/platform/mac*) macos=true ;;
      scripts/e2e/* | scripts/ergo-metadata-interop.sh) gui=true ;;
      *) all ;;
    esac
  done
fi

entries=()
$linux && entries+=('{"name":"Linux x86_64","os":"ubuntu-24.04"}')
$windows && entries+=('{"name":"Windows x86_64","os":"windows-latest"}' '{"name":"Windows ARM64","os":"windows-11-arm"}')
$macos && entries+=('{"name":"macOS ARM64","os":"macos-latest"}')

(IFS=,; echo "matrix=[${entries[*]}]")
echo "gui=$gui"
