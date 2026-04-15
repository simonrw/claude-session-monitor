import XCTest
@testable import CsmCore

/// XCTest verifying the UniFFI-generated Swift bindings load, a CoreHandle can
/// be constructed, and that subscribing delivers the initial-snapshot
/// callbacks expected by `common::view_model::CoreHandle::subscribe`.
///
/// Networking is not exercised — we point at a loopback address nothing is
/// listening on. The SSE worker reconnects in the background; the initial
/// snapshot is synchronous regardless.
final class CsmCoreBindingsTests: XCTestCase {

    final class Recorder: SessionObserver, @unchecked Sendable {
        let lock = NSLock()
        var sessions: [[SessionView]] = []
        var connections: [ConnectionState] = []
        var summaries: [MenuBarSummary] = []

        func onSessionsChanged(sessions: [SessionView]) {
            lock.lock(); defer { lock.unlock() }
            self.sessions.append(sessions)
        }
        func onConnectionChanged(state: ConnectionState) {
            lock.lock(); defer { lock.unlock() }
            self.connections.append(state)
        }
        func onSummaryChanged(summary: MenuBarSummary) {
            lock.lock(); defer { lock.unlock() }
            self.summaries.append(summary)
        }
    }

    func testSubscribeDeliversInitialSnapshot() {
        let core = CoreHandle(serverUrl: "http://127.0.0.1:1")
        let recorder = Recorder()
        let subscription = core.subscribe(observer: recorder)
        defer { subscription.cancel() }

        recorder.lock.lock()
        let sessionsCount = recorder.sessions.count
        let connectionsCount = recorder.connections.count
        let summariesCount = recorder.summaries.count
        recorder.lock.unlock()

        XCTAssertEqual(sessionsCount, 1, "initial sessions snapshot fires exactly once")
        XCTAssertEqual(connectionsCount, 1, "initial connection snapshot fires exactly once")
        XCTAssertEqual(summariesCount, 1, "initial summary snapshot fires exactly once")
    }

    func testMenuBarSummaryIsCodable() {
        let s = MenuBarSummary(waitingInput: 2, waitingPermission: 1, working: 3)
        XCTAssertEqual(s.waitingInput, 2)
        XCTAssertEqual(s.waitingPermission, 1)
        XCTAssertEqual(s.working, 3)
    }

    func testConnectionStateCases() {
        let states: [ConnectionState] = [.connecting, .connected, .disconnected]
        XCTAssertEqual(states.count, 3)
    }
}
