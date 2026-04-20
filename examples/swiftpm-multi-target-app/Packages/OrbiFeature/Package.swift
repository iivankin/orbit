// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "OrbiFeaturePackage",
    products: [
        .library(name: "OrbiFeature", targets: ["OrbiFeature"])
    ],
    targets: [
        .target(name: "OrbiCore"),
        .target(name: "OrbiFeature", dependencies: ["OrbiCore"])
    ]
)
