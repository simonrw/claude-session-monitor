import AppKit
import CsmCore
import Observation

/// View model backing [`PopoverView`]. `@Observable` so SwiftUI diff-renders
/// the list on each mutation.
///
/// Mutators (`apply(sessions:)`, `apply(connection:)`) MUST be called on the
/// main thread — the `@Observable` macro does not provide synchronization,
/// and SwiftUI reads these properties on main. The Rust observer hops via
/// `DispatchQueue.main.async` before calling in.
@Observable
final class PopoverViewModel {
    var waiting: [SessionView] = []
    /// Everything not `waiting`: `busy`, `shell`, `idle`, and `ended`
    /// sessions, sorted most-recently-updated first. Deliberately not split
    /// further - this mirrors the Rust GUI's `partition_sessions` (see its
    /// doc comment in `crates/gui/src/main.rs`), which makes the same single
    /// "does this need me right now" cut and leaves the finer-grained
    /// Busy/Shell/Idle/Ended distinction to `SessionDisplay.statusText`/
    /// `statusColor` within this bucket.
    var other: [SessionView] = []
    var connection: ConnectionState = .connecting
    var activationErrors: [String: String] = [:]
    /// Latest hosts the server has ever seen report, from
    /// `onHostStatusChanged`. Empty means either "no host has ever
    /// reported" or "no update has landed yet" - see `PopoverView`'s empty
    /// state, which uses this to distinguish that from "watcher is fine,
    /// there are just genuinely zero sessions right now".
    var hosts: [HostStatus] = []
    /// Whether at least one `onHostStatusChanged` callback has landed. Needed
    /// because an empty `hosts` array is ambiguous with "no update yet" on
    /// its own - see `hosts`' doc comment.
    var hasReceivedHostStatus = false

    /// Delete-request handler set by `StatusItemController`. Runs on main.
    /// Takes the session id and the window the row was clicked in, so the
    /// confirmation sheet attaches to the popover (not the detached app
    /// menu).
    var onRequestDelete: ((_ sessionId: String, _ sourceWindow: NSWindow?) -> Void)?

    /// Activation handler set by `StatusItemController`. Runs on main.
    var onActivateSession: ((_ session: SessionView) -> Void)?

    func apply(sessions: [SessionView]) {
        var waiting: [SessionView] = []
        var other: [SessionView] = []
        for s in sessions {
            switch s.status {
            case .waiting:
                waiting.append(s)
            case .busy, .shell, .idle:
                other.append(s)
            case .ended:
                // The server already filters ended sessions out, so this
                // never fires in practice - but iOS's `SessionStore` filters
                // `.ended` explicitly too (defence in depth), and the two
                // clients should keep agreeing rather than one silently
                // starting to render sessions the other drops.
                break
            }
        }
        other.sort { $0.updatedAt > $1.updatedAt }
        self.waiting = waiting
        self.other = other
        self.activationErrors = [:]
    }

    func apply(connection: ConnectionState) {
        self.connection = connection
    }

    func apply(hosts: [HostStatus]) {
        self.hosts = hosts
        self.hasReceivedHostStatus = true
    }

    /// Whether the empty-sessions state should read "the watcher isn't
    /// reporting" rather than "genuinely no sessions right now" - mirrors
    /// `watcher_appears_silent` in `crates/gui/src/main.rs` and
    /// `watcherAppearsSilent` in `web/src/lib/sessions.ts` (PRO-211/PRO-214,
    /// PRO-214 review finding 3). Only meaningful once
    /// `hasReceivedHostStatus` is true - before the first
    /// `onHostStatusChanged` callback lands, an empty `hosts` is ambiguous
    /// with "haven't heard back yet".
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

    func setActivationError(sessionId: String, message: String) {
        activationErrors[sessionId] = message
    }
}
