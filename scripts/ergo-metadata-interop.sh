#!/bin/sh
# Builds a pinned Ergo and runs it on the loopback interface with a disposable
# configuration, for the IRC metadata interoperability check described in
# spec/development.md. Everything (source, Go caches, database, config) stays
# in WORK_DIR; nothing is installed and no global setting is changed.
#
#   scripts/ergo-metadata-interop.sh WORK_DIR [PORT]
#
# Then, in another terminal:
#
#   CAYENCHAT_INTEROP_IRC=127.0.0.1:PORT \
#     cargo test --locked -p cayenchat-irc-core --test metadata_interop -- --ignored
set -eu
ERGO_TAG=v2.19.1
ERGO_COMMIT=63c743a70644f0f19109508ade8580bf37f7d23d
work=${1:?usage: scripts/ergo-metadata-interop.sh WORK_DIR [PORT]}
port=${2:-36667}
mkdir -p "$work"
work=$(cd "$work" && pwd)
if [ ! -d "$work/ergo-src" ]; then
    git clone --quiet --depth 1 --branch "$ERGO_TAG" \
        https://github.com/ergochat/ergo.git "$work/ergo-src"
fi
if [ "$(git -C "$work/ergo-src" rev-parse HEAD)" != "$ERGO_COMMIT" ]; then
    echo "ergo-src is not $ERGO_TAG ($ERGO_COMMIT)" >&2
    exit 1
fi
# Vendored dependencies; caches inside WORK_DIR.
(cd "$work/ergo-src" && GOPATH="$work/gopath" GOCACHE="$work/gocache" \
    GOFLAGS=-mod=vendor GOTOOLCHAIN=local go build -o "$work/ergo" .)

run="$work/run"
rm -rf "$run"
mkdir -p "$run"
cp -R "$work/ergo-src/languages" "$run/languages"
cp "$work/ergo-src/ergo.motd" "$run/ergo.motd"
# The default configuration with one plaintext listener on 127.0.0.1:PORT
# (no TLS listener, no IPv6), metadata left enabled as shipped.
awk -v port="$port" '
    /"127\.0\.0\.1:6667":/ { sub(/127\.0\.0\.1:6667/, "127.0.0.1:" port); print; skipping = 1; next }
    skipping && /# Example of a Unix domain socket/ { skipping = 0 }
    skipping && /^        [#"]/ && !/^        # Example/ { next }
    skipping && /^            / { next }
    skipping && /^ *$/ { next }
    { print }
' "$work/ergo-src/default.yaml" > "$run/ircd.yaml"
if ! grep -q "\"127.0.0.1:$port\":" "$run/ircd.yaml" || grep -q ':6697":' "$run/ircd.yaml"; then
    echo "Could not rewrite the listeners of default.yaml" >&2
    exit 1
fi
cd "$run"
"$work/ergo" initdb --conf ircd.yaml --quiet
echo "Ergo $ERGO_TAG listening on 127.0.0.1:$port (Ctrl-C to stop; data in $run)"
exec "$work/ergo" run --conf ircd.yaml
