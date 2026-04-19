import SwiftUI

/// First-run screen asking the user for their server URL. Presented as a
/// non-dismissable full-screen sheet so the main app can't render without a
/// URL configured.
struct OnboardingView: View {
    var onSave: (String) -> Void

    @State private var url: String = ""

    var body: some View {
        NavigationStack {
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
                    Text("The session-monitor server, reachable over your tailnet. Plain http is fine inside the tailnet.")
                }
            }
            .navigationTitle("Welcome")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", action: saveIfValid)
                        .disabled(!isValid)
                }
            }
        }
        .interactiveDismissDisabled(true)
    }

    private var isValid: Bool {
        let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        guard let parsed = URL(string: trimmed) else { return false }
        return parsed.scheme != nil && parsed.host != nil
    }

    private func saveIfValid() {
        guard isValid else { return }
        onSave(url.trimmingCharacters(in: .whitespacesAndNewlines))
    }
}
