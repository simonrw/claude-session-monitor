import AppKit
import CsmCore
import SwiftUI

/// Owns the NSStatusItem, the popover hosting the SwiftUI session list, and
/// the Rust core subscription. Callbacks from the core land on the SSE
/// worker thread; every UI mutation hops to main via
/// `DispatchQueue.main.async`.
final class StatusItemController: NSObject {
    private let statusItem: NSStatusItem
    private let viewModel: PopoverViewModel
    private let popover: NSPopover
    private var core: CoreHandle?
    private var subscription: SubscriptionHandle?

    override init() {
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        self.viewModel = PopoverViewModel()
        self.popover = NSPopover()
        super.init()

        self.statusItem.button?.image = IconRenderer.render(
            summary: MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 0)
        )
        self.statusItem.button?.action = #selector(statusItemClicked(_:))
        self.statusItem.button?.target = self

        self.popover.behavior = .transient
        self.popover.contentViewController = NSHostingController(
            rootView: PopoverView(viewModel: viewModel)
        )

        self.viewModel.onRequestDelete = { [weak self] sessionId, window in
            self?.confirmDelete(sessionId: sessionId, attachedTo: window)
        }
    }

    /// Start the Rust core and subscribe.
    func start(serverUrl: String? = nil) {
        let core = CoreHandle(serverUrl: serverUrl)
        self.core = core
        let observer = Observer(controller: self)
        self.subscription = core.subscribe(observer: observer)
    }

    // MARK: - Observer routing (main thread)

    fileprivate func apply(summary: MenuBarSummary) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.statusItem.button?.image = IconRenderer.render(summary: summary)
        }
    }

    fileprivate func apply(sessions: [SessionView]) {
        DispatchQueue.main.async { [weak self] in
            self?.viewModel.apply(sessions: sessions)
        }
    }

    fileprivate func apply(connection: ConnectionState) {
        DispatchQueue.main.async { [weak self] in
            self?.viewModel.apply(connection: connection)
        }
    }

    // MARK: - Popover toggle

    @objc private func statusItemClicked(_ sender: Any?) {
        guard let button = statusItem.button else { return }
        if popover.isShown {
            popover.performClose(sender)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.makeKey()
        }
    }

    // MARK: - Delete confirmation

    private func confirmDelete(sessionId: String, attachedTo window: NSWindow?) {
        let alert = NSAlert()
        alert.messageText = "Delete session?"
        alert.informativeText = "Session \(sessionId) will be removed."
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        alert.alertStyle = .warning

        let host = window ?? popover.contentViewController?.view.window
        if let host {
            alert.beginSheetModal(for: host) { [weak self] response in
                if response == .alertFirstButtonReturn {
                    self?.core?.deleteSession(sessionId: sessionId)
                }
            }
        } else {
            // Fallback for cases where the popover has no attached window yet.
            if alert.runModal() == .alertFirstButtonReturn {
                self.core?.deleteSession(sessionId: sessionId)
            }
        }
    }
}

/// Private observer that routes Rust-driven updates back into the
/// controller. Each callback dispatches to the main thread.
private final class Observer: SessionObserver, @unchecked Sendable {
    weak var controller: StatusItemController?

    init(controller: StatusItemController) {
        self.controller = controller
    }

    func onSessionsChanged(sessions: [SessionView]) {
        controller?.apply(sessions: sessions)
    }
    func onConnectionChanged(state: ConnectionState) {
        controller?.apply(connection: state)
    }
    func onSummaryChanged(summary: MenuBarSummary) {
        controller?.apply(summary: summary)
    }
}
