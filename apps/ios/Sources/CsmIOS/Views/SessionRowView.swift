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

    private var timeAgo: String {
        let diff = now.timeIntervalSince(session.updatedAt)
        if diff < 60 { return "\(max(0, Int(diff)))s" }
        if diff < 3600 { return "\(Int(diff / 60))m" }
        return "\(Int(diff / 3600))h"
    }
}
