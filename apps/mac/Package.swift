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
        .testTarget(
            name: "CsmCoreTests",
            dependencies: ["CsmCore"],
            path: "Tests/CsmCoreTests"
        ),
    ]
)
