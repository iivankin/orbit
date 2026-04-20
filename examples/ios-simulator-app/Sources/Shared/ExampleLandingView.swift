import OSLog
import OrbitGreeting
import SwiftUI

struct ExampleLandingView: View {
    private static let logger = Logger(
        subsystem: "dev.orbit.examples.exampleiosapp",
        category: "Landing"
    )
    @State private var currentPage = 0
    @State private var email = ""
    @State private var password = ""
    @State private var statusMessage = "Waiting for input"

    var body: some View {
        NavigationStack {
            TabView(selection: $currentPage) {
                introPage
                    .tag(0)
                formPage
                    .tag(1)
            }
            .tabViewStyle(.page(indexDisplayMode: .always))
            .background(
                LinearGradient(
                    colors: [
                        .indigo.opacity(0.18),
                        .mint.opacity(0.10),
                        Color(uiColor: .systemBackground),
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            )
            .onAppear {
                Self.logger.notice("ExampleLandingView appeared")
            }
        }
    }

    private var introPage: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "sparkles.rectangle.stack.fill")
                .font(.system(size: 56))
                .foregroundStyle(Color("AccentColor"))
                .symbolRenderingMode(.hierarchical)
            Text("Orbit UI Demo")
                .font(.largeTitle.bold())
            Text("Swipe once to open the form screen that the JSON flow drives.")
                .font(.headline)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Text(OrbitGreeting.headline)
                .font(.subheadline.weight(.medium))
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Button(action: openForm) {
                Text("Open Form")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            NavigationLink("Open Swipe Lab") {
                SwipeLabView()
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            Spacer()
            Text("Swipe left")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(.secondary)
        }
        .padding(32)
    }

    private var formPage: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Account Details")
                    .font(.title.bold())
                Text("This screen is intentionally simple so Orbit UI flows can target stable accessibility labels.")
                    .foregroundStyle(.secondary)

                VStack(spacing: 14) {
                    TextField("Email", text: $email)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .textContentType(.emailAddress)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                        .background(.thinMaterial, in: .rect(cornerRadius: 16))

                    SecureField("Password", text: $password)
                        .textContentType(.password)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                        .background(.thinMaterial, in: .rect(cornerRadius: 16))
                }

                Button(action: submit) {
                    Text("Continue")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

                Text(statusMessage)
                    .font(.headline)
                    .foregroundStyle(statusMessage == "Ready for automation" ? .green : .secondary)

                Text("Use scrollUntilVisible to reach the footer card.")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(.secondary)

                Color.clear
                    .frame(height: 520)

                Text("Automation Footer")
                    .font(.title3.bold())
                Text("This label starts off screen and exists to validate the scrollUntilVisible command.")
                    .foregroundStyle(.secondary)
            }
            .padding(32)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
    }

    private func submit() {
        statusMessage = "Ready for automation"
        Self.logger.notice("Continue tapped; status updated to ready")
    }

    private func openForm() {
        currentPage = 1
        Self.logger.notice("Open Form tapped; switched to page \(self.currentPage)")
    }
}
