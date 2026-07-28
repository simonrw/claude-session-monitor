import AppKit
import CsmCore
import SwiftUI

/// Pure helpers for rendering a [`SessionView`] in the popover. Kept
/// separate from the SwiftUI layout so unit tests can exercise them without
/// bringing up a view hierarchy.
enum SessionDisplay {
    /// Compact agent marker used in dense rows.
    static func agentMonogram(for session: SessionView) -> String {
        switch session.agentKind {
        case .claude: return "C"
        case .codex: return "X"
        }
    }

    static func agentLabel(for session: SessionView) -> String {
        switch session.agentKind {
        case .claude: return "Claude"
        case .codex: return "Codex"
        }
    }

    static func agentModelText(for session: SessionView) -> String? {
        guard let model = session.model, !model.isEmpty else { return nil }
        return model
    }

    /// `/rename` display name, or `nil` when unset/empty. The name is the
    /// user-chosen label of intent; `locationText` (cwd-based) stays put
    /// alongside it rather than being replaced, since that's still how a
    /// session in a shared project is told apart from others - see
    /// `PopoverView.SessionRow`, which shows this above `locationText` only
    /// when non-nil, so a session with no name renders exactly as before
    /// (PRO-215).
    static func nameText(for session: SessionView) -> String? {
        guard let name = session.name, !name.isEmpty else { return nil }
        return name
    }

    /// Status-line text, e.g. "busy(Bash)" / "shell" / "idle" /
    /// "waiting(continue?)". Mirrors the Rust GUI's `render_session` status
    /// string exactly (see `crates/gui/src/main.rs`) so the two clients read
    /// identically.
    static func statusText(_ status: Status) -> String {
        switch status {
        case .busy(let tool):
            if let tool, !tool.isEmpty { return "busy(\(tool))" }
            return "busy"
        case .shell:
            return "shell"
        case .idle:
            return "idle"
        case .waiting(let detail):
            if let detail, !detail.isEmpty {
                return "waiting(\(detail))"
            }
            return "waiting"
        case .ended:
            return "ended"
        }
    }

    /// Priority-coloured SwiftUI `Color` matching the status — mirrors the
    /// egui `status_color` logic (`crates/gui/src/main.rs`) so the menu-bar
    /// and the popover agree. `Waiting` no longer carries a
    /// Permission/Input distinction (removed with `WaitingReason`), so it
    /// gets a single red - unconditionally the state that most wants the
    /// user's attention. `Busy` keeps the old "working" green. `Shell` gets
    /// its own teal rather than reusing green: it is a genuinely new,
    /// previously-unrepresentable state (a foreground shell command
    /// running), and a distinct color lets a user tell "the model is
    /// thinking/tool-calling" apart from "a shell command is running" at a
    /// glance. `Idle` gets a muted blue, distinct from `Ended`'s gray, so
    /// "finished this turn, still a live session" doesn't read as "gone".
    static func statusColor(_ status: Status) -> Color {
        switch status {
        case .busy: return .green
        case .shell: return .teal
        case .idle: return .blue
        case .waiting: return .red
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
