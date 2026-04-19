import CsmCore
import SwiftUI

/// Detail sheet shown when a row is tapped. Surfaces the full cwd + git URL
/// (no truncation), the absolute `updated_at` timestamp, and — for Working
/// sessions — the tool name.
struct SessionDetailView: View {
    let session: SessionView

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section("Location") {
                    LabeledContent("cwd") {
                        Text(session.cwd)
                            .font(.system(.body, design: .monospaced))
                            .multilineTextAlignment(.trailing)
                            .textSelection(.enabled)
                    }
                    if let host = session.hostname {
                        LabeledContent("Hostname", value: host)
                    }
                }
                if session.gitBranch != nil || session.gitRemote != nil {
                    Section("Git") {
                        if let branch = session.gitBranch {
                            LabeledContent("Branch", value: branch)
                        }
                        if let remote = session.gitRemote {
                            LabeledContent("Remote") {
                                Text(remote)
                                    .font(.system(.callout, design: .monospaced))
                                    .multilineTextAlignment(.trailing)
                                    .textSelection(.enabled)
                            }
                        }
                    }
                }
                Section("Status") {
                    LabeledContent("State", value: statusText)
                    if case .working(let tool) = session.status, let tool, !tool.isEmpty {
                        LabeledContent("Tool", value: tool)
                    }
                    LabeledContent("Updated", value: absoluteTimestamp)
                }
                Section("Session") {
                    LabeledContent("ID") {
                        Text(session.sessionId)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }
            }
            .navigationTitle("Session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private var statusText: String {
        switch session.status {
        case .working: return "Working"
        case .waiting(let reason, let detail):
            let r = reason == .input ? "input" : "permission"
            if let detail, !detail.isEmpty { return "Waiting (\(r): \(detail))" }
            return "Waiting (\(r))"
        case .ended: return "Ended"
        }
    }

    private var absoluteTimestamp: String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .medium
        return formatter.string(from: session.updatedAt)
    }
}
