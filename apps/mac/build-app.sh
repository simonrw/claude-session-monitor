#!/usr/bin/env bash
#
# Assemble CsmMac.app from the SwiftPM-built executable + Info.plist.
#
# Depends on build-xcframework.sh having run first (so the CsmCoreFFI binary
# target is in place).
#
# Output: apps/mac/build/CsmMac.app

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME=CsmMac
PRODUCT_NAME=CsmMac
BUILD_DIR="$HERE/build"
APP_DIR="$BUILD_DIR/$APP_NAME.app"

echo "[1/3] building $PRODUCT_NAME (release) with SwiftPM"
(cd "$HERE" && swift build --configuration release --product "$PRODUCT_NAME")

EXECUTABLE="$(cd "$HERE" && swift build --configuration release --show-bin-path)/$PRODUCT_NAME"
if [ ! -x "$EXECUTABLE" ]; then
    echo "error: expected executable at $EXECUTABLE" >&2
    exit 1
fi

echo "[2/3] assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$EXECUTABLE" "$APP_DIR/Contents/MacOS/$APP_NAME"
cp "$HERE/Resources/Info.plist" "$APP_DIR/Contents/Info.plist"

echo "[3/3] validating bundle"
# `plutil -lint` rejects malformed plists. `LSUIElement` must be present or
# the dock will still show the app.
plutil -lint "$APP_DIR/Contents/Info.plist"
if ! /usr/libexec/PlistBuddy -c "Print :LSUIElement" "$APP_DIR/Contents/Info.plist" | grep -q true; then
    echo "error: LSUIElement is not true in Info.plist" >&2
    exit 1
fi

echo
echo "done: $APP_DIR"
