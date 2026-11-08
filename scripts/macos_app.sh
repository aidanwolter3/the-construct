#!/usr/bin/env bash
# The Construct for macOS (decision `rust-clients`): builds `construct-viewer` in release mode and wraps it in an
# app bundle and a disk image.
#
#   scripts/macos_app.sh            # build/macos/The Construct.app and build/macos/The-Construct.dmg
#
# Environment (optional): CONSTRUCT_VERSION_NAME, the bundle's version (default 0.1.0); CONSTRUCT_VERSION_CODE, its
# build number (default 1).
#
# The bundle is signed ad hoc, not with a Developer ID, and not notarized: macOS opens a downloaded copy only after
# Open from the Finder's context menu (or `xattr -dr com.apple.quarantine "/Applications/The Construct.app"`).
# A copy built on the same Mac opens as it is. Needs macOS (iconutil's .icns is checked in; hdiutil, codesign).
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $# -gt 0 ]]; then
  echo "usage: $0   (no options)" >&2
  exit 2
fi
[[ $(uname -s) == Darwin ]] || { echo "macos_app.sh runs on macOS only (hdiutil, codesign)" >&2; exit 1; }

VERSION=${CONSTRUCT_VERSION_NAME:-0.1.0}
BUILD=${CONSTRUCT_VERSION_CODE:-1}
OUT=build/macos
APP="$OUT/The Construct.app"

echo "==> building construct-viewer (release)"
cargo build -p construct-render --features viewer --release --bin construct-viewer

echo "==> bundling"
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/construct-viewer "$APP/Contents/MacOS/construct-viewer"
cp assets/icon/construct.icns "$APP/Contents/Resources/construct.icns"
# The bundle id is the old Compose desktop app's, so macOS's settings for the app (notifications, the Dock) carry
# over. An ad hoc signature changes with every build, so the Keychain asks again once after an update.
cat >"$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>The Construct</string>
  <key>CFBundleExecutable</key><string>construct-viewer</string>
  <key>CFBundleIconFile</key><string>construct</string>
  <key>CFBundleIdentifier</key><string>org.theconstruct.desktop</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>The Construct</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$BUILD</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.social-networking</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
EOF
plutil -lint "$APP/Contents/Info.plist" >/dev/null
# Ad hoc: Apple Silicon runs only signed code, and the signature seals the Info.plist and icon with the binary.
codesign --force --sign - --timestamp=none "$APP"
codesign --verify --strict "$APP"

echo "==> disk image"
STAGE="$OUT/dmg"
rm -rf "$STAGE" "$OUT/The-Construct.dmg" && mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "The Construct" -srcfolder "$STAGE" -ov -format UDZO "$OUT/The-Construct.dmg"
rm -rf "$STAGE"
echo "built $APP and $OUT/The-Construct.dmg"
