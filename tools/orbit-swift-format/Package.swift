// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "orbit-swift-format",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "OrbitSwiftFormatFFI", type: .dynamic, targets: ["OrbitSwiftFormatFFI"]),
    ],
    dependencies: [
        .package(url: "https://github.com/swiftlang/swift-format.git", exact: "602.0.0"),
    ],
    targets: [
        .target(
            name: "OrbitSwiftFormatFFI",
            dependencies: [
                .product(name: "SwiftFormat", package: "swift-format"),
            ]
        ),
    ]
)
