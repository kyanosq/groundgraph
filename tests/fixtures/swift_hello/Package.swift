// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Greeter",
    products: [
        .library(name: "Greeter", targets: ["Greeter"])
    ],
    targets: [
        .target(name: "Greeter", path: "Sources/Greeter"),
        .testTarget(name: "GreeterTests", dependencies: ["Greeter"], path: "Tests/GreeterTests"),
    ]
)
