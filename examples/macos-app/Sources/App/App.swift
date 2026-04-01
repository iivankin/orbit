import SwiftUI

@main
struct ExampleMacApp: App {
    var body: some Scene {
        WindowGroup {
            FixtureView()
        }
    }
}

private struct FixtureView: View {
    @State private var name = ""
    @State private var greeting = "Waiting for input"
    @State private var secondaryClickStatus = "Awaiting secondary click"
    @State private var dropStatus = "Drop target idle"
    @State private var hoverStatus = "Awaiting hover"
    @State private var shortcutStatus = "Awaiting shortcut"

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Orbit macOS fixture")
                .font(.title2.bold())
                .accessibilityIdentifier("fixture-title")

            Text("Drive this app with the Orbit macOS UI backend.")
                .foregroundStyle(.secondary)

            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .frame(width: 280)
                .accessibilityIdentifier("name-field")

            Button("Apply") {
                let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
                greeting = trimmed.isEmpty ? "Waiting for input" : "Hello, \(trimmed)"
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
                KeyboardShortcutFixture(status: $shortcutStatus)
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
    }
}
