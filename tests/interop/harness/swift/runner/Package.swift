// swift-tools-version:6.0
//
// Swift interop harness. It links the generated package (../gen, written by the
// interop orchestrator) and the Swift transport library by local path, so the
// build needs no network. The manifest is in `runner/`, not in `swift/`: SwiftPM
// names a package by its directory, and `transports/swift` already has the name
// `swift`.
import PackageDescription

let package = Package(
    name: "csil-interop-swift",
    // `interop.csil` has `.regex` constraints, so the generated package needs
    // macOS 13, and SwiftPM refuses a root whose floor is below a dependency's.
    platforms: [.macOS(.v13)],
    dependencies: [
        // SwiftPM names a path dependency by its last path component.
        .package(path: "../gen"),
        .package(path: "../../../../../transports/swift"),
    ],
    targets: [
        .executableTarget(
            name: "csil-interop-swift",
            dependencies: [
                .product(name: "interop_api", package: "gen"),
                .product(name: "CsilgenTransport", package: "swift"),
            ]
        )
    ],
    // Swift 5 mode: the harness reads the C `stdout` global, which Swift 6 mode
    // refuses as shared mutable state.
    swiftLanguageModes: [.v5]
)
