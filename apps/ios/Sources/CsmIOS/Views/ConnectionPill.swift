import CsmCore
import SwiftUI

/// Capsule showing the SSE connection status. Tapping presents a sheet with
/// the server URL, time since the last successful message, and a retry
/// button that asks the app to rebuild the `CoreHandle`.
struct ConnectionPill: View {
    @ObservedObject var store: SessionStore
    var onRetry: () -> Void

    @State private var showingDetails = false

    var body: some View {
        Button {
            showingDetails = true
        } label: {
            indicator
                .frame(width: 20, height: 20)
        }
        .accessibilityLabel(accessibilityLabel)
        .sheet(isPresented: $showingDetails) {
            ConnectionDetailSheet(
                store: store,
                onRetry: {
                    showingDetails = false
                    onRetry()
                }
            )
            .presentationDetents([.medium])
        }
    }

    @ViewBuilder
    private var indicator: some View {
        switch store.connection {
        case .connecting:
            ProgressView()
                .progressViewStyle(.circular)
                .tint(.yellow)
                .scaleEffect(0.7)
        case .connected:
            Circle().fill(Color.green)
        case .disconnected:
            Circle().fill(Color.red)
        }
    }

    private var accessibilityLabel: String {
        switch store.connection {
        case .connecting: return "Connecting"
        case .connected: return "Connected"
        case .disconnected: return "Disconnected"
        }
    }
}

/// Sheet body shown when the pill is tapped.
private struct ConnectionDetailSheet: View {
    @ObservedObject var store: SessionStore
    var onRetry: () -> Void

    /// Ticks once a second so the "time since last message" text refreshes
    /// while the sheet is open.
    @State private var now: Date = Date()
    private let timer = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        NavigationStack {
            Form {
                Section("Server") {
                    Text(store.serverURL)
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                }
                Section("Status") {
                    LabeledContent("Connection", value: statusText)
                    LabeledContent("Last message", value: lastMessageText)
                }
                Section {
                    Button("Retry connection", action: onRetry)
                }
            }
            .navigationTitle("Connection")
            .navigationBarTitleDisplayMode(.inline)
        }
        .onReceive(timer) { now = $0 }
    }

    private var statusText: String {
        switch store.connection {
        case .connecting: return "Connecting"
        case .connected: return "Connected"
        case .disconnected: return "Disconnected"
        }
    }

    private var lastMessageText: String {
        guard let last = store.lastSuccessfulMessage else { return "never" }
        let diff = Int(now.timeIntervalSince(last))
        if diff < 60 { return "\(max(0, diff))s ago" }
        if diff < 3600 { return "\(diff / 60)m ago" }
        return "\(diff / 3600)h ago"
    }
}
