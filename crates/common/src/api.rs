use crate::config::DEFAULT_SERVER_URL;
use crate::session::Status;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Resolve the coordination server URL.
///
/// Precedence, highest first: CLI arg, `CLAUDE_MONITOR_URL` env var, config-file
/// value, compiled-in default.
pub fn resolve_server_url(cli_arg: Option<&str>, file_value: Option<&str>) -> String {
    if let Some(url) = cli_arg {
        tracing::debug!(url, source = "cli_arg", "resolved server URL");
        return url.to_string();
    }
    if let Ok(url) = std::env::var("CLAUDE_MONITOR_URL") {
        tracing::debug!(url, source = "env", "resolved server URL");
        return url;
    }
    if let Some(url) = file_value {
        tracing::debug!(url, source = "file", "resolved server URL");
        return url.to_string();
    }
    tracing::debug!(
        url = DEFAULT_SERVER_URL,
        source = "default",
        "resolved server URL"
    );
    DEFAULT_SERVER_URL.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl Default for AgentKind {
    fn default() -> Self {
        Self::Claude
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportPayload {
    pub session_id: String,
    pub cwd: String,
    pub status: Status,
    #[serde(default)]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub notification_type: Option<String>,
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
    pub tmux_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub session_id: String,
    pub cwd: String,
    pub status: Status,
    #[serde(default)]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
    pub tmux_target: Option<String>,
    /// Display label set via Claude Code's `/rename`, as carried by
    /// [`SnapshotSession::name`] and persisted by the server. `None` for
    /// every Codex session (the Codex hook path has no equivalent field and
    /// never populates the stored column) and for any Claude session that
    /// was never renamed. Additive and optional throughout so a session
    /// with no name renders exactly as it did before this field existed -
    /// see PRO-215.
    #[serde(default)]
    pub name: Option<String>,
}

/// One session as observed by a host's watcher, as carried inside a
/// [`SnapshotPayload`].
///
/// This is the watcher's publish shape, not the server's stored/broadcast
/// shape (`SessionView`): the host and agent kind apply to the whole
/// snapshot rather than to each session. `name` (the `/rename` display
/// label) round-trips into `SessionView::name` (PRO-215) via the server's
/// `name` column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotSession {
    pub session_id: String,
    pub cwd: String,
    pub status: Status,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub git_remote: Option<String>,
    #[serde(default)]
    pub tmux_target: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// Body of `POST /api/hosts/{hostname}/sessions`: the complete set of live
/// sessions a host currently observes for one agent kind.
///
/// The hostname itself is not repeated here; it comes from the URL path.
/// Publishing a snapshot is idempotent: the server's view of this host's
/// sessions for this agent kind is replaced to match `sessions` exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub agent_kind: AgentKind,
    pub observed_at: DateTime<Utc>,
    pub sessions: Vec<SnapshotSession>,
}

/// One entry of `GET /api/hosts`: the last time the server accepted a
/// snapshot (`POST /api/hosts/{hostname}/sessions`) from this host and agent
/// kind, whether or not that snapshot actually changed anything.
///
/// This exists to let a client distinguish "this host genuinely has zero
/// live sessions right now" from "this host's watcher has stopped reporting
/// entirely" (PRO-211) - a distinction `SessionView`'s empty-list shape
/// cannot express on its own, since a watcher that legitimately empties its
/// snapshot and a watcher that has crashed or lost its network path both
/// eventually look identical: no active rows for that host. `last_seen_at`
/// is recorded independently of whether the snapshot contained any
/// sessions, so a host that only ever publishes empty snapshots still has a
/// fresh `last_seen_at`, while a host whose watcher has stopped publishing
/// altogether does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostStatus {
    pub hostname: String,
    pub agent_kind: AgentKind,
    pub last_seen_at: DateTime<Utc>,
}

/// How long a host can go without reporting before its watcher is treated as
/// having gone silent, as opposed to genuinely reporting zero sessions right
/// now (see [`HostStatus`]'s doc comment for that distinction).
///
/// Chosen against the watcher's 2-second default poll interval (`csm-watcher
/// --interval`): `last_seen_at` is refreshed on every successful publish,
/// changed or not, so a healthy watcher advances it roughly every 2s. A
/// client only observes that through its own poll of `GET /api/hosts`
/// though - `crates/common/src/view_model.rs`'s `HOST_STATUS_POLL_INTERVAL`
/// and `web/src/hooks/use-sessions.ts`'s `HOST_STATUS_POLL_INTERVAL_MS` are
/// both 10s - so under fully healthy operation `now - last_seen_at` can
/// already read as high as ~12s (one client poll interval plus one watcher
/// poll interval) with nothing actually wrong. 30s sits comfortably above
/// that ceiling, so a merely slow poll, one dropped beat, or a scheduling
/// hiccup never flips this, while a watcher that has genuinely gone silent
/// is still caught within one more client poll cycle after the threshold
/// elapses - well under a minute, not minutes.
pub const HOST_STALE_THRESHOLD_SECS: i64 = 30;

/// Whether a host last seen at `last_seen_at` should be treated as having
/// gone silent as of `now`. See [`HOST_STALE_THRESHOLD_SECS`] for the
/// threshold and the reasoning behind it. Free function (rather than only a
/// method on [`HostStatus`]) so the FFI boundary (`core-ffi`'s
/// `host_status_is_stale`) can expose the same comparison to Swift without
/// needing a full `HostStatus` round-trip.
pub fn host_is_stale(last_seen_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(last_seen_at) >= chrono::Duration::seconds(HOST_STALE_THRESHOLD_SECS)
}

impl HostStatus {
    /// Whether this host's watcher should be treated as having gone silent
    /// as of `now`. See [`host_is_stale`].
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        host_is_stale(self.last_seen_at, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Status;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn cli_arg_wins_over_env_file_and_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CLAUDE_MONITOR_URL", "http://env:7685") };
        let url = resolve_server_url(Some("http://cli:7685"), Some("http://file:7685"));
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        assert_eq!(url, "http://cli:7685");
    }

    #[test]
    fn env_wins_over_file_and_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CLAUDE_MONITOR_URL", "http://env:7685") };
        let url = resolve_server_url(None, Some("http://file:7685"));
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        assert_eq!(url, "http://env:7685");
    }

    #[test]
    fn file_wins_over_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        let url = resolve_server_url(None, Some("http://file:7685"));
        assert_eq!(url, "http://file:7685");
    }

    #[test]
    fn default_returned_when_no_other_source() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        let url = resolve_server_url(None, None);
        assert_eq!(url, "http://localhost:7685");
    }

    #[test]
    fn report_payload_serializes_and_deserializes() {
        let payload = ReportPayload {
            session_id: "abc123".into(),
            cwd: "/home/user/project".into(),
            status: Status::Busy { tool: None },
            agent_kind: AgentKind::Claude,
            model: None,
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let restored: ReportPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, payload.session_id);
        assert_eq!(restored.cwd, payload.cwd);
        assert_eq!(restored.hook_event_name, payload.hook_event_name);
        assert_eq!(restored.agent_kind, AgentKind::Claude);
        assert_eq!(restored.model, None);
    }

    #[test]
    fn agent_kind_uses_closed_wire_values() {
        assert_eq!(
            serde_json::to_string(&AgentKind::Claude).unwrap(),
            "\"claude\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Codex).unwrap(),
            "\"codex\""
        );
        assert_eq!(
            serde_json::from_str::<AgentKind>("\"claude\"").unwrap(),
            AgentKind::Claude
        );
        assert_eq!(
            serde_json::from_str::<AgentKind>("\"codex\"").unwrap(),
            AgentKind::Codex
        );
    }

    #[test]
    fn old_report_payload_defaults_agent_kind_to_claude() {
        let json = serde_json::json!({
            "session_id": "old-reporter",
            "cwd": "/home/user/project",
            "status": { "type": "busy", "tool": null },
            "hook_event_name": "SessionStart",
            "tool_name": null,
            "tool_input": null,
            "notification_type": null,
            "hostname": null,
            "git_branch": null,
            "git_remote": null,
            "tmux_target": null
        });
        let restored: ReportPayload = serde_json::from_value(json).unwrap();
        assert_eq!(restored.agent_kind, AgentKind::Claude);
        assert_eq!(restored.model, None);
    }

    #[test]
    fn report_payload_with_enrichment_fields_round_trips() {
        let payload = ReportPayload {
            session_id: "enriched-session".into(),
            cwd: "/home/user/project".into(),
            status: Status::Busy { tool: None },
            agent_kind: AgentKind::Codex,
            model: Some("gpt-5.1-codex".into()),
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: Some("myhost".into()),
            git_branch: Some("main".into()),
            git_remote: Some("https://github.com/user/repo.git".into()),
            tmux_target: Some("main:0.1".into()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let restored: ReportPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.hostname, Some("myhost".into()));
        assert_eq!(restored.git_branch, Some("main".into()));
        assert_eq!(
            restored.git_remote,
            Some("https://github.com/user/repo.git".into())
        );
        assert_eq!(restored.tmux_target, Some("main:0.1".into()));
        assert_eq!(restored.agent_kind, AgentKind::Codex);
        assert_eq!(restored.model, Some("gpt-5.1-codex".into()));
    }

    #[test]
    fn session_view_serializes_and_deserializes() {
        let view = SessionView {
            session_id: "abc123".into(),
            cwd: "/home/user/project".into(),
            status: Status::Busy {
                tool: Some("Bash".into()),
            },
            agent_kind: AgentKind::Claude,
            model: None,
            updated_at: chrono::Utc::now(),
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
            name: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        let restored: SessionView = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, view.session_id);
        assert_eq!(restored.cwd, view.cwd);
        assert_eq!(restored.status, view.status);
        assert_eq!(restored.agent_kind, AgentKind::Claude);
        assert_eq!(restored.model, None);
        assert_eq!(restored.name, None);
    }

    #[test]
    fn session_view_with_enrichment_fields_round_trips() {
        let view = SessionView {
            session_id: "enriched-view".into(),
            cwd: "/home/user/project".into(),
            status: Status::Busy { tool: None },
            agent_kind: AgentKind::Codex,
            model: Some("gpt-5.1-codex".into()),
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: Some("feature/foo".into()),
            git_remote: Some("https://github.com/org/repo.git".into()),
            tmux_target: Some("dev:1.0".into()),
            name: Some("captain-marvel".into()),
        };
        let json = serde_json::to_string(&view).unwrap();
        let restored: SessionView = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.hostname, Some("myhost".into()));
        assert_eq!(restored.git_branch, Some("feature/foo".into()));
        assert_eq!(
            restored.git_remote,
            Some("https://github.com/org/repo.git".into())
        );
        assert_eq!(restored.tmux_target, Some("dev:1.0".into()));
        assert_eq!(restored.agent_kind, AgentKind::Codex);
        assert_eq!(restored.model, Some("gpt-5.1-codex".into()));
        assert_eq!(restored.name, Some("captain-marvel".into()));
    }

    #[test]
    fn session_view_omitted_name_defaults_to_none() {
        let json = serde_json::json!({
            "session_id": "s1",
            "cwd": "/home/user/project",
            "status": { "type": "busy", "tool": null },
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "hostname": null,
            "git_branch": null,
            "git_remote": null,
            "tmux_target": null
        });
        let restored: SessionView = serde_json::from_value(json).unwrap();
        assert_eq!(restored.name, None);
    }

    #[test]
    fn snapshot_payload_serializes_and_deserializes() {
        let payload = SnapshotPayload {
            agent_kind: AgentKind::Claude,
            observed_at: chrono::Utc::now(),
            sessions: vec![
                SnapshotSession {
                    session_id: "s1".into(),
                    cwd: "/home/user/project".into(),
                    status: Status::Busy { tool: None },
                    name: Some("my-session".into()),
                    git_branch: Some("main".into()),
                    git_remote: Some("https://github.com/user/repo.git".into()),
                    tmux_target: Some("main:0.1".into()),
                    model: None,
                },
                SnapshotSession {
                    session_id: "s2".into(),
                    cwd: "/home/user/other".into(),
                    status: Status::Waiting {
                        detail: Some("Shall I continue?".into()),
                    },
                    name: None,
                    git_branch: None,
                    git_remote: None,
                    tmux_target: None,
                    model: None,
                },
            ],
        };
        let json = serde_json::to_string(&payload).unwrap();
        let restored: SnapshotPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, payload);
    }

    #[test]
    fn host_status_serializes_and_deserializes() {
        let status = HostStatus {
            hostname: "myhost".into(),
            agent_kind: AgentKind::Claude,
            last_seen_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let restored: HostStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn host_status_not_stale_when_freshly_seen() {
        let now = chrono::Utc::now();
        assert!(!host_is_stale(now, now));
    }

    #[test]
    fn host_status_not_stale_just_under_threshold() {
        let now = chrono::Utc::now();
        let last_seen_at = now - chrono::Duration::seconds(HOST_STALE_THRESHOLD_SECS - 1);
        assert!(!host_is_stale(last_seen_at, now));
    }

    #[test]
    fn host_status_stale_at_threshold() {
        let now = chrono::Utc::now();
        let last_seen_at = now - chrono::Duration::seconds(HOST_STALE_THRESHOLD_SECS);
        assert!(host_is_stale(last_seen_at, now));
    }

    #[test]
    fn host_status_stale_well_past_threshold() {
        let now = chrono::Utc::now();
        let last_seen_at = now - chrono::Duration::minutes(5);
        assert!(host_is_stale(last_seen_at, now));
    }

    #[test]
    fn host_status_is_stale_method_matches_free_function() {
        let now = chrono::Utc::now();
        let status = HostStatus {
            hostname: "myhost".into(),
            agent_kind: AgentKind::Claude,
            last_seen_at: now - chrono::Duration::minutes(1),
        };
        assert!(status.is_stale(now));
    }

    #[test]
    fn snapshot_session_omitted_optional_fields_default_to_none() {
        let json = serde_json::json!({
            "session_id": "s1",
            "cwd": "/home/user/project",
            "status": { "type": "busy", "tool": null }
        });
        let restored: SnapshotSession = serde_json::from_value(json).unwrap();
        assert_eq!(restored.name, None);
        assert_eq!(restored.git_branch, None);
        assert_eq!(restored.git_remote, None);
        assert_eq!(restored.tmux_target, None);
        assert_eq!(restored.model, None);
    }
}
