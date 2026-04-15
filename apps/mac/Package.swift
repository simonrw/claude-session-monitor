// swift-tools-version: 5.9
import PackageDescription

// Binary target points at the XCFramework produced by build-xcframework.sh.
// The script must run before `swift build` / `swift test`. CI does this in
// .github/workflows/ci.yml; locally, run `./build-xcframework.sh` once.
let package = Package(
    name: "CsmCore",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "CsmCore", targets: ["CsmCore"]),
        .executable(name: "csmctl", targets: ["csmctl"]),
        .executable(name: "CsmMac", targets: ["CsmMac"]),
    ],
    targets: [
        // The binary target name must match the UniFFI-generated modulemap
        // module name (`csm_coreFFI`) so the generated `csm_core.swift`'s
        // `import csm_coreFFI` resolves.
        .binaryTarget(
            name: "csm_coreFFI",
            path: "Frameworks/csm_coreFFI.xcframework"
        ),
        .target(
            name: "CsmCore",
            dependencies: ["csm_coreFFI"],
            path: "Sources/CsmCore"
        ),
        .executableTarget(
            name: "csmctl",
            dependencies: ["CsmCore"],
            path: "Sources/csmctl"
        ),
        // AppKit menu-bar app. SwiftPM alone can't produce a .app bundle;
        // build-app.sh wraps the executable + Info.plist (with
        // LSUIElement=YES) into apps/mac/build/CsmMac.app.
        .executableTarget(
            name: "CsmMac",
            dependencies: ["CsmCore"],
            path: "Sources/CsmMac"
        ),
        .testTarget(
            name: "CsmCoreTests",
            dependencies: ["CsmCore"],
            path: "Tests/CsmCoreTests"
        ),
        .testTarget(
            name: "CsmMacTests",
            dependencies: ["CsmMac"],
            path: "Tests/CsmMacTests"
        ),
    ]
)
