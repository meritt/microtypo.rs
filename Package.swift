// swift-tools-version: 6.4

import PackageDescription

let release = (
    url: "https://github.com/meritt/microtypo.rs/releases/download/v0.2.2/microtypo-v0.2.2-xcframework.zip",
    checksum: "ecbaaebaf7d5d640b6b8e68156993aa49096cd6825d4b111119d1b269238f772"
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
