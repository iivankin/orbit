// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "OrbitFeaturePackage",
    products: [
        .library(name: "OrbitFeature", targets: ["OrbitFeature"])
    ],
    targets: [
        .target(name: "OrbitCore"),
        .target(name: "OrbitFeature", dependencies: ["OrbitCore"])
    ]
)
