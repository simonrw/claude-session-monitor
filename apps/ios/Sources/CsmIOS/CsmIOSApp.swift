import CsmCore
import SwiftUI

/// iOS app entry point. Owns the telemetry + sentry guards, the persisted
/// server URL, and the single `CoreHandle`. The handle is rebuilt when the
/// user edits the URL in `SettingsView`.
@main
struct CsmIOSApp: App {
    @StateObject private var appState = AppState()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(appState)
        }
    }
}

/// Top-level container: owns the `CoreHandle`, the `SessionStore`, and the
/// URL-presence state that gates the onboarding sheet. Lives as a
/// `StateObject` on the `App` so its guards survive scene changes.
///
/// Pinned to the main actor because `SessionStore` is `@MainActor` — the
/// app-level state object is the right boundary to enforce that.
@MainActor
final class AppState: ObservableObject {
    @Published var serverURL: String?
    @Published private(set) var store: SessionStore?

    private var telemetry: TelemetryGuard?
    private var sentry: SentryGuard?

    init() {
        self.telemetry = Self.startTelemetry()
        self.sentry = initSentry(appLabel: "ios")
        let url = UserDefaults.standard.string(forKey: Self.serverURLKey)
        self.serverURL = url
        if let url, !url.isEmpty {
            self.store = SessionStore(serverURL: url)
        }
    }

    static let serverURLKey = "serverURL"

    func saveServerURL(_ url: String) {
        let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        UserDefaults.standard.set(trimmed, forKey: Self.serverURLKey)
        self.serverURL = trimmed
        rebuildStore()
    }

    /// Drop the current store + core, then construct a fresh one. Called on
    /// settings change and by the Retry button on the connection pill.
    func rebuildStore() {
        self.store = nil
        guard let url = serverURL, !url.isEmpty else { return }
        self.store = SessionStore(serverURL: url)
    }

    /// Log directory lives under Caches so iOS can reclaim it when space is
    /// tight. `init_telemetry` creates the directory on the Rust side.
    private static func startTelemetry() -> TelemetryGuard {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        let logDir = caches.appendingPathComponent("claude-session-monitor").path
        return initTelemetry(appLabel: "ios", logLevel: "info", logDir: logDir)
    }
}

/// Routes between onboarding (no URL set) and the session list.
private struct RootView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        Group {
            if let store = appState.store {
                SessionListView(store: store)
            } else {
                // Placeholder shown behind the onboarding sheet.
                Color(.systemBackground)
            }
        }
        .fullScreenCover(isPresented: Binding(
            get: { appState.serverURL == nil || (appState.serverURL?.isEmpty ?? true) },
            set: { _ in }
        )) {
            OnboardingView { url in
                appState.saveServerURL(url)
            }
        }
    }
}
