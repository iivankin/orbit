// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "orbit-swiftlint",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "OrbitSwiftLintFFI", type: .dynamic, targets: ["OrbitSwiftLintFFI"]),
    ],
    dependencies: [
        .package(url: "https://github.com/realm/SwiftLint.git", exact: "0.63.2"),
    ],
    targets: [
        .target(
            name: "OrbitSwiftLintFFI",
            dependencies: [
                .product(name: "SwiftLintFramework", package: "SwiftLint"),
            ]
        ),
    ]
)
