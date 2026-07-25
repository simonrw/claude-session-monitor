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
}

/// One session as observed by a host's watcher, as carried inside a
/// [`SnapshotPayload`].
///
/// This is the watcher's publish shape, not the server's stored/broadcast
/// shape (`SessionView`): the host and agent kind apply to the whole
/// snapshot rather than to each session, and `name` (the `/rename` display
/// label) has no equivalent in `SessionView` yet - it is persisted by the
/// server but not exposed to clients until a later ticket wires up display.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};
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
            status: Status::Working(WorkingStatus { tool: None }),
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
            "status": { "type": "working", "tool": null },
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
            status: Status::Working(WorkingStatus { tool: None }),
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
            status: Status::Working(WorkingStatus {
                tool: Some("Bash".into()),
            }),
            agent_kind: AgentKind::Claude,
            model: None,
            updated_at: chrono::Utc::now(),
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        let restored: SessionView = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, view.session_id);
        assert_eq!(restored.cwd, view.cwd);
        assert_eq!(restored.status, view.status);
        assert_eq!(restored.agent_kind, AgentKind::Claude);
        assert_eq!(restored.model, None);
    }

    #[test]
    fn session_view_with_enrichment_fields_round_trips() {
        let view = SessionView {
            session_id: "enriched-view".into(),
            cwd: "/home/user/project".into(),
            status: Status::Working(WorkingStatus { tool: None }),
            agent_kind: AgentKind::Codex,
            model: Some("gpt-5.1-codex".into()),
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: Some("feature/foo".into()),
            git_remote: Some("https://github.com/org/repo.git".into()),
            tmux_target: Some("dev:1.0".into()),
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
                    status: Status::Working(WorkingStatus { tool: None }),
                    name: Some("my-session".into()),
                    git_branch: Some("main".into()),
                    git_remote: Some("https://github.com/user/repo.git".into()),
                    tmux_target: Some("main:0.1".into()),
                    model: None,
                },
                SnapshotSession {
                    session_id: "s2".into(),
                    cwd: "/home/user/other".into(),
                    status: Status::Waiting(WaitingStatus {
                        reason: WaitingReason::Input,
                        detail: Some("Shall I continue?".into()),
                    }),
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
    fn snapshot_session_omitted_optional_fields_default_to_none() {
        let json = serde_json::json!({
            "session_id": "s1",
            "cwd": "/home/user/project",
            "status": { "type": "working", "tool": null }
        });
        let restored: SnapshotSession = serde_json::from_value(json).unwrap();
        assert_eq!(restored.name, None);
        assert_eq!(restored.git_branch, None);
        assert_eq!(restored.git_remote, None);
        assert_eq!(restored.tmux_target, None);
        assert_eq!(restored.model, None);
    }
}
