import Foundation
import Observation
import ServiceManagement

/// User-facing preferences. Backed by `UserDefaults` for string/bool values;
/// the launch-at-login toggle round-trips through `SMAppService`, not
/// `UserDefaults` — macOS owns the source of truth there.
///
/// Stored properties are observable (SwiftUI forms rebind on edit) with a
/// `didSet` that persists each change. `didSet` does not fire during `init`,
/// so reading from UserDefaults on startup won't re-write the same value.
@Observable
final class AppPreferences {
    enum Key {
        static let serverUrl = "com.claude-session-monitor.csm-mac.serverUrl"
        static let logLevel = "com.claude-session-monitor.csm-mac.logLevel"
    }

    /// Valid levels for the tracing subscriber. Anything outside this list
    /// falls back to "info" at startup.
    static let logLevels: [String] = ["trace", "debug", "info", "warn", "error"]

    @ObservationIgnored private let defaults: UserDefaults

    var serverUrl: String {
        didSet { defaults.set(serverUrl, forKey: Key.serverUrl) }
    }

    var logLevel: String {
        didSet { defaults.set(logLevel, forKey: Key.logLevel) }
    }

    /// Current SMAppService status mirrored as a bool. Setting writes through
    /// to register/unregister; failures are logged, not surfaced, so a bad
    /// state (e.g. unsigned binary) doesn't crash the app.
    var launchAtLogin: Bool {
        didSet { applyLaunchAtLogin(launchAtLogin) }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let rawLevel = defaults.string(forKey: Key.logLevel) ?? "info"
        self.serverUrl = defaults.string(forKey: Key.serverUrl) ?? ""
        self.logLevel = Self.logLevels.contains(rawLevel) ? rawLevel : "info"
        self.launchAtLogin = SMAppService.mainApp.status == .enabled
    }

    /// Resolves the configured server URL for passing to `CoreHandle`.
    /// Returns `nil` when the preference is empty, so the core falls through
    /// to the env/config/default resolution ladder.
    var configuredServerUrl: String? {
        let trimmed = serverUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private func applyLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            NSLog("launch-at-login toggle failed: \(error.localizedDescription)")
        }
    }
}
