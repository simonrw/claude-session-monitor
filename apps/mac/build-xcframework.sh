#!/usr/bin/env bash
#
# Build an XCFramework for `csm-core-ffi` and drop the UniFFI-generated Swift
# source into the SwiftPM tree so `swift build` / `swift test` can link it.
#
# Produces:
#   apps/mac/Frameworks/csm_coreFFI.xcframework
#   apps/mac/Sources/CsmCore/csm_core.swift   (generated bindings, committed? no — see .gitignore)
#
# Usage:
#   build-xcframework.sh [--platforms mac|ios|all]
#
#   --platforms mac  build only macOS slices (arm64 + x86_64, lipo'd universal)
#   --platforms ios  build only iOS slices (device arm64 + simulator arm64)
#   --platforms all  build every slice (default)
#
# Prereqs: Xcode + rust with the Apple targets installed. Run:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim
# first.

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

PLATFORMS=all

while [[ $# -gt 0 ]]; do
    case "$1" in
        --platforms)
            PLATFORMS="${2:-}"
            shift 2
            ;;
        --platforms=*)
            PLATFORMS="${1#*=}"
            shift
            ;;
        -h|--help)
            sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

case "$PLATFORMS" in
    mac|ios|all) ;;
    *)
        echo "--platforms must be one of: mac, ios, all (got '$PLATFORMS')" >&2
        exit 2
        ;;
esac

BUILD_MAC=0
BUILD_IOS=0
[[ "$PLATFORMS" == "mac" || "$PLATFORMS" == "all" ]] && BUILD_MAC=1
[[ "$PLATFORMS" == "ios" || "$PLATFORMS" == "all" ]] && BUILD_IOS=1

rm -rf "$BUILD_DIR" "$FRAMEWORK_DIR/$MODULE_NAME.xcframework"
mkdir -p "$BUILD_DIR" "$FRAMEWORK_DIR"

# Build the crate for one rust target.
cargo_build_target() {
    local target="$1"
    echo "building $CRATE for $target"
    (cd "$REPO_ROOT" && cargo build --release -p "$CRATE" --target "$target")
}

# Runs uniffi-bindgen against the named target's dylib, copies the generated
# Swift into the SwiftPM tree and the C header + modulemap into
# $BUILD_DIR/headers. Sets $HEADERS_DIR on success (avoids using stdout so
# progress `echo`s don't pollute command substitution).
#
# All slices share the same Swift + header + modulemap, so xcodebuild's
# -headers flag can point at the same directory for every -library.
prepare_bindings() {
    local bindgen_target="$1"
    echo "generating Swift bindings from the compiled cdylib ($bindgen_target)"
    local dylib_path="$TARGET_DIR/$bindgen_target/$PROFILE/$LIB_BASENAME.dylib"
    local bindings_tmp="$BUILD_DIR/bindings"
    mkdir -p "$bindings_tmp"
    (cd "$REPO_ROOT" && cargo run --bin uniffi-bindgen -- \
        generate --library "$dylib_path" --language swift --out-dir "$bindings_tmp")

    # The generated Swift file goes into the SwiftPM CsmCore target. The header
    # + modulemap go into the XCFramework so Swift can find the C symbols.
    mkdir -p "$BINDINGS_OUT"
    cp "$bindings_tmp/csm_core.swift" "$BINDINGS_OUT/csm_core.swift"

    HEADERS_DIR="$BUILD_DIR/headers"
    mkdir -p "$HEADERS_DIR"
    cp "$bindings_tmp/csm_coreFFI.h" "$HEADERS_DIR/"
    # xcodebuild wants the modulemap named `module.modulemap` in the headers dir
    cp "$bindings_tmp/csm_coreFFI.modulemap" "$HEADERS_DIR/module.modulemap"
}

XCFRAMEWORK_ARGS=()

if [[ "$BUILD_MAC" == 1 ]]; then
    cargo_build_target aarch64-apple-darwin
    cargo_build_target x86_64-apple-darwin

    AARCH64_LIB="$TARGET_DIR/aarch64-apple-darwin/$PROFILE/$LIB_BASENAME.a"
    X86_64_LIB="$TARGET_DIR/x86_64-apple-darwin/$PROFILE/$LIB_BASENAME.a"
    MAC_UNIVERSAL_LIB="$BUILD_DIR/macos/$LIB_BASENAME.a"
    mkdir -p "$(dirname "$MAC_UNIVERSAL_LIB")"

    echo "lipo-ing macOS universal static lib"
    lipo -create "$AARCH64_LIB" "$X86_64_LIB" -output "$MAC_UNIVERSAL_LIB"
fi

if [[ "$BUILD_IOS" == 1 ]]; then
    # Pin the iOS deployment target so rustc and C sub-builds (aws-lc-sys,
    # ring) agree. Without this, rustc links against iOS 10 while the C
    # dependencies build against the SDK's current iOS, producing
    # `___chkstk_darwin` undefined-symbol errors. Matches the Swift app's
    # IPHONEOS_DEPLOYMENT_TARGET.
    export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"

    cargo_build_target aarch64-apple-ios
    cargo_build_target aarch64-apple-ios-sim

    IOS_DEVICE_LIB="$TARGET_DIR/aarch64-apple-ios/$PROFILE/$LIB_BASENAME.a"
    IOS_SIM_LIB="$TARGET_DIR/aarch64-apple-ios-sim/$PROFILE/$LIB_BASENAME.a"
fi

# Bindings are generated once, from a macOS dylib — uniffi-bindgen's
# library-mode needs a `.dylib` and iOS rustc targets don't produce one that's
# loadable on the host. When --platforms=ios is requested we build a macOS
# slice transiently just so bindgen has something to read.
HEADERS_DIR=""
if [[ "$BUILD_MAC" == 1 ]]; then
    prepare_bindings aarch64-apple-darwin
else
    echo "building $CRATE for aarch64-apple-darwin (bindgen only)"
    (cd "$REPO_ROOT" && cargo build --release -p "$CRATE" --target aarch64-apple-darwin)
    prepare_bindings aarch64-apple-darwin
fi

if [[ "$BUILD_MAC" == 1 ]]; then
    XCFRAMEWORK_ARGS+=(-library "$MAC_UNIVERSAL_LIB" -headers "$HEADERS_DIR")
fi
if [[ "$BUILD_IOS" == 1 ]]; then
    XCFRAMEWORK_ARGS+=(-library "$IOS_DEVICE_LIB" -headers "$HEADERS_DIR")
    XCFRAMEWORK_ARGS+=(-library "$IOS_SIM_LIB" -headers "$HEADERS_DIR")
fi

echo "wrapping as $MODULE_NAME.xcframework (platforms=$PLATFORMS)"
xcodebuild -create-xcframework \
    "${XCFRAMEWORK_ARGS[@]}" \
    -output "$FRAMEWORK_DIR/$MODULE_NAME.xcframework"

echo
echo "done: $FRAMEWORK_DIR/$MODULE_NAME.xcframework"
echo "     $BINDINGS_OUT/csm_core.swift"
