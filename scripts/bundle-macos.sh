#!/bin/sh
# Local development bundle only; no signing, installation, or system changes.
set -eu
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
    echo 'This development bundle helper requires macOS.' >&2
    exit 1
fi
# --test-build: an optimized "CayenChat Test" bundle whose settings live in a
# fresh temporary directory per launch (see the `test-build` feature).
if [ "${1:-}" = --test-build ]; then
    cargo build --locked --release -p cayenchat-ui --features test-build
    binary=target/release/cayenchat
    bundle="target/CayenChat Test.app"
    name='CayenChat Test'
    identifier=dev.cayenchat.test
else
    cargo build --locked -p cayenchat-ui
    binary=target/debug/cayenchat
    bundle=target/CayenChat.app
    name=CayenChat
    identifier=dev.cayenchat.bootstrap
fi
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
mkdir -p "$bundle/Contents/Resources/locales"
cp "$binary" "$bundle/Contents/MacOS/cayenchat"
cp crates/ui/resources/macos/CayenChat.icns "$bundle/Contents/Resources/CayenChat.icns"
cp locales/*.json "$bundle/Contents/Resources/locales/"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>cayenchat</string>
<key>CFBundleIdentifier</key><string>$identifier</string>
<key>CFBundleName</key><string>$name</string>
<key>CFBundleIconFile</key><string>CayenChat</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
printf 'Development bundle: %s\n' "$bundle"
