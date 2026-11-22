// swift-tools-version: 6.4

import PackageDescription

let release = (
    url: "https://github.com/meritt/microtypo.rs/releases/download/v0.2.0/microtypo-v0.2.0-xcframework.zip",
    checksum: "01068f84937bad438796735bdd211397d9c2f7697a950d43f9b4d3c74c89417e"
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
