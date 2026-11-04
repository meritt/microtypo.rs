// swift-tools-version: 6.2

import Foundation
import PackageDescription

let release = (
    url: "https://github.com/meritt/microtypo.rs/releases/download/v0.1.0/microtypo-v0.1.0-xcframework.zip",
    checksum: "5bdaef1d31affed1c4e88b4eb5127b8e117c82a7558306eba52abea284b3eb8c"
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
