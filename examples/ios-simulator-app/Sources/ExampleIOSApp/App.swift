import OSLog
import SwiftUI

@main
struct ExampleIOSApp: App {
    private let logger = Logger(
        subsystem: "dev.orbit.examples.exampleiosapp",
        category: "App"
    )

    init() {
        logger.notice("ExampleIOSApp launched")
    }

    var body: some Scene {
        WindowGroup { ExampleLandingView() }
    }
}
