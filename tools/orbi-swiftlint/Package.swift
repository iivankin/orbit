// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "orbi-swiftlint",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "OrbiSwiftLintFFI", type: .dynamic, targets: ["OrbiSwiftLintFFI"]),
    ],
    dependencies: [
        .package(url: "https://github.com/realm/SwiftLint.git", exact: "0.63.2"),
    ],
    targets: [
        .target(
            name: "OrbiSwiftLintFFI",
            dependencies: [
                .product(name: "SwiftLintFramework", package: "SwiftLint"),
            ]
        ),
    ]
)
