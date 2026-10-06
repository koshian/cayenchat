#!/usr/bin/env bash
# Tests the release workflow's artifact and tag checks without a real tag,
# release or network: `gh` is replaced by a stub that serves canned answers.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/release" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
FAILED=0

expect() { # expect pass|fail DESCRIPTION COMMAND...
  local want="$1" what="$2"
  shift 2
  if "$@" >/dev/null 2>&1; then got=pass; else got=fail; fi
  if [ "$got" != "$want" ]; then
    echo "FAIL: $what (expected $want)"
    FAILED=1
  fi
}

artifacts() { # artifacts VERSION [DEB_VERSION] -> directory
  local dir="$WORK/artifacts-$1-${2:-$1}"
  mkdir -p "$dir"
  touch "$dir/cayenchat-$1-macos-arm64.zip" "$dir/cayenchat-$1-windows-arm64.zip" \
    "$dir/cayenchat-$1-windows-x86_64.zip" "$dir/cayenchat-ui_${2:-$1}-1_amd64.deb"
  echo "$dir"
}

expect pass "normal release" "$ROOT/check-artifacts.sh" "$(artifacts 0.9.0)" 0.9.0
expect pass "release candidate" "$ROOT/check-artifacts.sh" "$(artifacts 1.0.0-rc.1 '1.0.0~rc.1')" 1.0.0-rc.1
expect fail "pre-release deb without tilde" "$ROOT/check-artifacts.sh" "$(artifacts 1.0.0-rc.1)" 1.0.0-rc.1
expect fail "wrong version" "$ROOT/check-artifacts.sh" "$(artifacts 0.9.0)" 0.9.1
mkdir "$WORK/missing" && cp "$(artifacts 0.9.0)"/* "$WORK/missing" && rm "$WORK/missing"/*macos*
expect fail "missing platform" "$ROOT/check-artifacts.sh" "$WORK/missing" 0.9.0
mkdir "$WORK/extra" && cp "$(artifacts 0.9.0)"/* "$WORK/extra" && touch "$WORK/extra/stray"
expect fail "extra file" "$ROOT/check-artifacts.sh" "$WORK/extra" 0.9.0

# gh stub: `gh api .../git/ref/tags/*` prints $STUB_REF, `.../git/tags/*` prints
# $STUB_TAG; STUB_FAIL makes every call fail. A real create would print here.
mkdir "$WORK/bin"
cat > "$WORK/bin/gh" <<'EOF_GH'
#!/usr/bin/env bash
[ -z "${STUB_FAIL:-}" ] || exit 1
case "$2" in
  */git/ref/tags/*) printf '%s' "$STUB_REF" ;;
  */git/tags/*) printf '%s' "$STUB_TAG" ;;
esac
EOF_GH
chmod +x "$WORK/bin/gh"
export PATH="$WORK/bin:$PATH" GITHUB_REPOSITORY=o/r
SHA=1111111111111111111111111111111111111111
OTHER=2222222222222222222222222222222222222222
ref='{"object":{"type":"tag","sha":"aaa"}}'

STUB_REF="$ref" STUB_TAG="$SHA" expect pass "tag unchanged" "$ROOT/verify-tag.sh" v0.9.0 "$SHA"
STUB_REF="$ref" STUB_TAG="$OTHER" expect fail "tag moved" "$ROOT/verify-tag.sh" v0.9.0 "$SHA"
STUB_REF="$ref" STUB_TAG="" expect fail "tag points to a non-commit" "$ROOT/verify-tag.sh" v0.9.0 "$SHA"
STUB_REF='{"object":{"type":"commit","sha":"bbb"}}' STUB_TAG="$SHA" expect fail "lightweight tag" "$ROOT/verify-tag.sh" v0.9.0 "$SHA"
STUB_REF='{"message":"Not Found"}' STUB_TAG="$SHA" expect fail "tag deleted" "$ROOT/verify-tag.sh" v0.9.0 "$SHA"
STUB_FAIL=1 STUB_REF="$ref" STUB_TAG="$SHA" expect fail "API failure" "$ROOT/verify-tag.sh" v0.9.0 "$SHA"

# The workflow must run the tag check before creating the release.
verify="$(grep -n 'release/verify-tag.sh' "$ROOT/../../.github/workflows/release.yml" | head -1 | cut -d: -f1)"
create="$(grep -n 'gh release create' "$ROOT/../../.github/workflows/release.yml" | head -1 | cut -d: -f1)"
if [ -z "$verify" ] || [ -z "$create" ] || [ "$verify" -ge "$create" ]; then
  echo "FAIL: release.yml must verify the tag before gh release create"
  FAILED=1
fi

[ "$FAILED" = 0 ] && echo "release checks OK"
exit "$FAILED"
