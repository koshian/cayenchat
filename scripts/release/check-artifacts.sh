#!/usr/bin/env bash
# Usage: check-artifacts.sh DIR VERSION
# Fails unless DIR holds exactly the four release packages for VERSION.
set -euo pipefail

DIR="$1"
VERSION="$2"
# cargo-deb writes a pre-release `1.0.0-rc.1` as the Debian version `1.0.0~rc.1`.
DEB_VERSION="${VERSION/-/\~}"
EXPECTED="cayenchat-${VERSION}-macos-arm64.zip
cayenchat-${VERSION}-windows-arm64.zip
cayenchat-${VERSION}-windows-x86_64.zip
cayenchat-ui_${DEB_VERSION}-1_amd64.deb"
if [ "$(ls "$DIR")" != "$EXPECTED" ]; then
  echo "::error::Unexpected release artifacts:"
  ls "$DIR"
  exit 1
fi
