// swift-tools-version: 6.2

import Foundation
import PackageDescription

let release = (
    url: "https://github.com/meritt/microtypo.rs/releases/download/v0.1.0/microtypo-v0.1.0-xcframework.zip",
    checksum: "7f715fcc140c0e5d1202e93b3cc56e45934ca57f5e638a5f81f27c0b04befa4f"
)

let local = "packages/swift/MicroTypoFFI.xcframework"

let ffi: Target =
    FileManager.default.fileExists(atPath: Context.packageDirectory + "/" + local)
    ? .binaryTarget(name: "MicroTypoFFI", path: local)
    : .binaryTarget(name: "MicroTypoFFI", url: release.url, checksum: release.checksum)

let package = Package(
    name: "MicroTypo",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "MicroTypo", targets: ["MicroTypo"])
    ],
    targets: [
        ffi,
        .target(name: "MicroTypo", dependencies: ["MicroTypoFFI"], path: "packages/swift/Sources/MicroTypo"),
        .testTarget(name: "MicroTypoTests", dependencies: ["MicroTypo"], path: "packages/swift/Tests/MicroTypoTests"),
    ]
)
