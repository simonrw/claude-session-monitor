import CsmCore
import SwiftUI

/// Primary screen — sectioned list of Waiting + Working sessions.
/// Leading navbar item: connection pill. Trailing: gear → settings.
/// Rows are tap-to-detail and support swipe-to-delete (two-tap: reveal
/// button, then tap the red button).
struct SessionListView: View {
    @ObservedObject var store: SessionStore
    @EnvironmentObject var appState: AppState

    @State private var detailSession: SessionView?
    @State private var showingSettings = false
    /// Ticks once a second so the "time ago" strings in rows update while
    /// the list is visible.
    @State private var now: Date = Date()
    private let timer = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        NavigationStack {
            content
                .navigationTitle("Sessions")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        ConnectionPill(store: store) {
                            appState.rebuildStore()
                        }
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button {
                            showingSettings = true
                        } label: {
                            Image(systemName: "gearshape")
                        }
                        .accessibilityLabel("Settings")
                    }
                }
                .sheet(item: $detailSession) { session in
                    SessionDetailView(session: session)
                }
                .sheet(isPresented: $showingSettings) {
                    NavigationStack {
                        SettingsView()
                            .toolbar {
                                ToolbarItem(placement: .cancellationAction) {
                                    Button("Cancel") { showingSettings = false }
                                }
                            }
                    }
                }
        }
        .onReceive(timer) { now = $0 }
    }

    @ViewBuilder
    private var content: some View {
        if store.connection == .disconnected && store.sessions.isEmpty {
            emptyDisconnectedState
        } else if store.sessions.isEmpty {
            ContentUnavailableView(
                "No active sessions",
                systemImage: "moon.zzz",
                description: Text("Claude isn't running anywhere right now.")
            )
        } else {
            list
        }
    }

    private var list: some View {
        List {
            if !store.waiting.isEmpty {
                Section("Waiting") {
                    ForEach(store.waiting, id: \.sessionId) { session in
                        row(for: session, waiting: true)
                    }
                }
            }
            if !store.working.isEmpty {
                Section("Working") {
                    ForEach(store.working, id: \.sessionId) { session in
                        row(for: session, waiting: false)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .opacity(store.connection == .disconnected ? 0.6 : 1.0)
    }

    private func row(for session: SessionView, waiting: Bool) -> some View {
        SessionRowView(session: session, now: now)
            .contentShape(Rectangle())
            .onTapGesture { detailSession = session }
            .listRowBackground(rowBackground(for: session))
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                Button(role: .destructive) {
                    store.delete(sessionID: session.sessionId)
                } label: {
                    Label("Delete", systemImage: "trash")
                }
            }
    }

    /// Subtle colour-code by status. Waiting(input) = red, waiting(permission)
    /// = yellow, working = green. Uses a very low opacity so the text stays
    /// legible in light + dark.
    private func rowBackground(for session: SessionView) -> Color {
        switch session.status {
        case .waiting(let reason, _):
            switch reason {
            case .input: return Color.red.opacity(0.12)
            case .permission: return Color.yellow.opacity(0.15)
            }
        case .working: return Color.green.opacity(0.10)
        case .ended: return Color.clear
        }
    }

    @ViewBuilder
    private var emptyDisconnectedState: some View {
        ContentUnavailableView {
            Label("Can't reach server", systemImage: "wifi.exclamationmark")
        } description: {
            Text("Can't reach \(store.serverURL). Tailscale running?")
        } actions: {
            Button("Retry") { appState.rebuildStore() }
                .buttonStyle(.borderedProminent)
        }
    }
}

/// Allow `SessionView` to drive `.sheet(item:)`.
extension SessionView: Identifiable {
    public var id: String { sessionId }
}
