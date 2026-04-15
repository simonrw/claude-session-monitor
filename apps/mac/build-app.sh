#!/usr/bin/env bash
#
# Assemble CsmMac.app by driving the Xcode project at apps/mac/CsmCore.xcodeproj.
#
# Depends on:
#   - build-xcframework.sh having run first (produces Frameworks/csm_coreFFI.xcframework
#     and Sources/CsmCore/csm_core.swift)
#   - xcodegen installed (`brew install xcodegen`)
#
# Output: apps/mac/build/CsmMac.app

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT="$HERE/CsmCore.xcodeproj"
SCHEME=CsmMac
CONFIG=Release
BUILD_DIR="$HERE/build"
DERIVED="$BUILD_DIR/DerivedData"
APP_OUT="$BUILD_DIR/CsmMac.app"

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "error: xcodegen is not installed; run 'brew install xcodegen'" >&2
    exit 1
fi

if [ ! -d "$HERE/Frameworks/csm_coreFFI.xcframework" ]; then
    echo "error: csm_coreFFI.xcframework missing — run build-xcframework.sh first" >&2
    exit 1
fi

echo "[1/4] regenerating Xcode project via xcodegen"
(cd "$HERE" && xcodegen generate)

echo "[2/4] xcodebuild $SCHEME ($CONFIG)"
xcodebuild \
    -project "$PROJECT" \
    -scheme "$SCHEME" \
    -configuration "$CONFIG" \
    -derivedDataPath "$DERIVED" \
    -destination 'platform=macOS' \
    build | xcbeautify 2>/dev/null || \
xcodebuild \
    -project "$PROJECT" \
    -scheme "$SCHEME" \
    -configuration "$CONFIG" \
    -derivedDataPath "$DERIVED" \
    -destination 'platform=macOS' \
    build

BUILT_APP="$DERIVED/Build/Products/$CONFIG/CsmMac.app"
if [ ! -d "$BUILT_APP" ]; then
    echo "error: expected .app at $BUILT_APP" >&2
    exit 1
fi

echo "[3/4] copying to $APP_OUT"
rm -rf "$APP_OUT"
cp -R "$BUILT_APP" "$APP_OUT"

echo "[4/4] validating bundle"
plutil -lint "$APP_OUT/Contents/Info.plist"
if ! /usr/libexec/PlistBuddy -c "Print :LSUIElement" "$APP_OUT/Contents/Info.plist" | grep -q true; then
    echo "error: LSUIElement is not true in Info.plist" >&2
    exit 1
fi

echo
echo "done: $APP_OUT"
