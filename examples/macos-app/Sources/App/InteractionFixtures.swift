import AppKit
import SwiftUI

struct SecondaryClickFixture: View {
    @Binding var status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Secondary Click")
                .font(.headline)

            SecondaryClickCaptureView {
                status = "Secondary click recognized"
            }
            .frame(width: 180, height: 84)

            Text(status)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("secondary-click-status")
        }
    }
}

struct DragAndDropFixture: View {
    @Binding var status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Drag And Drop")
                .font(.headline)

            HStack(spacing: 12) {
                Text("Orbit token")
                    .font(.headline)
                    .frame(width: 110, height: 84)
                    .background(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .fill(.quaternary)
                    )
                    .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .accessibilityIdentifier("drag-source")
                    .draggable("orbit-token")

                Text(status)
                    .font(.subheadline.weight(.medium))
                    .multilineTextAlignment(.center)
                    .frame(width: 150, height: 84)
                    .background(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(.secondary.opacity(0.35), lineWidth: 1)
                            .background(
                                RoundedRectangle(cornerRadius: 12, style: .continuous)
                                    .fill(.quinary)
                            )
                    )
                    .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .accessibilityIdentifier("drop-target")
                    .dropDestination(for: String.self) { items, _ in
                        guard let item = items.first else {
                            return false
                        }
                        status = "Dropped \(item)"
                        return true
                    }
            }
        }
    }
}

private struct SecondaryClickCaptureView: NSViewRepresentable {
    let onSecondaryClick: () -> Void

    func makeNSView(context: Context) -> SecondaryClickTargetView {
        let view = SecondaryClickTargetView()
        view.onSecondaryClick = onSecondaryClick
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.button)
        view.setAccessibilityLabel("Secondary click area")
        view.setAccessibilityIdentifier("secondary-click-area")
        return view
    }

    func updateNSView(_ nsView: SecondaryClickTargetView, context: Context) {
        nsView.onSecondaryClick = onSecondaryClick
    }
}

private final class SecondaryClickTargetView: NSView {
    var onSecondaryClick: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = NSColor.quaternaryLabelColor.withAlphaComponent(0.2).cgColor
        layer?.cornerRadius = 12
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    override func rightMouseDown(with event: NSEvent) {
        onSecondaryClick?()
    }
}
