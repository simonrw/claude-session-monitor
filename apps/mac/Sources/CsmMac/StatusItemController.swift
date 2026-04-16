import AppKit
import CsmCore
import SwiftUI

/// Owns the NSStatusItem, the popover hosting the SwiftUI session list, the
/// preferences window, and the Rust core subscription. Callbacks from the
/// core land on the SSE worker thread; every UI mutation hops to main via
/// `DispatchQueue.main.async`.
final class StatusItemController: NSObject {
    private let statusItem: NSStatusItem
    private let viewModel: PopoverViewModel
    private let popover: NSPopover
    private let preferences: AppPreferences
    private var preferencesWindow: NSWindow?
    private var core: CoreHandle?
    private var subscription: SubscriptionHandle?

    init(preferences: AppPreferences = AppPreferences()) {
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        self.viewModel = PopoverViewModel()
        self.popover = NSPopover()
        self.preferences = preferences
        super.init()

        self.statusItem.button?.image = IconRenderer.render(
            summary: MenuBarSummary(waitingInput: 0, waitingPermission: 0, working: 0)
        )
        self.statusItem.button?.action = #selector(statusItemClicked(_:))
        self.statusItem.button?.target = self
        self.statusItem.button?.sendAction(on: [.leftMouseUp, .rightMouseUp])

        self.popover.animates = false
        self.popover.behavior = .transient
        self.popover.contentSize = NSSize(width: 360, height: 420)
        self.popover.contentViewController = NSHostingController(
            rootView: PopoverView(
                viewModel: viewModel,
                onOpenPreferences: { [weak self] in self?.showPreferences() }
            )
        )

        self.viewModel.onRequestDelete = { [weak self] sessionId, window in
            self?.confirmDelete(sessionId: sessionId, attachedTo: window)
        }
    }

    /// Start the Rust core and subscribe. If `serverUrl` is `nil` the
    /// preference value is used; pass an explicit value to override (e.g.
    /// when env var `CSM_SERVER_URL` is set).
    func start(serverUrl: String? = nil) {
        let url = serverUrl ?? preferences.configuredServerUrl
        let core = CoreHandle(serverUrl: url)
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
        let event = NSApp.currentEvent
        if event?.type == .rightMouseUp {
            showContextMenu(from: button)
            return
        }
        if popover.isShown {
            popover.performClose(sender)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.makeKey()
        }
    }

    private func showContextMenu(from button: NSStatusBarButton) {
        let menu = NSMenu()
        menu.addItem(
            withTitle: "Preferences…",
            action: #selector(menuOpenPreferences),
            keyEquivalent: ","
        ).target = self
        menu.addItem(.separator())
        menu.addItem(
            withTitle: "Quit",
            action: #selector(menuQuit),
            keyEquivalent: "q"
        ).target = self

        statusItem.menu = menu
        button.performClick(nil)
        statusItem.menu = nil
    }

    @objc private func menuOpenPreferences() {
        showPreferences()
    }

    @objc private func menuQuit() {
        NSApp.terminate(nil)
    }

    // MARK: - Preferences

    private func showPreferences() {
        // Closing the popover before presenting the window gives focus to
        // the preferences window instead of dismissing it as an outside
        // click.
        popover.performClose(nil)

        if let window = preferencesWindow {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let view = PreferencesView(preferences: preferences) { [weak self] url in
            self?.reconfigureCore(serverUrl: url)
        }
        let hosting = NSHostingController(rootView: view)
        let window = NSWindow(contentViewController: hosting)
        window.title = "Preferences"
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        preferencesWindow = window
    }

    /// Replace the current CoreHandle with one pointed at `serverUrl`.
    /// Dropping the old subscription detaches its observer; the new subscribe
    /// call replays an initial snapshot.
    private func reconfigureCore(serverUrl: String) {
        subscription = nil
        core = nil
        viewModel.apply(sessions: [])
        viewModel.apply(connection: .connecting)
        let trimmed = serverUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        start(serverUrl: trimmed.isEmpty ? nil : trimmed)
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
