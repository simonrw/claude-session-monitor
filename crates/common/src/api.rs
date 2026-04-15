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

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportPayload {
    pub session_id: String,
    pub cwd: String,
    pub status: Status,
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub notification_type: Option<String>,
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub session_id: String,
    pub cwd: String,
    pub status: Status,
    pub updated_at: DateTime<Utc>,
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Status, WorkingStatus};

    #[test]
    fn cli_arg_wins_over_env_file_and_default() {
        unsafe { std::env::set_var("CLAUDE_MONITOR_URL", "http://env:7685") };
        let url = resolve_server_url(Some("http://cli:7685"), Some("http://file:7685"));
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        assert_eq!(url, "http://cli:7685");
    }

    #[test]
    fn env_wins_over_file_and_default() {
        unsafe { std::env::set_var("CLAUDE_MONITOR_URL", "http://env:7685") };
        let url = resolve_server_url(None, Some("http://file:7685"));
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        assert_eq!(url, "http://env:7685");
    }

    #[test]
    fn file_wins_over_default() {
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        let url = resolve_server_url(None, Some("http://file:7685"));
        assert_eq!(url, "http://file:7685");
    }

    #[test]
    fn default_returned_when_no_other_source() {
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
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: None,
            git_branch: None,
            git_remote: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let restored: ReportPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, payload.session_id);
        assert_eq!(restored.cwd, payload.cwd);
        assert_eq!(restored.hook_event_name, payload.hook_event_name);
    }

    #[test]
    fn report_payload_with_enrichment_fields_round_trips() {
        let payload = ReportPayload {
            session_id: "enriched-session".into(),
            cwd: "/home/user/project".into(),
            status: Status::Working(WorkingStatus { tool: None }),
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: Some("myhost".into()),
            git_branch: Some("main".into()),
            git_remote: Some("https://github.com/user/repo.git".into()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let restored: ReportPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.hostname, Some("myhost".into()));
        assert_eq!(restored.git_branch, Some("main".into()));
        assert_eq!(
            restored.git_remote,
            Some("https://github.com/user/repo.git".into())
        );
    }

    #[test]
    fn session_view_serializes_and_deserializes() {
        let view = SessionView {
            session_id: "abc123".into(),
            cwd: "/home/user/project".into(),
            status: Status::Working(WorkingStatus {
                tool: Some("Bash".into()),
            }),
            updated_at: chrono::Utc::now(),
            hostname: None,
            git_branch: None,
            git_remote: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        let restored: SessionView = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, view.session_id);
        assert_eq!(restored.cwd, view.cwd);
        assert_eq!(restored.status, view.status);
    }

    #[test]
    fn session_view_with_enrichment_fields_round_trips() {
        let view = SessionView {
            session_id: "enriched-view".into(),
            cwd: "/home/user/project".into(),
            status: Status::Working(WorkingStatus { tool: None }),
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: Some("feature/foo".into()),
            git_remote: Some("https://github.com/org/repo.git".into()),
        };
        let json = serde_json::to_string(&view).unwrap();
        let restored: SessionView = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.hostname, Some("myhost".into()));
        assert_eq!(restored.git_branch, Some("feature/foo".into()));
        assert_eq!(
            restored.git_remote,
            Some("https://github.com/org/repo.git".into())
        );
    }
}
