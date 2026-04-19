import SwiftUI

/// Settings screen. Same editable field as onboarding; saving persists the
/// new URL and asks `AppState` to rebuild the core.
struct SettingsView: View {
    @EnvironmentObject var appState: AppState
    @Environment(\.dismiss) private var dismiss

    @State private var url: String = ""

    var body: some View {
        Form {
            Section {
                TextField("http://csm.your-tailnet.ts.net:8080", text: $url)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .textContentType(.URL)
                    .submitLabel(.done)
                    .onSubmit(saveIfValid)
            } header: {
                Text("Server URL")
            } footer: {
                Text("Changing this reconnects to the new server. Existing session state is discarded.")
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Save", action: saveIfValid)
                    .disabled(!isValid || !isDifferent)
            }
        }
        .onAppear {
            if url.isEmpty {
                url = appState.serverURL ?? ""
            }
        }
    }

    private var isValid: Bool {
        let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        guard let parsed = URL(string: trimmed) else { return false }
        return parsed.scheme != nil && parsed.host != nil
    }

    private var isDifferent: Bool {
        url.trimmingCharacters(in: .whitespacesAndNewlines) != (appState.serverURL ?? "")
    }

    private func saveIfValid() {
        guard isValid else { return }
        appState.saveServerURL(url)
        dismiss()
    }
}
