#!/usr/bin/env bash
#
# Build an XCFramework for `csm-core-ffi` and drop the UniFFI-generated Swift
# source into the SwiftPM tree so `swift build` / `swift test` can link it.
#
# Produces:
#   apps/mac/Frameworks/CsmCoreFFI.xcframework
#   apps/mac/Sources/CsmCore/csm_core.swift   (generated bindings, committed? no — see .gitignore)
#
# Prereqs: Xcode + rust with aarch64-apple-darwin and x86_64-apple-darwin targets.
# Run `rustup target add aarch64-apple-darwin x86_64-apple-darwin` first.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

CRATE=csm-core-ffi
LIB_BASENAME=libcsm_core
# Must match the UniFFI-generated modulemap module name so `import csm_coreFFI`
# in the generated Swift file resolves. See Package.swift.
MODULE_NAME=csm_coreFFI
PROFILE=release

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
BUILD_DIR="$HERE/build"
FRAMEWORK_DIR="$HERE/Frameworks"
BINDINGS_OUT="$HERE/Sources/CsmCore"

rm -rf "$BUILD_DIR" "$FRAMEWORK_DIR/$MODULE_NAME.xcframework"
mkdir -p "$BUILD_DIR" "$FRAMEWORK_DIR"

echo "[1/5] building $CRATE for aarch64-apple-darwin"
(cd "$REPO_ROOT" && cargo build --release -p "$CRATE" --target aarch64-apple-darwin)

echo "[2/5] building $CRATE for x86_64-apple-darwin"
(cd "$REPO_ROOT" && cargo build --release -p "$CRATE" --target x86_64-apple-darwin)

AARCH64_LIB="$TARGET_DIR/aarch64-apple-darwin/$PROFILE/$LIB_BASENAME.a"
X86_64_LIB="$TARGET_DIR/x86_64-apple-darwin/$PROFILE/$LIB_BASENAME.a"
UNIVERSAL_LIB="$BUILD_DIR/$LIB_BASENAME.a"

echo "[3/5] lipo-ing universal static lib"
lipo -create "$AARCH64_LIB" "$X86_64_LIB" -output "$UNIVERSAL_LIB"

echo "[4/5] generating Swift bindings from the compiled cdylib"
# We need the dylib (not static) for library-mode uniffi-bindgen to read
# metadata. cargo build above produced it as a side-effect of crate-type.
DYLIB_PATH="$TARGET_DIR/aarch64-apple-darwin/$PROFILE/$LIB_BASENAME.dylib"
BINDINGS_TMP="$BUILD_DIR/bindings"
mkdir -p "$BINDINGS_TMP"
(cd "$REPO_ROOT" && cargo run --bin uniffi-bindgen -- \
    generate --library "$DYLIB_PATH" --language swift --out-dir "$BINDINGS_TMP")

# The generated Swift file goes into the SwiftPM CsmCore target. The header +
# modulemap go into the XCFramework so Swift can find the C symbols.
mkdir -p "$BINDINGS_OUT"
cp "$BINDINGS_TMP/csm_core.swift" "$BINDINGS_OUT/csm_core.swift"

HEADERS_DIR="$BUILD_DIR/headers"
mkdir -p "$HEADERS_DIR"
cp "$BINDINGS_TMP/csm_coreFFI.h" "$HEADERS_DIR/"
# xcodebuild wants the modulemap named `module.modulemap` in the headers dir
cp "$BINDINGS_TMP/csm_coreFFI.modulemap" "$HEADERS_DIR/module.modulemap"

echo "[5/5] wrapping as $MODULE_NAME.xcframework"
xcodebuild -create-xcframework \
    -library "$UNIVERSAL_LIB" -headers "$HEADERS_DIR" \
    -output "$FRAMEWORK_DIR/$MODULE_NAME.xcframework"

echo
echo "done: $FRAMEWORK_DIR/$MODULE_NAME.xcframework"
echo "     $BINDINGS_OUT/csm_core.swift"
