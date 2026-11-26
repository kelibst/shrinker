#!/usr/bin/env bash
# Build Shrinker.app and a drag-to-Applications disk image (macOS only).
#
#   packaging/macos/build-app.sh <path/to/shrinker-binary> <version>
#
# Writes dist/Shrinker.app and dist/Shrinker-<version>-macos.dmg.
# The app has no Dock icon (LSUIElement): it opens a page in the browser,
# which has a Quit button.
set -euo pipefail

BIN="$1"
VERSION="$2"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/Shrinker.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/shrinker"
chmod 755 "$APP/Contents/MacOS/shrinker"
cp "$ROOT/packaging/icons/shrinker.icns" "$APP/Contents/Resources/shrinker.icns"
sed "s/__VERSION__/$VERSION/g" "$ROOT/packaging/macos/Info.plist" > "$APP/Contents/Info.plist"

# Ad-hoc signature: lets Apple-chip Macs run it; Gatekeeper still asks the first
# time (right-click → Open), because it isn't notarised by Apple.
codesign --force --deep --sign - "$APP"

STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp "$ROOT/README.md" "$STAGE/README.md"
hdiutil create -volname "Shrinker $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DIST/Shrinker-$VERSION-macos.dmg"
rm -rf "$STAGE"
echo "Built $DIST/Shrinker-$VERSION-macos.dmg"
