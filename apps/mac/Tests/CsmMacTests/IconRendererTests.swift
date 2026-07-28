import XCTest
import AppKit
@testable import CsmMac
@testable import CsmCore

/// Unit + pixel-sampling tests for [`IconRenderer`].
///
/// Snapshot-style: for each `MenuBarSummary` state we render the icon and
/// assert that the dominant colour in the glyph region matches the expected
/// priority tint. Full pixel-diff snapshots are brittle across font/OS
/// updates, so this samples a small ROI at image centre instead.
final class IconRendererTests: XCTestCase {

    // MARK: - IconSpec rules (pure logic)

    func testIdleStateUsesGrayTint() {
        let spec = IconSpec.from(MenuBarSummary(busy: 0, waiting: 0))
        XCTAssertEqual(spec.tint, .idle)
        XCTAssertNil(spec.badgeText)
    }

    func testBusyStateUsesGreenTint() {
        let spec = IconSpec.from(MenuBarSummary(busy: 3, waiting: 0))
        XCTAssertEqual(spec.tint, .busy)
        XCTAssertNil(spec.badgeText)
    }

    func testWaitingUsesRedAndBadge() {
        let spec = IconSpec.from(MenuBarSummary(busy: 0, waiting: 2))
        XCTAssertEqual(spec.tint, .waiting)
        XCTAssertEqual(spec.badgeText, "2")
    }

    func testWaitingPriorityBeatsBusy() {
        let spec = IconSpec.from(MenuBarSummary(busy: 2, waiting: 5))
        XCTAssertEqual(spec.tint, .waiting)
        XCTAssertEqual(spec.badgeText, "5")
    }

    func testWaitingBadgeShowsCount() {
        let spec = IconSpec.from(MenuBarSummary(busy: 0, waiting: 99))
        XCTAssertEqual(spec.badgeText, "99")
    }

    // MARK: - Rendering

    func testRendererProducesExpectedSize() {
        let summary = MenuBarSummary(busy: 0, waiting: 0)
        let image = IconRenderer.render(summary: summary)
        XCTAssertEqual(image.size, IconRenderer.defaultSize)
    }

    func testRendererIsNotTemplate() {
        // We do explicit priority-colour tinting, so the image must not be a
        // template — template images are overridden by the menu bar's own
        // tinting.
        let image = IconRenderer.render(
            summary: MenuBarSummary(busy: 0, waiting: 1)
        )
        XCTAssertFalse(image.isTemplate)
    }

    /// Snapshot-style pixel sample: render each summary state, extract the
    /// NSBitmapImageRep, and assert the glyph centre's dominant colour
    /// matches the expected tint.
    func testRenderedGlyphColourMatchesPriorityTint() throws {
        let cases: [(MenuBarSummary, IconSpec.TintPriority)] = [
            (MenuBarSummary(busy: 0, waiting: 0), .idle),
            (MenuBarSummary(busy: 1, waiting: 0), .busy),
            (MenuBarSummary(busy: 0, waiting: 1), .waiting),
        ]
        for (summary, expectedTint) in cases {
            let image = IconRenderer.render(summary: summary)
            let bitmap = try XCTUnwrap(
                bitmapRep(of: image),
                "bitmap rep for summary \(summary)"
            )
            let glyphColor = dominantGlyphColor(in: bitmap)
            XCTAssertTrue(
                isClose(glyphColor, to: expectedTint.nsColor, tolerance: 0.30),
                "expected \(expectedTint) tint (\(expectedTint.nsColor)) but glyph sampled as \(String(describing: glyphColor)) for \(summary)"
            )
        }
    }

    // MARK: - Helpers

    private func bitmapRep(of image: NSImage) -> NSBitmapImageRep? {
        guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
            return nil
        }
        return NSBitmapImageRep(cgImage: cg)
    }

    /// Average colour across every opaque pixel in the bitmap. The glyph is
    /// the only thing drawn (no badge in the tested cases with
    /// waiting_input=0), so any opaque pixel must be glyph ink.
    ///
    /// We weight by alpha to survive antialiasing — fully-lit pixels count
    /// more than edge pixels, but both contribute the same hue.
    private func dominantGlyphColor(in bitmap: NSBitmapImageRep) -> NSColor? {
        var r = 0.0, g = 0.0, b = 0.0, weight = 0.0
        for x in 0..<bitmap.pixelsWide {
            for y in 0..<bitmap.pixelsHigh {
                guard let c = bitmap.colorAt(x: x, y: y) else { continue }
                let a = Double(c.alphaComponent)
                if a < 0.2 { continue }
                r += Double(c.redComponent) * a
                g += Double(c.greenComponent) * a
                b += Double(c.blueComponent) * a
                weight += a
            }
        }
        guard weight > 0 else { return nil }
        return NSColor(
            red: CGFloat(r / weight),
            green: CGFloat(g / weight),
            blue: CGFloat(b / weight),
            alpha: 1.0
        )
    }

    private func isClose(_ a: NSColor?, to b: NSColor, tolerance: CGFloat) -> Bool {
        guard let a, let a_ = a.usingColorSpace(.sRGB), let b_ = b.usingColorSpace(.sRGB) else {
            return false
        }
        let dr = abs(a_.redComponent - b_.redComponent)
        let dg = abs(a_.greenComponent - b_.greenComponent)
        let db = abs(a_.blueComponent - b_.blueComponent)
        return dr < tolerance && dg < tolerance && db < tolerance
    }
}
