import OSLog
import SwiftUI

private let appLogger = Logger(subsystem: "dev.orbi.examples.macos", category: "app")
private let fixtureLogger = Logger(subsystem: "dev.orbi.examples.macos", category: "fixture")

@main
struct ExampleMacApp: App {
    @StateObject private var automationMenu = AutomationMenuState()

    init() {
        appLogger.info("ExampleMacApp launched")
        print("ExampleMacApp print launched")
    }

    var body: some Scene {
        WindowGroup {
            FixtureView()
                .environmentObject(automationMenu)
        }
        .commands {
            CommandMenu("Automation") {
            Button("Trigger Shortcut") {
                automationMenu.status = "Menu action triggered"
                appLogger.info("Automation menu shortcut triggered")
            }
            .keyboardShortcut("k", modifiers: [.command, .shift])
        }
    }
}
}

final class AutomationMenuState: ObservableObject {
    @Published var status = "Awaiting menu action"
}

private struct FixtureView: View {
    @State private var name = ""
    @State private var greeting = "Waiting for input"
    @State private var secondaryClickStatus = "Awaiting secondary click"
    @State private var dropStatus = "Drop target idle"
    @State private var hoverStatus = "Awaiting hover"

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Orbi macOS fixture")
                .font(.title2.bold())
                .accessibilityIdentifier("fixture-title")

            Text("Drive this app with the Orbi macOS UI backend.")
                .foregroundStyle(.secondary)

            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .frame(width: 280)
                .accessibilityIdentifier("name-field")

            Button("Apply") {
                let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
                greeting = trimmed.isEmpty ? "Waiting for input" : "Hello, \(trimmed)"
                fixtureLogger.info("Apply tapped with name: \(trimmed, privacy: .public)")
            }
            .keyboardShortcut(.defaultAction)
            .accessibilityIdentifier("apply-button")

            Text(greeting)
                .font(.headline)
                .accessibilityIdentifier("greeting-label")

            Divider()

            HStack(alignment: .top, spacing: 20) {
                SecondaryClickFixture(status: $secondaryClickStatus)
                DragAndDropFixture(status: $dropStatus)
            }

            Divider()

            HStack(alignment: .top, spacing: 20) {
                HoverFixture(status: $hoverStatus)
                AutomationMenuFixture()
            }

            Divider()

            PersistenceFixture()

            Divider()

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(1...18, id: \.self) { index in
                        Text("Fixture Row \(index)")
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 4)
                    }

                    Text("Automation Footer")
                        .font(.headline)
                        .accessibilityIdentifier("scroll-footer")
                        .padding(.top, 8)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(width: 320, height: 140)
            .accessibilityIdentifier("fixture-scroll")
        }
        .padding(24)
        .frame(minWidth: 620, minHeight: 760)
        .onAppear {
            fixtureLogger.info("FixtureView appeared")
            print("FixtureView print appeared")
        }
    }
}
