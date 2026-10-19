#!/usr/bin/env bash
# Usage: check-artifacts.sh DIR VERSION
# Fails unless DIR holds exactly the four release packages for VERSION.
set -euo pipefail

DIR="$1"
VERSION="$2"
EXPECTED="cayenchat-${VERSION}-linux-x86_64.deb
cayenchat-${VERSION}-macos-arm64.zip
cayenchat-${VERSION}-windows-arm64.zip
cayenchat-${VERSION}-windows-x86_64.zip"
if [ "$(ls "$DIR")" != "$EXPECTED" ]; then
  echo "::error::Unexpected release artifacts:"
  ls "$DIR"
  exit 1
fi
