// swift-tools-version: 6.4

import PackageDescription

let release = (
    url: "https://github.com/meritt/microtypo.rs/releases/download/v0.1.0/microtypo-v0.1.0-xcframework.zip",
    checksum: "7f715fcc140c0e5d1202e93b3cc56e45934ca57f5e638a5f81f27c0b04befa4f"
)

let settings: [SwiftSetting] = [
    .swiftLanguageMode(.v6),
    .strictMemorySafety(),
    .enableUpcomingFeature("ApproachableConcurrency"),
    .enableUpcomingFeature("InternalImportsByDefault"),
    .enableUpcomingFeature("MemberImportVisibility"),
    .enableUpcomingFeature("ExistentialAny"),
    .enableUpcomingFeature("ImmutableWeakCaptures"),
    .treatAllWarnings(as: .error),
]

let package = Package(
    name: "MicroTypo",
    platforms: [.macOS(.v27)],
    products: [
        .library(name: "MicroTypo", targets: ["MicroTypo"])
    ],
    targets: [
        .binaryTarget(name: "MicroTypoFFI", url: release.url, checksum: release.checksum),
        .target(
            name: "MicroTypo",
            dependencies: ["MicroTypoFFI"],
            path: "packages/swift/Sources/MicroTypo",
            swiftSettings: settings
        ),
        .testTarget(
            name: "MicroTypoTests",
            dependencies: ["MicroTypo"],
            path: "packages/swift/Tests/MicroTypoTests",
            swiftSettings: settings
        ),
    ]
)
