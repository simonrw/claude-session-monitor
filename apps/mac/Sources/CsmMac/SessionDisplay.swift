import AppKit
import CsmCore
import SwiftUI

/// Pure helpers for rendering a [`SessionView`] in the popover. Kept
/// separate from the SwiftUI layout so unit tests can exercise them without
/// bringing up a view hierarchy.
enum SessionDisplay {
    /// Status-line text, e.g. "working(Bash)" / "waiting(input: continue?)".
    static func statusText(_ status: Status) -> String {
        switch status {
        case .working(let tool):
            if let tool, !tool.isEmpty { return "working(\(tool))" }
            return "working"
        case .waiting(let reason, let detail):
            let r: String
            switch reason {
            case .permission: r = "permission"
            case .input: r = "input"
            }
            if let detail, !detail.isEmpty {
                return "waiting(\(r): \(detail))"
            }
            return "waiting(\(r))"
        case .ended:
            return "ended"
        }
    }

    /// Priority-coloured SwiftUI `Color` matching the status — mirrors the
    /// egui `status_color` logic so the menu-bar and the popover agree.
    static func statusColor(_ status: Status) -> Color {
        switch status {
        case .working: return .green
        case .waiting(let reason, _):
            switch reason {
            case .permission: return .red
            case .input: return .orange
            }
        case .ended: return .gray
        }
    }

    /// `hostname:cwd (branch → repo)` line. Reproduces the egui GUI format.
    static func locationText(for session: SessionView) -> String {
        let home = ProcessInfo.processInfo.environment["HOME"] ?? ""
        var shortCwd = session.cwd
        if !home.isEmpty, shortCwd.hasPrefix(home) {
            shortCwd = "~" + shortCwd.dropFirst(home.count)
        }
        var repoPart: String? = nil
        if let remote = session.gitRemote {
            var stripped = remote
            if stripped.hasPrefix("https://github.com/") {
                stripped = String(stripped.dropFirst("https://github.com/".count))
            }
            if stripped.hasSuffix(".git") {
                stripped = String(stripped.dropLast(4))
            }
            repoPart = stripped
        }
        var branchRepo = ""
        switch (session.gitBranch, repoPart) {
        case (.some(let b), .some(let r)): branchRepo = " (\(b) \u{2192} \(r))"
        case (.some(let b), .none): branchRepo = " (\(b))"
        default: break
        }
        if let host = session.hostname {
            return "\(host):\(shortCwd)\(branchRepo)"
        }
        return "\(shortCwd)\(branchRepo)"
    }

    /// "Ns ago" / "Nm ago" relative time.
    static func relativeTime(for session: SessionView, now: Date = Date()) -> String {
        let diff = now.timeIntervalSince(session.updatedAt)
        if diff < 60 { return "\(max(0, Int(diff)))s ago" }
        return "\(Int(diff / 60))m ago"
    }
}
