// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "orbi-swift-format",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "OrbiSwiftFormatFFI", type: .dynamic, targets: ["OrbiSwiftFormatFFI"]),
    ],
    dependencies: [
        .package(url: "https://github.com/swiftlang/swift-format.git", exact: "602.0.0"),
    ],
    targets: [
        .target(
            name: "OrbiSwiftFormatFFI",
            dependencies: [
                .product(name: "SwiftFormat", package: "swift-format"),
            ]
        ),
    ]
)
