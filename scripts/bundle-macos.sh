#!/bin/sh
# Local development bundle only; no signing, installation, or system changes.
set -eu
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
    echo 'This development bundle helper requires macOS.' >&2
    exit 1
fi
cargo build --locked -p cayenchat-ui
bundle=target/CayenChat.app
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/debug/cayenchat "$bundle/Contents/MacOS/cayenchat"
cp crates/ui/resources/macos/CayenChat.icns "$bundle/Contents/Resources/CayenChat.icns"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>cayenchat</string>
<key>CFBundleIdentifier</key><string>dev.cayenchat.bootstrap</string>
<key>CFBundleName</key><string>CayenChat</string>
<key>CFBundleIconFile</key><string>CayenChat</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
printf 'Development bundle: %s\n' "$bundle"
