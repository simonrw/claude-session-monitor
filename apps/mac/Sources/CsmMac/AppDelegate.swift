import AppKit
import CsmCore

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let preferences = AppPreferences()
    private var statusItem: StatusItemController?
    private var telemetry: TelemetryGuard?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Log level from preferences (set in a prior session), overridden by
        // the env var CSM_LOG_LEVEL if present — mostly for local debugging.
        let logLevel = ProcessInfo.processInfo.environment["CSM_LOG_LEVEL"]
            ?? preferences.logLevel
        telemetry = initTelemetry(appLabel: "mac", logLevel: logLevel)

        let controller = StatusItemController(preferences: preferences)
        let envServer = ProcessInfo.processInfo.environment["CSM_SERVER_URL"]
        controller.start(serverUrl: envServer)
        statusItem = controller
    }

    /// Menu-bar apps should not terminate when a window closes — they have
    /// no windows.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}
