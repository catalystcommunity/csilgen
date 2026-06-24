// swift-tools-version:6.0
//
// TODO (publishing): SwiftPM cannot consume a package from a subdirectory via a git
// *URL* (`Package.swift` must sit at the git root; swift-package-manager#5768 was never
// implemented). Until we either (a) add a root-level `Package.swift` that vendors
// `transports/swift` via a target `path:`, or (b) mirror this directory to its own repo
// or a Swift Package Registry, consume it with a *local path* dependency after cloning
// or submoduling csilgen: `.package(path: "../csilgen/transports/swift")`. See README.
import PackageDescription

// No `dependencies:` on the package at all — the visible proof of the zero-dep rule. The
// codec, conventions, carriers, and conformance suite touch only the Swift stdlib (no
// Foundation), so the meaningful test surface needs nothing but the Swift toolchain.
let package = Package(
    name: "CsilgenTransport",
    products: [
        .library(name: "CsilgenTransport", targets: ["CsilgenTransport"])
    ],
    targets: [
        .target(name: "CsilgenTransport"),
        .testTarget(
            name: "CsilgenTransportTests",
            dependencies: ["CsilgenTransport"],
            // The conformance vectors live outside the package tree (transports/conformance);
            // the tests reach them by walking up from #filePath, so nothing to bundle here.
            resources: []
        ),
    ]
)
