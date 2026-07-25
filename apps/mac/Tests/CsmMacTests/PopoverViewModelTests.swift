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
        agentKind: AgentKind = .claude,
        model: String? = nil,
        updatedAt: Date = Date(),
        hostname: String? = nil,
        cwd: String = "/tmp",
        gitBranch: String? = nil,
        gitRemote: String? = nil,
        tmuxTarget: String? = nil
    ) -> SessionView {
        SessionView(
            sessionId: id,
            cwd: cwd,
            status: status,
            agentKind: agentKind,
            model: model,
            updatedAt: updatedAt,
            hostname: hostname,
            gitBranch: gitBranch,
            gitRemote: gitRemote,
            tmuxTarget: tmuxTarget
        )
    }

    // MARK: - apply(sessions:)

    func testApplyPartitionsWaitingFromEverythingElse() {
        let vm = PopoverViewModel()
        vm.apply(sessions: [
            session(id: "a", status: .waiting(detail: nil)),
            session(id: "b", status: .busy(tool: nil)),
            session(id: "c", status: .waiting(detail: "allow this?")),
            session(id: "d", status: .ended),
            session(id: "e", status: .shell),
            session(id: "f", status: .idle),
        ])
        XCTAssertEqual(vm.waiting.map(\.sessionId), ["a", "c"])
        // Busy, Shell, and Idle land in `other` - see its doc comment for
        // why this mirrors the Rust GUI's single "does this need me right
        // now" cut rather than splitting further. Ended sessions are
        // dropped entirely, matching iOS's `SessionStore` (PRO-214 review
        // finding 6): the server already filters them out, but the two
        // clients should agree even if one somehow slipped through.
        XCTAssertEqual(Set(vm.other.map(\.sessionId)), Set(["b", "e", "f"]))
    }

    func testOtherSortedByUpdatedAtDescending() {
        let vm = PopoverViewModel()
        let now = Date()
        vm.apply(sessions: [
            session(id: "old", status: .busy(tool: nil), updatedAt: now.addingTimeInterval(-300)),
            session(id: "new", status: .busy(tool: nil), updatedAt: now),
            session(id: "mid", status: .busy(tool: nil), updatedAt: now.addingTimeInterval(-60)),
        ])
        XCTAssertEqual(vm.other.map(\.sessionId), ["new", "mid", "old"])
    }

    func testSuccessiveApplyReplacesState() {
        let vm = PopoverViewModel()
        vm.apply(sessions: [
            session(id: "a", status: .waiting(detail: nil)),
        ])
        vm.apply(sessions: [
            session(id: "b", status: .busy(tool: nil)),
        ])
        XCTAssertEqual(vm.waiting.count, 0)
        XCTAssertEqual(vm.other.map(\.sessionId), ["b"])
    }

    func testApplySessionsClearsActivationErrors() {
        let vm = PopoverViewModel()
        vm.setActivationError(sessionId: "s1", message: "no tmux clients")
        XCTAssertEqual(vm.activationErrors["s1"], "no tmux clients")
        vm.apply(sessions: [
            session(id: "s1", status: .busy(tool: nil)),
        ])
        XCTAssertTrue(vm.activationErrors.isEmpty)
    }

    func testApplyConnectionUpdatesState() {
        let vm = PopoverViewModel()
        XCTAssertEqual(vm.connection, .connecting)
        vm.apply(connection: .connected)
        XCTAssertEqual(vm.connection, .connected)
        vm.apply(connection: .disconnected)
        XCTAssertEqual(vm.connection, .disconnected)
    }

    // MARK: - apply(hosts:)

    func testApplyHostsRecordsHostsAndSetsReceivedFlag() {
        let vm = PopoverViewModel()
        XCTAssertFalse(vm.hasReceivedHostStatus)
        XCTAssertTrue(vm.hosts.isEmpty)

        vm.apply(hosts: [
            HostStatus(hostname: "my-mac", agentKind: .claude, lastSeenAt: Date())
        ])
        XCTAssertTrue(vm.hasReceivedHostStatus)
        XCTAssertEqual(vm.hosts.map(\.hostname), ["my-mac"])
    }

    // MARK: - watcherAppearsSilent

    func testWatcherAppearsSilentFalseBeforeFirstHostStatusPoll() {
        let vm = PopoverViewModel()
        XCTAssertFalse(vm.watcherAppearsSilent(now: Date()))
    }

    func testWatcherAppearsSilentTrueWhenNoHostEverReported() {
        let vm = PopoverViewModel()
        vm.apply(hosts: [])
        XCTAssertTrue(vm.watcherAppearsSilent(now: Date()))
    }

    func testWatcherAppearsSilentFalseWithFreshlySeenHost() {
        let vm = PopoverViewModel()
        let now = Date()
        vm.apply(hosts: [HostStatus(hostname: "mbp", agentKind: .claude, lastSeenAt: now)])
        XCTAssertFalse(vm.watcherAppearsSilent(now: now))
    }

    func testWatcherAppearsSilentTrueOnceOnlyHostGoesStale() {
        let vm = PopoverViewModel()
        let now = Date()
        let stale = now.addingTimeInterval(-5 * 60)
        vm.apply(hosts: [HostStatus(hostname: "mbp", agentKind: .claude, lastSeenAt: stale)])
        XCTAssertTrue(vm.watcherAppearsSilent(now: now))
    }

    func testWatcherAppearsSilentFalseIfAnyHostStillFresh() {
        let vm = PopoverViewModel()
        let now = Date()
        let stale = now.addingTimeInterval(-5 * 60)
        vm.apply(hosts: [
            HostStatus(hostname: "dead-host", agentKind: .claude, lastSeenAt: stale),
            HostStatus(hostname: "live-host", agentKind: .claude, lastSeenAt: now),
        ])
        XCTAssertFalse(vm.watcherAppearsSilent(now: now))
    }

    // MARK: - SessionDisplay

    func testStatusTextFormat() {
        XCTAssertEqual(SessionDisplay.statusText(.busy(tool: nil)), "busy")
        XCTAssertEqual(SessionDisplay.statusText(.busy(tool: "Bash")), "busy(Bash)")
        XCTAssertEqual(SessionDisplay.statusText(.shell), "shell")
        XCTAssertEqual(SessionDisplay.statusText(.idle), "idle")
        XCTAssertEqual(SessionDisplay.statusText(.waiting(detail: nil)), "waiting")
        XCTAssertEqual(
            SessionDisplay.statusText(.waiting(detail: "rm -rf")),
            "waiting(rm -rf)"
        )
        XCTAssertEqual(SessionDisplay.statusText(.ended), "ended")
    }

    func testAgentMetadataUsesCompactMonogramAndOptionalModel() {
        let codex = session(
            id: "codex",
            status: .busy(tool: nil),
            agentKind: .codex,
            model: "gpt-5-codex"
        )
        let claude = session(
            id: "claude",
            status: .busy(tool: nil)
        )

        XCTAssertEqual(SessionDisplay.agentMonogram(for: codex), "X")
        XCTAssertEqual(SessionDisplay.agentLabel(for: codex), "Codex")
        XCTAssertEqual(SessionDisplay.agentModelText(for: codex), "gpt-5-codex")
        XCTAssertEqual(SessionDisplay.agentMonogram(for: claude), "C")
        XCTAssertEqual(SessionDisplay.agentLabel(for: claude), "Claude")
        XCTAssertNil(SessionDisplay.agentModelText(for: claude))
    }

    func testLocationTextWithHostAndBranchAndRemote() {
        let s = session(
            id: "x",
            status: .busy(tool: nil),
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
            status: .busy(tool: nil),
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
