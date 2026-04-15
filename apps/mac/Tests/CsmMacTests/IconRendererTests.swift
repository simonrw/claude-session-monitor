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
        let spec = IconSpec.from(MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 0))
        XCTAssertEqual(spec.tint, .idle)
        XCTAssertNil(spec.badgeText)
    }

    func testWorkingStateUsesGreenTint() {
        let spec = IconSpec.from(MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 3))
        XCTAssertEqual(spec.tint, .working)
        XCTAssertNil(spec.badgeText)
    }

    func testWaitingInputUsesYellowAndBadge() {
        let spec = IconSpec.from(MenuBarSummary(waitingInput: 2, waitingPermission: 0, working: 0))
        XCTAssertEqual(spec.tint, .waitingInput)
        XCTAssertEqual(spec.badgeText, "2")
    }

    func testWaitingPermissionUsesRed() {
        let spec = IconSpec.from(MenuBarSummary(waitingInput: 0, waitingPermission: 1, working: 0))
        XCTAssertEqual(spec.tint, .waitingPermission)
        XCTAssertNil(spec.badgeText)
    }

    func testWaitingPermissionPriorityBeatsWaitingInput() {
        let spec = IconSpec.from(MenuBarSummary(waitingInput: 5, waitingPermission: 1, working: 2))
        XCTAssertEqual(spec.tint, .waitingPermission)
        // Badge still shows the waiting_input count — orthogonal to tint.
        XCTAssertEqual(spec.badgeText, "5")
    }

    func testWaitingInputBadgeShowsCount() {
        let spec = IconSpec.from(MenuBarSummary(waitingInput: 99, waitingPermission: 0, working: 0))
        XCTAssertEqual(spec.badgeText, "99")
    }

    // MARK: - Rendering

    func testRendererProducesExpectedSize() {
        let summary = MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 0)
        let image = IconRenderer.render(summary: summary)
        XCTAssertEqual(image.size, IconRenderer.defaultSize)
    }

    func testRendererIsNotTemplate() {
        // We do explicit priority-colour tinting, so the image must not be a
        // template — template images are overridden by the menu bar's own
        // tinting.
        let image = IconRenderer.render(
            summary: MenuBarSummary(waitingInput: 0, waitingPermission: 1, working: 0)
        )
        XCTAssertFalse(image.isTemplate)
    }

    /// Snapshot-style pixel sample: render each summary state, extract the
    /// NSBitmapImageRep, and assert the glyph centre's dominant colour
    /// matches the expected tint.
    func testRenderedGlyphColourMatchesPriorityTint() throws {
        let cases: [(MenuBarSummary, IconSpec.TintPriority)] = [
            (MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 0), .idle),
            (MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 1), .working),
            (MenuBarSummary(waitingInput: 1, waitingPermission: 0, working: 0), .waitingInput),
            (MenuBarSummary(waitingInput: 0, waitingPermission: 1, working: 0), .waitingPermission),
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

    /// Sample a 4x4 region at the centre of the bitmap and return the
    /// average colour, skipping fully-transparent pixels.
    private func dominantGlyphColor(in bitmap: NSBitmapImageRep) -> NSColor? {
        let cx = bitmap.pixelsWide / 2
        let cy = bitmap.pixelsHigh / 2
        var r = 0.0, g = 0.0, b = 0.0, count = 0.0
        for dx in -2...1 {
            for dy in -2...1 {
                guard let c = bitmap.colorAt(x: cx + dx, y: cy + dy) else { continue }
                if c.alphaComponent < 0.1 { continue }
                r += Double(c.redComponent)
                g += Double(c.greenComponent)
                b += Double(c.blueComponent)
                count += 1
            }
        }
        guard count > 0 else { return nil }
        return NSColor(
            red: CGFloat(r / count),
            green: CGFloat(g / count),
            blue: CGFloat(b / count),
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
