import SwiftUI

/// SwiftUI form backing the Preferences window. The server URL field is held
/// in local state so edits don't reconnect the core on every keystroke — the
/// user commits with Return or the Apply button.
struct PreferencesView: View {
    @Bindable var preferences: AppPreferences

    /// Invoked when the user applies a new server URL. The controller
    /// reconstructs the CoreHandle against this value.
    var onApplyServerUrl: (String) -> Void

    @State private var draftServerUrl: String = ""

    var body: some View {
        Form {
            Section("Connection") {
                HStack {
                    TextField(
                        "http://localhost:7685",
                        text: $draftServerUrl
                    )
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(applyServerUrl)
                    Button("Apply", action: applyServerUrl)
                        .disabled(draftServerUrl == preferences.serverUrl)
                }
                Text("Leave blank to use the env var / config file default.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Logging") {
                Picker("Log level", selection: $preferences.logLevel) {
                    ForEach(AppPreferences.logLevels, id: \.self) { level in
                        Text(level.capitalized).tag(level)
                    }
                }
                Text("Applied on next launch.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Startup") {
                Toggle("Launch at login", isOn: $preferences.launchAtLogin)
            }
        }
        .padding(20)
        .frame(width: 440)
        .onAppear { draftServerUrl = preferences.serverUrl }
    }

    private func applyServerUrl() {
        preferences.serverUrl = draftServerUrl
        onApplyServerUrl(draftServerUrl)
    }
}
