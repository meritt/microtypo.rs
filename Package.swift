// swift-tools-version: 6.4

import PackageDescription

let release = (
    url: "https://github.com/meritt/microtypo.rs/releases/download/v0.2.1/microtypo-v0.2.1-xcframework.zip",
    checksum: "4a5f2e5fa8bc9535488b4a3ac79fcc746f1eb4ef5d2dc3da78f3ea95aa242cd4"
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
