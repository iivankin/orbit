// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "OrbiGreeting",
    products: [
        .library(name: "OrbiGreeting", targets: ["OrbiGreeting"]),
    ],
    targets: [
        .target(name: "OrbiGreeting"),
    ]
)
