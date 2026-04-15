import AppKit
import CsmCore

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: StatusItemController?
    private var telemetry: TelemetryGuard?

    func applicationDidFinishLaunching(_ notification: Notification) {
        telemetry = initTelemetry(appLabel: "mac", logLevel: "info")
        let controller = StatusItemController()
        controller.start(serverUrl: ProcessInfo.processInfo.environment["CSM_SERVER_URL"])
        statusItem = controller
    }

    /// Menu-bar apps should not terminate when a window closes — they have
    /// no windows.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}
