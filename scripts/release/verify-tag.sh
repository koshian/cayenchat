#!/usr/bin/env bash
# Usage: verify-tag.sh TAG SHA
# Fails unless the remote tag is annotated and still points to SHA. Needs
# GITHUB_REPOSITORY and an authenticated `gh`.
set -euo pipefail

TAG="$1"
SHA="$2"
REF="$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$TAG")"
if [ "$(jq -r '.object.type' <<<"$REF")" != tag ]; then
  echo "::error::Tag $TAG is not an annotated tag"
  exit 1
fi
TAG_SHA="$(jq -r '.object.sha' <<<"$REF")"
TARGET="$(gh api "repos/$GITHUB_REPOSITORY/git/tags/$TAG_SHA" --jq '.object | select(.type == "commit") | .sha')"
if [ "$TARGET" != "$SHA" ]; then
  echo "::error::Tag $TAG no longer points to $SHA; refusing to publish"
  exit 1
fi
