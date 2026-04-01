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

struct KeyboardShortcutFixture: View {
    @Binding var status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Keyboard Shortcut")
                .font(.headline)

            KeyboardShortcutCaptureView {
                status = "Shortcut triggered"
            }
            .frame(width: 180, height: 84)

            Text(status)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("shortcut-status")
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

private struct KeyboardShortcutCaptureView: NSViewRepresentable {
    let onShortcut: () -> Void

    func makeNSView(context: Context) -> KeyboardShortcutTargetView {
        let view = KeyboardShortcutTargetView()
        view.onShortcut = onShortcut
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.group)
        view.setAccessibilityLabel("Shortcut capture area")
        view.setAccessibilityIdentifier("shortcut-capture-area")
        return view
    }

    func updateNSView(_ nsView: KeyboardShortcutTargetView, context: Context) {
        nsView.onShortcut = onShortcut
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

private final class KeyboardShortcutTargetView: NSView {
    var onShortcut: (() -> Void)?
    private var keyMonitor: Any?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = NSColor.systemIndigo.withAlphaComponent(0.12).cgColor
        layer?.cornerRadius = 12
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.systemIndigo.withAlphaComponent(0.35).cgColor
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    override var acceptsFirstResponder: Bool {
        true
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)

        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 13, weight: .semibold),
            .foregroundColor: NSColor.labelColor,
        ]
        let text = "Focus me, then press Command-Shift-K"
        let size = text.size(withAttributes: attributes)
        let rect = NSRect(
            x: bounds.midX - (size.width / 2),
            y: bounds.midY - (size.height / 2),
            width: size.width,
            height: size.height
        )
        text.draw(in: rect, withAttributes: attributes)
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        // The fixture owns keyboard verification, so it eagerly claims first responder
        // when the test window appears instead of depending on SwiftUI shortcut routing.
        DispatchQueue.main.async { [weak self] in
            guard let self else {
                return
            }
            self.window?.makeFirstResponder(self)
        }
        installKeyMonitor()
    }

    override func keyDown(with event: NSEvent) {
        if matchesShortcut(event) {
            onShortcut?()
            return
        }
        super.keyDown(with: event)
    }

    deinit {
        if let keyMonitor {
            NSEvent.removeMonitor(keyMonitor)
        }
    }

    private func installKeyMonitor() {
        guard keyMonitor == nil else {
            return
        }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self else {
                return event
            }
            if self.matchesShortcut(event) {
                self.onShortcut?()
            }
            return event
        }
    }

    private func matchesShortcut(_ event: NSEvent) -> Bool {
        let requiredFlags: NSEvent.ModifierFlags = [.command, .shift]
        let activeFlags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        return activeFlags.isSuperset(of: requiredFlags)
            && event.charactersIgnoringModifiers?.lowercased() == "k"
    }
}
