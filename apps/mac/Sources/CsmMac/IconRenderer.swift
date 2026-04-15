import AppKit
import CsmCore

/// Pure rules for how a [`MenuBarSummary`] maps to tint colour and badge text.
///
/// Extracted so it can be unit-tested without invoking AppKit image drawing.
/// Priority order per the PRD: red > yellow > green > gray.
struct IconSpec: Equatable {
    let tint: TintPriority
    let badgeText: String?

    enum TintPriority: Equatable {
        case waitingPermission
        case waitingInput
        case working
        case idle

        var nsColor: NSColor {
            switch self {
            case .waitingPermission: return .systemRed
            case .waitingInput: return .systemYellow
            case .working: return .systemGreen
            case .idle: return .systemGray
            }
        }
    }

    static func from(_ summary: MenuBarSummary) -> IconSpec {
        let tint: TintPriority
        if summary.waitingPermission > 0 {
            tint = .waitingPermission
        } else if summary.waitingInput > 0 {
            tint = .waitingInput
        } else if summary.working > 0 {
            tint = .working
        } else {
            tint = .idle
        }
        let badge = summary.waitingInput > 0 ? "\(summary.waitingInput)" : nil
        return IconSpec(tint: tint, badgeText: badge)
    }
}

/// Procedurally-drawn status-bar icon. See parent PRD PRO-122 §"Status-bar
/// icon" for the design rules this implements.
enum IconRenderer {
    /// Standard 22x22 menu-bar icon size.
    static let defaultSize = NSSize(width: 22, height: 22)

    /// Render an icon reflecting the summary. Called on the main thread from
    /// the StatusItemController's on_summary_changed observer callback.
    static func render(summary: MenuBarSummary, size: NSSize = defaultSize) -> NSImage {
        let spec = IconSpec.from(summary)
        let image = NSImage(size: size, flipped: false) { rect in
            drawGlyph(tint: spec.tint.nsColor, in: rect)
            if let badge = spec.badgeText {
                drawBadge(text: badge, tint: spec.tint.nsColor, in: rect)
            }
            return true
        }
        // Not a template image — we're applying priority colour explicitly,
        // which macOS would override on a template.
        image.isTemplate = false
        return image
    }

    private static func drawGlyph(tint: NSColor, in rect: NSRect) {
        let font = NSFont.systemFont(ofSize: rect.height * 0.7, weight: .bold)
        let attrs: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: tint,
        ]
        let glyph = NSAttributedString(string: "C", attributes: attrs)
        let size = glyph.size()
        let origin = NSPoint(
            x: (rect.width - size.width) / 2,
            y: (rect.height - size.height) / 2
        )
        glyph.draw(at: origin)
    }

    private static func drawBadge(text: String, tint: NSColor, in rect: NSRect) {
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 8, weight: .bold),
            .foregroundColor: NSColor.white,
        ]
        let str = NSAttributedString(string: text, attributes: attrs)
        let textSize = str.size()
        let padding: CGFloat = 2
        let pillSize = NSSize(
            width: textSize.width + padding * 2,
            height: textSize.height + padding
        )
        let pillOrigin = NSPoint(
            x: rect.width - pillSize.width,
            y: rect.height - pillSize.height
        )
        let pillRect = NSRect(origin: pillOrigin, size: pillSize)

        tint.setFill()
        NSBezierPath(
            roundedRect: pillRect,
            xRadius: pillSize.height / 2,
            yRadius: pillSize.height / 2
        ).fill()

        str.draw(at: NSPoint(
            x: pillOrigin.x + padding,
            y: pillOrigin.y + padding / 2
        ))
    }
}
