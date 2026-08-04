//! UI-agnostic presentation rules for the session monitor.
//!
//! These are the rules a frontend needs to turn the raw session/host model
//! (`api::SessionView`, `api::HostStatus`, `session::Status`) into something a
//! human reads: how to order sessions, when to dim them, what label and colour
//! a status gets, and how to shorten paths and remotes. They lived inline in
//! the egui GUI's render code (PRO-221); promoting them here keeps the TUI from
//! becoming a fourth copy alongside the Rust GUI, Swift, and web clients.
//!
//! Everything here is deliberately UI-toolkit-free: colours are expressed as
//! [`Rgb`] identities that each frontend maps to its own colour type (egui's
//! `Color32`, ratatui's `Color`), and time-dependent rules take an explicit
//! `now` so they are pure and testable.

use crate::api::{HostStatus, SessionView, host_is_stale};
use crate::session::Status;
use chrono::{DateTime, Utc};

/// How long a session can go without an update before it is treated as stale
/// (and thus faded). Distinct from the host-level
/// [`crate::api::HOST_STALE_THRESHOLD_SECS`]: this is about an individual
/// session sitting untouched, not a watcher going silent.
pub const SESSION_STALE_THRESHOLD_MINS: i64 = 30;

/// Whether a session last updated at `updated_at` counts as stale as of `now`.
pub fn is_stale(updated_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(updated_at) >= chrono::Duration::minutes(SESSION_STALE_THRESHOLD_MINS)
}

/// Whether a session should be rendered faded: either the client is
/// disconnected (so every session's freshness is suspect) or the session
/// itself has gone stale.
pub fn should_fade(connected: bool, stale: bool) -> bool {
    !connected || stale
}

/// Whether the empty session list should be explained as "the watcher isn't
/// reporting" rather than "genuinely no sessions right now" - the PRO-211/
/// PRO-214 distinction also wired into the web client's `noHostsReported`
/// and mac/iOS's `hasReceivedHostStatus`/`hosts` checks.
///
/// True when either no host has ever reported (`hosts` empty), or every host
/// that has reported has gone stale as of `now` (see
/// [`crate::api::host_is_stale`] for the threshold and why it was chosen) -
/// the case a plain `hosts.is_empty()` check misses: a watcher that reported
/// once and then died leaves `hosts` non-empty forever with a frozen
/// `last_seen_at`, which would otherwise look like a perfectly healthy,
/// silent watcher.
///
/// Only meaningful once `has_received_host_status` is true: before the first
/// `GET /api/hosts` poll lands, an empty `hosts` is ambiguous with "haven't
/// heard back yet" rather than "watcher is silent".
pub fn watcher_appears_silent(
    hosts: &[HostStatus],
    has_received_host_status: bool,
    now: DateTime<Utc>,
) -> bool {
    has_received_host_status
        && (hosts.is_empty() || hosts.iter().all(|h| host_is_stale(h.last_seen_at, now)))
}

/// Splits sessions into "waiting for you" (top) and everything else
/// (bottom, sorted most-recently-updated first).
///
/// The second bucket is deliberately not called "working": it also holds
/// [`Status::Idle`] and [`Status::Ended`] sessions, exactly as it held
/// [`Status::Ended`] before PRO-214 (the previous binary
/// Waiting/everything-else split already lumped `Ended` in with `Working`
/// here). The frontend only makes one cut - "does this need me right now" -
/// and [`status_label`]/[`status_color`] are what carry the finer-grained
/// Busy/Shell/Idle/Ended distinction within that bottom bucket.
pub fn partition_sessions(sessions: &[SessionView]) -> (Vec<&SessionView>, Vec<&SessionView>) {
    let mut waiting = Vec::new();
    let mut other = Vec::new();
    for session in sessions {
        match &session.status {
            Status::Waiting { .. } => waiting.push(session),
            _ => other.push(session),
        }
    }
    other.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    (waiting, other)
}

/// The one-line status label shown against a session: `busy`, `busy(Tool)`,
/// `shell`, `idle`, `waiting(detail)` (falling back to bare `waiting` when the
/// detail is absent or empty), and `ended`.
pub fn status_label(status: &Status) -> String {
    match status {
        Status::Busy { tool } => match tool {
            Some(tool) => format!("busy({})", tool),
            None => "busy".into(),
        },
        Status::Shell => "shell".into(),
        Status::Idle => "idle".into(),
        Status::Waiting { detail } => match detail.as_deref() {
            Some(detail) if !detail.is_empty() => format!("waiting({})", detail),
            _ => "waiting".into(),
        },
        Status::Ended => "ended".into(),
    }
}

/// A UI-toolkit-agnostic colour identity. Frontends map this to their own
/// colour type (egui `Color32`, ratatui `Color`) rather than the module
/// depending on any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Colour identity per [`Status`] variant.
///
/// `Waiting` no longer carries a Permission/Input distinction (removed with
/// `WaitingReason` - see [`Status`]'s doc comment), so it gets a single red,
/// same as the old Permission colour, since Waiting is unconditionally the
/// state that most wants the user's attention. `Busy` keeps the old Working
/// green. `Shell` gets its own teal rather than reusing green: it is a
/// genuinely new, previously-unrepresentable state (a foreground shell
/// command), and giving it a distinct colour lets a user tell "the model is
/// thinking/tool-calling" apart from "a shell command is running" at a glance.
/// `Idle` gets a muted blue-gray, distinct from `Ended`'s gray, so "finished
/// this turn, still a live session" doesn't read as "gone".
pub fn status_color(status: &Status) -> Rgb {
    match status {
        Status::Busy { .. } => Rgb::new(80, 200, 120),
        Status::Shell => Rgb::new(70, 170, 190),
        Status::Idle => Rgb::new(140, 150, 190),
        Status::Waiting { .. } => Rgb::new(220, 80, 80),
        // Matches egui's `Color32::GRAY` (160,160,160), the value the GUI
        // used for `Ended` before this rule moved here.
        Status::Ended => Rgb::new(160, 160, 160),
    }
}

/// Replaces a leading `home` prefix in `cwd` with `~`. An empty `home` (or a
/// `cwd` that doesn't live under it) is returned unchanged.
pub fn shorten_cwd(cwd: &str, home: &str) -> String {
    if !home.is_empty() && cwd.starts_with(home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd.to_owned()
    }
}

/// Strips the `https://github.com/` scheme/host prefix and a trailing `.git`
/// from a git remote, leaving the bare `owner/repo`. A remote that matches
/// neither is returned unchanged.
pub fn strip_git_remote(remote: &str) -> String {
    let stripped = remote.strip_prefix("https://github.com/").unwrap_or(remote);
    let stripped = stripped.strip_suffix(".git").unwrap_or(stripped);
    stripped.to_owned()
}

/// Human "N ago" for how long since `updated_at` as of `now`: seconds under a
/// minute, whole minutes above. Never renders a negative value (a clock skew
/// putting `updated_at` in the future clamps to `0s ago`).
pub fn relative_time(updated_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let diff = now.signed_duration_since(updated_at);
    if diff.num_seconds() < 60 {
        format!("{}s ago", diff.num_seconds().max(0))
    } else {
        format!("{}m ago", diff.num_minutes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::AgentKind;

    fn make_session(id: &str, status: Status, updated_at: DateTime<Utc>) -> SessionView {
        SessionView {
            session_id: id.into(),
            cwd: "/tmp/project".into(),
            status,
            agent_kind: AgentKind::Claude,
            model: None,
            updated_at,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
            name: None,
        }
    }

    fn make_host(hostname: &str, last_seen_at: DateTime<Utc>) -> HostStatus {
        HostStatus {
            hostname: hostname.into(),
            agent_kind: AgentKind::Claude,
            last_seen_at,
        }
    }

    // A fixed `now` so every time-dependent rule is deterministic.
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // --- watcher_appears_silent ---

    #[test]
    fn watcher_not_silent_before_first_host_status_poll() {
        assert!(!watcher_appears_silent(&[], false, now()));
    }

    #[test]
    fn watcher_silent_when_no_host_has_ever_reported() {
        assert!(watcher_appears_silent(&[], true, now()));
    }

    #[test]
    fn watcher_not_silent_with_a_freshly_seen_host() {
        let hosts = vec![make_host("mbp", now())];
        assert!(!watcher_appears_silent(&hosts, true, now()));
    }

    #[test]
    fn watcher_silent_once_its_only_host_goes_stale() {
        let hosts = vec![make_host("mbp", now() - chrono::Duration::minutes(5))];
        assert!(watcher_appears_silent(&hosts, true, now()));
    }

    #[test]
    fn watcher_not_silent_if_any_host_is_still_fresh() {
        let hosts = vec![
            make_host("dead-host", now() - chrono::Duration::minutes(5)),
            make_host("live-host", now()),
        ];
        assert!(!watcher_appears_silent(&hosts, true, now()));
    }

    // --- staleness / fade ---

    #[test]
    fn stale_at_thirty_minutes() {
        assert!(is_stale(now() - chrono::Duration::minutes(30), now()));
    }

    #[test]
    fn not_stale_at_twenty_nine_minutes() {
        assert!(!is_stale(now() - chrono::Duration::minutes(29), now()));
    }

    #[test]
    fn not_faded_when_connected_and_fresh() {
        assert!(!should_fade(true, false));
    }

    #[test]
    fn faded_when_connected_and_stale() {
        assert!(should_fade(true, true));
    }

    #[test]
    fn faded_when_disconnected_and_fresh() {
        assert!(should_fade(false, false));
    }

    #[test]
    fn faded_when_disconnected_and_stale() {
        assert!(should_fade(false, true));
    }

    // --- partition_sessions ---

    #[test]
    fn partition_waiting_to_top() {
        let sessions = vec![
            make_session("s1", Status::Waiting { detail: None }, now()),
            make_session(
                "s2",
                Status::Waiting {
                    detail: Some("Allow Bash to run rm?".into()),
                },
                now(),
            ),
        ];
        let (top, bottom) = partition_sessions(&sessions);
        assert_eq!(top.len(), 2);
        assert_eq!(bottom.len(), 0);
    }

    #[test]
    fn partition_busy_shell_idle_ended_to_bottom() {
        let sessions = vec![
            make_session("s1", Status::Busy { tool: None }, now()),
            make_session("s2", Status::Shell, now()),
            make_session("s3", Status::Idle, now()),
            make_session("s4", Status::Ended, now()),
        ];
        let (top, bottom) = partition_sessions(&sessions);
        assert_eq!(top.len(), 0);
        assert_eq!(bottom.len(), 4);
    }

    #[test]
    fn partition_bottom_sorted_by_updated_at_desc() {
        let older = now() - chrono::Duration::minutes(5);
        let sessions = vec![
            make_session("s1", Status::Busy { tool: None }, older),
            make_session("s2", Status::Busy { tool: None }, now()),
        ];
        let (_, bottom) = partition_sessions(&sessions);
        assert_eq!(bottom[0].session_id, "s2");
        assert_eq!(bottom[1].session_id, "s1");
    }

    // --- status_label ---

    #[test]
    fn label_busy_without_tool() {
        assert_eq!(status_label(&Status::Busy { tool: None }), "busy");
    }

    #[test]
    fn label_busy_with_tool() {
        assert_eq!(
            status_label(&Status::Busy {
                tool: Some("Bash".into())
            }),
            "busy(Bash)"
        );
    }

    #[test]
    fn label_shell_idle_ended() {
        assert_eq!(status_label(&Status::Shell), "shell");
        assert_eq!(status_label(&Status::Idle), "idle");
        assert_eq!(status_label(&Status::Ended), "ended");
    }

    #[test]
    fn label_waiting_with_detail() {
        assert_eq!(
            status_label(&Status::Waiting {
                detail: Some("Continue?".into())
            }),
            "waiting(Continue?)"
        );
    }

    #[test]
    fn label_waiting_falls_back_to_bare_on_empty_or_missing_detail() {
        assert_eq!(status_label(&Status::Waiting { detail: None }), "waiting");
        assert_eq!(
            status_label(&Status::Waiting {
                detail: Some(String::new())
            }),
            "waiting"
        );
    }

    // --- status_color ---

    #[test]
    fn color_identities_are_distinct_per_variant() {
        assert_eq!(status_color(&Status::Busy { tool: None }), Rgb::new(80, 200, 120));
        assert_eq!(status_color(&Status::Shell), Rgb::new(70, 170, 190));
        assert_eq!(status_color(&Status::Idle), Rgb::new(140, 150, 190));
        assert_eq!(status_color(&Status::Waiting { detail: None }), Rgb::new(220, 80, 80));
        assert_eq!(status_color(&Status::Ended), Rgb::new(160, 160, 160));
    }

    // --- shorten_cwd ---

    #[test]
    fn shorten_cwd_replaces_home_with_tilde() {
        assert_eq!(
            shorten_cwd("/Users/me/dev/project", "/Users/me"),
            "~/dev/project"
        );
    }

    #[test]
    fn shorten_cwd_leaves_paths_outside_home_alone() {
        assert_eq!(shorten_cwd("/tmp/project", "/Users/me"), "/tmp/project");
    }

    #[test]
    fn shorten_cwd_with_empty_home_is_unchanged() {
        assert_eq!(shorten_cwd("/tmp/project", ""), "/tmp/project");
    }

    // --- strip_git_remote ---

    #[test]
    fn strip_git_remote_removes_github_prefix_and_git_suffix() {
        assert_eq!(
            strip_git_remote("https://github.com/owner/repo.git"),
            "owner/repo"
        );
    }

    #[test]
    fn strip_git_remote_without_git_suffix() {
        assert_eq!(
            strip_git_remote("https://github.com/owner/repo"),
            "owner/repo"
        );
    }

    #[test]
    fn strip_git_remote_leaves_unrecognised_remote_alone() {
        assert_eq!(
            strip_git_remote("git@example.com:owner/repo"),
            "git@example.com:owner/repo"
        );
    }

    // --- relative_time ---

    #[test]
    fn relative_time_under_a_minute_in_seconds() {
        assert_eq!(relative_time(now() - chrono::Duration::seconds(5), now()), "5s ago");
    }

    #[test]
    fn relative_time_at_and_over_a_minute_in_minutes() {
        assert_eq!(relative_time(now() - chrono::Duration::seconds(90), now()), "1m ago");
    }

    #[test]
    fn relative_time_clamps_future_to_zero_seconds() {
        assert_eq!(relative_time(now() + chrono::Duration::seconds(5), now()), "0s ago");
    }
}
