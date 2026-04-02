import AppKit
import SwiftUI

struct HoverFixture: View {
    @Binding var status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Hover")
                .font(.headline)

            HoverCaptureView {
                status = "Hover detected"
            }
            .frame(width: 180, height: 84)

            Text(status)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("hover-status")
        }
    }
}

struct AutomationMenuFixture: View {
    @EnvironmentObject private var automationMenu: AutomationMenuState

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Automation Menu")
                .font(.headline)

            ZStack {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(.indigo.opacity(0.08))
                    .overlay {
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(.indigo.opacity(0.35), lineWidth: 1)
                    }

                Text("Automation > Trigger Shortcut")
                    .font(.system(size: 13, weight: .semibold))
                    .multilineTextAlignment(.center)
                    .padding(12)
            }
            .frame(width: 180, height: 84)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Automation menu fixture")
            .accessibilityIdentifier("automation-menu-card")

            Text(automationMenu.status)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("menu-status")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct PersistenceFixture: View {
    @AppStorage("orbit.fixture.persisted-message")
    private var message = "Clean slate"

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Persisted State")
                .font(.headline)

            Button("Persist State") {
                message = "Persisted state restored"
            }
            .accessibilityIdentifier("persist-state-button")

            Text(message)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("persisted-state-label")
        }
    }
}

private struct HoverCaptureView: NSViewRepresentable {
    let onHover: () -> Void

    func makeNSView(context: Context) -> HoverTargetView {
        let view = HoverTargetView()
        view.onHover = onHover
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.group)
        view.setAccessibilityLabel("Hover area")
        view.setAccessibilityIdentifier("hover-target")
        return view
    }

    func updateNSView(_ nsView: HoverTargetView, context: Context) {
        nsView.onHover = onHover
    }
}

private final class HoverTargetView: NSView {
    var onHover: (() -> Void)?
    private var trackingArea: NSTrackingArea?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = NSColor.systemTeal.withAlphaComponent(0.12).cgColor
        layer?.cornerRadius = 12
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.systemTeal.withAlphaComponent(0.35).cgColor
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    override func updateTrackingAreas() {
        if let trackingArea {
            removeTrackingArea(trackingArea)
        }

        let trackingArea = NSTrackingArea(
            rect: bounds,
            options: [.activeAlways, .mouseEnteredAndExited, .mouseMoved, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(trackingArea)
        self.trackingArea = trackingArea
        super.updateTrackingAreas()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        window?.acceptsMouseMovedEvents = true
    }

    override func mouseEntered(with event: NSEvent) {
        onHover?()
    }

    override func mouseMoved(with event: NSEvent) {
        onHover?()
    }
}
