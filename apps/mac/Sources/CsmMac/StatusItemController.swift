import AppKit
import CsmCore

/// Owns the NSStatusItem and keeps its icon in sync with the
/// [`MenuBarSummary`] pushed by the Rust core.
///
/// Callbacks from the core may land on the SSE worker thread; UI mutation
/// must happen on the main thread, so each observer method hops via
/// `DispatchQueue.main.async`.
final class StatusItemController {
    private let statusItem: NSStatusItem
    private var core: CoreHandle?
    private var subscription: SubscriptionHandle?

    init() {
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        self.statusItem.button?.image = IconRenderer.render(
            summary: MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 0)
        )
        self.statusItem.button?.action = #selector(statusItemClicked(_:))
        self.statusItem.button?.target = self
    }

    /// Start the Rust core and subscribe. v1 keeps this simple — no popover.
    /// The popover lands in PRO-127.
    func start(serverUrl: String? = nil) {
        let core = CoreHandle(serverUrl: serverUrl)
        self.core = core
        let observer = Observer(controller: self)
        self.subscription = core.subscribe(observer: observer)
    }

    fileprivate func apply(summary: MenuBarSummary) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.statusItem.button?.image = IconRenderer.render(summary: summary)
        }
    }

    @objc private func statusItemClicked(_ sender: Any?) {
        // v1: placeholder. The sectioned popover UI lands in PRO-127.
    }
}

/// Private observer that forwards Rust-driven summary updates back into the
/// controller on the main thread.
private final class Observer: SessionObserver, @unchecked Sendable {
    weak var controller: StatusItemController?

    init(controller: StatusItemController) {
        self.controller = controller
    }

    func onSessionsChanged(sessions: [SessionView]) {}
    func onConnectionChanged(state: ConnectionState) {}
    func onSummaryChanged(summary: MenuBarSummary) {
        controller?.apply(summary: summary)
    }
}
