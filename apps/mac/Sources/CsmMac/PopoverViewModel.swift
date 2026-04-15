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
    var working: [SessionView] = []
    var connection: ConnectionState = .connecting

    /// Delete-request handler set by `StatusItemController`. Runs on main.
    /// Takes the session id and the window the row was clicked in, so the
    /// confirmation sheet attaches to the popover (not the detached app
    /// menu).
    var onRequestDelete: ((_ sessionId: String, _ sourceWindow: NSWindow?) -> Void)?

    func apply(sessions: [SessionView]) {
        var waiting: [SessionView] = []
        var working: [SessionView] = []
        for s in sessions {
            switch s.status {
            case .waiting:
                waiting.append(s)
            case .working:
                working.append(s)
            case .ended:
                break
            }
        }
        working.sort { $0.updatedAt > $1.updatedAt }
        self.waiting = waiting
        self.working = working
    }

    func apply(connection: ConnectionState) {
        self.connection = connection
    }
}
