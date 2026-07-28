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
        busy: 0, waiting: 0
    )
    /// Timestamp of the most recent successful message from the core.
    /// Reset when a fresh `.connected` transition arrives.
    @Published private(set) var lastSuccessfulMessage: Date?
    /// Latest hosts the server has ever seen report, from
    /// `onHostStatusChanged`. Lets a view distinguish "no host has ever
    /// reported" from "genuinely zero sessions right now" (PRO-211/PRO-214) -
    /// see `hasReceivedHostStatus`.
    @Published private(set) var hosts: [HostStatus] = []
    /// Whether at least one `onHostStatusChanged` callback has landed - an
    /// empty `hosts` is ambiguous with "no update yet" on its own.
    @Published private(set) var hasReceivedHostStatus = false

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

    /// Everything not `waiting`: `busy`, `shell`, and `idle` sessions
    /// (`ended` is already filtered out in `apply(sessions:)`). Mirrors the
    /// same single "does this need me right now" cut as the mac popover's
    /// `PopoverViewModel.other` - see its doc comment.
    var other: [SessionView] {
        sessions.filter { if case .waiting = $0.status { return false } else { return true } }
    }

    func delete(sessionID: String) {
        core.deleteSession(sessionId: sessionID)
    }

    /// Whether the empty-sessions state should read "the watcher isn't
    /// reporting" rather than "genuinely no sessions right now" - mirrors
    /// `watcher_appears_silent` in `crates/gui/src/main.rs`,
    /// `watcherAppearsSilent` in `web/src/lib/sessions.ts`, and mac's
    /// `PopoverViewModel.watcherAppearsSilent` (PRO-211/PRO-214, PRO-214
    /// review finding 3). Only meaningful once `hasReceivedHostStatus` is
    /// true - before the first `onHostStatusChanged` callback lands, an
    /// empty `hosts` is ambiguous with "haven't heard back yet".
    ///
    /// True when either no host has ever reported (`hosts` empty) or every
    /// host that has reported has gone stale as of `now` - see
    /// `hostStatusIsStale` (wraps `common::api::host_is_stale`), which
    /// catches a watcher that reported once and then died: `hosts` stays
    /// non-empty forever in that case, with a `lastSeenAt` that stops
    /// advancing, which a plain `hosts.isEmpty` check would miss entirely.
    func watcherAppearsSilent(now: Date = Date()) -> Bool {
        guard hasReceivedHostStatus else { return false }
        return hosts.isEmpty || hosts.allSatisfy { hostStatusIsStale(lastSeenAt: $0.lastSeenAt, now: now) }
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

    fileprivate func apply(hosts: [HostStatus]) {
        self.hosts = hosts
        self.hasReceivedHostStatus = true
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

    func onHostStatusChanged(hosts: [HostStatus]) {
        Task { @MainActor [weak store] in
            store?.apply(hosts: hosts)
        }
    }
}
