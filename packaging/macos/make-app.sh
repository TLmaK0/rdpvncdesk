#!/bin/sh
# Builds target/release/rdpvncdesk.app, needed on macOS to receive rdp:// links.
# Usage: packaging/macos/make-app.sh [--target <triple>]
#   then: open target/release/rdpvncdesk.app   (or copy it to /Applications)
#         target/release/rdpvncdesk.app/Contents/MacOS/rdpvncdesk --register-url-handler
set -eu
cd "$(dirname "$0")/../.."

TARGET_DIR=target/release
if [ "${1:-}" = "--target" ]; then
  cargo build --release --target "$2"
  TARGET_DIR="target/$2/release"
else
  cargo build --release
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
SCHEME=${RDPVNCDESK_SCHEME:-rdp}
BUNDLE_ID=${RDPVNCDESK_BUNDLE_ID:-io.github.tlmak0.rdpvncdesk}
APP="$TARGET_DIR/rdpvncdesk.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$TARGET_DIR/rdpvncdesk" "$APP/Contents/MacOS/rdpvncdesk"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>rdpvncdesk</string>
  <key>CFBundleDisplayName</key><string>rdpvncdesk</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>rdpvncdesk</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>Remote Desktop Protocol</string>
      <key>CFBundleURLSchemes</key><array><string>$SCHEME</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
# Ad-hoc signature so Gatekeeper/LaunchServices accept the local bundle.
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
echo "Built $APP"
