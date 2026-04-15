import XCTest
@testable import CsmMac
@testable import CsmCore

/// Exercises PopoverViewModel.apply(sessions:) and apply(connection:), plus
/// the SessionDisplay helpers. Covers the observer → view-model wiring
/// expected by PRO-127's acceptance criteria without spinning up a popover.
final class PopoverViewModelTests: XCTestCase {

    private func session(
        id: String,
        status: Status,
        updatedAt: Date = Date(),
        hostname: String? = nil,
        cwd: String = "/tmp",
        gitBranch: String? = nil,
        gitRemote: String? = nil
    ) -> SessionView {
        SessionView(
            sessionId: id,
            cwd: cwd,
            status: status,
            updatedAt: updatedAt,
            hostname: hostname,
            gitBranch: gitBranch,
            gitRemote: gitRemote
        )
    }

    // MARK: - apply(sessions:)

    func testApplyPartitionsWaitingFromWorking() {
        let vm = PopoverViewModel()
        vm.apply(sessions: [
            session(id: "a", status: .waiting(reason: .input, detail: nil)),
            session(id: "b", status: .working(tool: nil)),
            session(id: "c", status: .waiting(reason: .permission, detail: nil)),
            session(id: "d", status: .ended),
        ])
        XCTAssertEqual(vm.waiting.map(\.sessionId), ["a", "c"])
        XCTAssertEqual(vm.working.map(\.sessionId), ["b"])
        // Ended sessions are dropped entirely.
    }

    func testWorkingSortedByUpdatedAtDescending() {
        let vm = PopoverViewModel()
        let now = Date()
        vm.apply(sessions: [
            session(id: "old", status: .working(tool: nil), updatedAt: now.addingTimeInterval(-300)),
            session(id: "new", status: .working(tool: nil), updatedAt: now),
            session(id: "mid", status: .working(tool: nil), updatedAt: now.addingTimeInterval(-60)),
        ])
        XCTAssertEqual(vm.working.map(\.sessionId), ["new", "mid", "old"])
    }

    func testSuccessiveApplyReplacesState() {
        let vm = PopoverViewModel()
        vm.apply(sessions: [
            session(id: "a", status: .waiting(reason: .input, detail: nil)),
        ])
        vm.apply(sessions: [
            session(id: "b", status: .working(tool: nil)),
        ])
        XCTAssertEqual(vm.waiting.count, 0)
        XCTAssertEqual(vm.working.map(\.sessionId), ["b"])
    }

    func testApplyConnectionUpdatesState() {
        let vm = PopoverViewModel()
        XCTAssertEqual(vm.connection, .connecting)
        vm.apply(connection: .connected)
        XCTAssertEqual(vm.connection, .connected)
        vm.apply(connection: .disconnected)
        XCTAssertEqual(vm.connection, .disconnected)
    }

    // MARK: - SessionDisplay

    func testStatusTextFormat() {
        XCTAssertEqual(SessionDisplay.statusText(.working(tool: nil)), "working")
        XCTAssertEqual(SessionDisplay.statusText(.working(tool: "Bash")), "working(Bash)")
        XCTAssertEqual(
            SessionDisplay.statusText(.waiting(reason: .input, detail: nil)),
            "waiting(input)"
        )
        XCTAssertEqual(
            SessionDisplay.statusText(.waiting(reason: .permission, detail: "rm -rf")),
            "waiting(permission: rm -rf)"
        )
        XCTAssertEqual(SessionDisplay.statusText(.ended), "ended")
    }

    func testLocationTextWithHostAndBranchAndRemote() {
        let s = session(
            id: "x",
            status: .working(tool: nil),
            hostname: "myhost",
            cwd: "/home/simon/project",
            gitBranch: "feature/foo",
            gitRemote: "https://github.com/org/repo.git"
        )
        let text = SessionDisplay.locationText(for: s)
        // The remote prefix/suffix are stripped and the branch arrow is
        // present — regardless of $HOME.
        XCTAssertTrue(text.hasPrefix("myhost:"))
        XCTAssertTrue(text.contains("(feature/foo \u{2192} org/repo)"))
    }

    func testLocationTextStripsHomePrefix() {
        let home = ProcessInfo.processInfo.environment["HOME"] ?? "/tmp"
        let s = session(
            id: "x",
            status: .working(tool: nil),
            cwd: "\(home)/nested"
        )
        XCTAssertEqual(SessionDisplay.locationText(for: s), "~/nested")
    }

    func testRelativeTime() {
        let now = Date()
        let s1 = session(id: "a", status: .ended, updatedAt: now.addingTimeInterval(-5))
        XCTAssertEqual(SessionDisplay.relativeTime(for: s1, now: now), "5s ago")

        let s2 = session(id: "b", status: .ended, updatedAt: now.addingTimeInterval(-120))
        XCTAssertEqual(SessionDisplay.relativeTime(for: s2, now: now), "2m ago")
    }
}
