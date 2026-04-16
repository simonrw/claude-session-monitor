import AppKit
import CsmCore
import SwiftUI

/// Sectioned session list shown in the status-item popover. Layout per parent
/// PRD PRO-122 §"Popover content": "Waiting for you" section + "Working"
/// section, per-row status colour, location, relative time, delete button.
struct PopoverView: View {
    @Bindable var viewModel: PopoverViewModel
    /// Invoked when the user clicks the gear icon. The controller opens the
    /// preferences window.
    var onOpenPreferences: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if viewModel.waiting.isEmpty && viewModel.working.isEmpty {
                        Text("No active sessions.")
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 24)
                            .frame(maxWidth: .infinity, alignment: .center)
                    } else {
                        if !viewModel.waiting.isEmpty {
                            Section {
                                ForEach(viewModel.waiting, id: \.sessionId) { session in
                                    SessionRow(session: session, viewModel: viewModel)
                                }
                            } header: {
                                SectionHeader(title: "Waiting for you", count: viewModel.waiting.count)
                            }
                        }
                        if !viewModel.working.isEmpty {
                            Section {
                                ForEach(viewModel.working, id: \.sessionId) { session in
                                    SessionRow(session: session, viewModel: viewModel)
                                }
                            } header: {
                                SectionHeader(title: "Working", count: viewModel.working.count)
                            }
                        }
                    }
                }
                .padding(12)
            }
        }
        .frame(width: 360, height: 420)
    }

    @ViewBuilder
    private var header: some View {
        HStack(spacing: 8) {
            Text("Claude Session Monitor")
                .font(.headline)
            Circle()
                .fill(connectionColor)
                .frame(width: 8, height: 8)
            Spacer()
            Button(action: onOpenPreferences) {
                Image(systemName: "gearshape")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .help("Preferences…")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var connectionColor: Color {
        switch viewModel.connection {
        case .connected: return .green
        case .connecting: return .yellow
        case .disconnected: return .red
        }
    }
}

private struct SectionHeader: View {
    let title: String
    let count: Int
    var body: some View {
        HStack {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text("(\(count))")
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
        }
        .padding(.vertical, 2)
    }
}

private struct SessionRow: View {
    let session: SessionView
    let viewModel: PopoverViewModel

    private var isClickable: Bool { session.tmuxTarget != nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text(SessionDisplay.locationText(for: session))
                    .font(.system(.body, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer()
                Button {
                    let window = NSApp.keyWindow ?? NSApp.windows.first
                    viewModel.onRequestDelete?(session.sessionId, window)
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.borderless)
                .help("Delete session")
            }
            HStack(alignment: .firstTextBaseline) {
                Text(SessionDisplay.statusText(session.status))
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(SessionDisplay.statusColor(session.status))
                Text(SessionDisplay.relativeTime(for: session))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // Inline activation error
            if let error = viewModel.activationErrors[session.sessionId] {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding(.vertical, 4)
        .opacity(isClickable ? 1.0 : 0.5)
        .contentShape(Rectangle())
        .onTapGesture {
            if isClickable {
                viewModel.onActivateSession?(session)
            }
        }
        .onHover { hovering in
            if isClickable && hovering {
                NSCursor.pointingHand.push()
            } else {
                NSCursor.pop()
            }
        }
    }
}
