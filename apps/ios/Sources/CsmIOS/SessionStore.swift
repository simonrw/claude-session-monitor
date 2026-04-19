import Combine
import CsmCore
import Foundation

/// ObservableObject-backed session store. Owns the `CoreHandle` and its
/// subscription; bridges UniFFI's `SessionObserver` callbacks (on an SSE
/// worker thread) onto `@MainActor` before mutating `@Published` state.
@MainActor
final class SessionStore: ObservableObject {
    @Published private(set) var sessions: [SessionView] = []
    @Published private(set) var connection: ConnectionState = .connecting
    @Published private(set) var summary: MenuBarSummary = MenuBarSummary(
        waitingInput: 0, waitingPermission: 0, working: 0
    )
    /// Timestamp of the most recent successful message from the core.
    /// Reset when a fresh `.connected` transition arrives.
    @Published private(set) var lastSuccessfulMessage: Date?

    let serverURL: String

    private let core: CoreHandle
    private var subscription: SubscriptionHandle?

    init(serverURL: String) {
        self.serverURL = serverURL
        self.core = CoreHandle(serverUrl: serverURL)
        let observer = Observer(store: self)
        self.subscription = core.subscribe(observer: observer)
    }

    /// Sessions filtered into the two display buckets. Ended sessions are
    /// already elided by the Rust view-model but we filter defensively.
    var waiting: [SessionView] {
        sessions.filter { if case .waiting = $0.status { return true } else { return false } }
    }

    var working: [SessionView] {
        sessions.filter { if case .working = $0.status { return true } else { return false } }
    }

    func delete(sessionID: String) {
        core.deleteSession(sessionId: sessionID)
    }

    // MARK: - Main-thread apply helpers (called from the private Observer)

    fileprivate func apply(sessions: [SessionView]) {
        self.sessions = sessions.filter {
            if case .ended = $0.status { return false } else { return true }
        }
        self.lastSuccessfulMessage = Date()
    }

    fileprivate func apply(connection: ConnectionState) {
        let wasConnected = self.connection == .connected
        self.connection = connection
        if connection == .connected && !wasConnected {
            self.lastSuccessfulMessage = Date()
        }
    }

    fileprivate func apply(summary: MenuBarSummary) {
        self.summary = summary
    }
}

/// UniFFI callback observer. Callbacks land on a background (SSE) thread;
/// each hops to the main actor before touching `SessionStore`.
private final class Observer: SessionObserver, @unchecked Sendable {
    weak var store: SessionStore?

    init(store: SessionStore) {
        self.store = store
    }

    func onSessionsChanged(sessions: [SessionView]) {
        Task { @MainActor [weak store] in
            store?.apply(sessions: sessions)
        }
    }

    func onConnectionChanged(state: ConnectionState) {
        Task { @MainActor [weak store] in
            store?.apply(connection: state)
        }
    }

    func onSummaryChanged(summary: MenuBarSummary) {
        Task { @MainActor [weak store] in
            store?.apply(summary: summary)
        }
    }
}
