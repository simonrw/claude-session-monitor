import CsmCore
import SwiftUI

/// Single row in the session list. Displays a dense summary: truncated cwd,
/// hostname, git branch, truncated remote, and time-since-updated.
struct SessionRowView: View {
    let session: SessionView
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(agentMonogram)
                    .font(.system(size: 10, weight: .semibold, design: .rounded))
                    .foregroundStyle(.secondary)
                    .frame(width: 18, height: 18)
                    .background(Color.secondary.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                    .accessibilityLabel(agentLabel)
                Text(shortCwd)
                    .font(.system(.body, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 8)
                Text(timeAgo)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            metadataLine
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private var metadataLine: some View {
        HStack(spacing: 6) {
            if let host = session.hostname {
                Label(host, systemImage: "server.rack")
                    .labelStyle(.titleAndIcon)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let branch = session.gitBranch {
                Label(branch, systemImage: "arrow.triangle.branch")
                    .labelStyle(.titleAndIcon)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            if let remote = shortRemote {
                Text(remote)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            if let model = modelText {
                Text(model)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
    }

    /// Collapse `$HOME` prefix to `~`. Matches the mac app's formatting.
    private var shortCwd: String {
        let home = ProcessInfo.processInfo.environment["HOME"] ?? ""
        if !home.isEmpty, session.cwd.hasPrefix(home) {
            return "~" + session.cwd.dropFirst(home.count)
        }
        return session.cwd
    }

    /// Strip `https://github.com/` prefix and trailing `.git` so the remote
    /// reads like `owner/repo`.
    private var shortRemote: String? {
        guard let remote = session.gitRemote else { return nil }
        var stripped = remote
        if stripped.hasPrefix("https://github.com/") {
            stripped = String(stripped.dropFirst("https://github.com/".count))
        }
        if stripped.hasSuffix(".git") {
            stripped = String(stripped.dropLast(4))
        }
        return stripped
    }

    private var agentMonogram: String {
        switch session.agentKind {
        case .claude: return "C"
        case .codex: return "X"
        }
    }

    private var agentLabel: String {
        switch session.agentKind {
        case .claude: return "Claude"
        case .codex: return "Codex"
        }
    }

    private var modelText: String? {
        guard let model = session.model, !model.isEmpty else { return nil }
        return model
    }

    private var timeAgo: String {
        let diff = now.timeIntervalSince(session.updatedAt)
        if diff < 60 { return "\(max(0, Int(diff)))s" }
        if diff < 3600 { return "\(Int(diff / 60))m" }
        return "\(Int(diff / 3600))h"
    }
}
