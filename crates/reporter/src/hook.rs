use common::api::AgentKind;
use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};
use serde::Deserialize;

#[derive(Debug)]
pub struct NormalizedHookEvent {
    pub agent_kind: AgentKind,
    pub session_id: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub notification_type: Option<String>,
}

pub type HookEvent = NormalizedHookEvent;

#[derive(Debug, Deserialize)]
struct ClaudeHookEvent {
    session_id: String,
    cwd: String,
    hook_event_name: String,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    notification_type: Option<String>,
    // Fields present in some hooks but not used for status derivation
    #[serde(flatten)]
    _extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CodexHookEvent {
    session_id: String,
    cwd: String,
    hook_event_name: String,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    #[serde(flatten)]
    _extra: std::collections::HashMap<String, serde_json::Value>,
}

impl From<ClaudeHookEvent> for NormalizedHookEvent {
    fn from(event: ClaudeHookEvent) -> Self {
        Self {
            agent_kind: AgentKind::Claude,
            session_id: event.session_id,
            cwd: event.cwd,
            hook_event_name: event.hook_event_name,
            tool_name: event.tool_name,
            tool_input: event.tool_input,
            notification_type: event.notification_type,
        }
    }
}

impl From<CodexHookEvent> for NormalizedHookEvent {
    fn from(event: CodexHookEvent) -> Self {
        Self {
            agent_kind: AgentKind::Codex,
            session_id: event.session_id,
            cwd: event.cwd,
            hook_event_name: event.hook_event_name,
            tool_name: event.tool_name,
            tool_input: event.tool_input,
            notification_type: None,
        }
    }
}

pub fn parse_hook_event(
    agent_kind: AgentKind,
    input: &str,
) -> Result<NormalizedHookEvent, serde_json::Error> {
    match agent_kind {
        AgentKind::Claude => serde_json::from_str::<ClaudeHookEvent>(input).map(Into::into),
        AgentKind::Codex => serde_json::from_str::<CodexHookEvent>(input).map(Into::into),
    }
}

pub fn derive_status(event: &HookEvent) -> Status {
    match (event.agent_kind, event.hook_event_name.as_str()) {
        (_, "SessionStart") | (_, "UserPromptSubmit") => {
            Status::Working(WorkingStatus { tool: None })
        }
        (_, "PreToolUse") => Status::Working(WorkingStatus {
            tool: event.tool_name.clone(),
        }),
        (_, "PostToolUse") => Status::Working(WorkingStatus { tool: None }),
        (_, "Notification") => {
            if event.notification_type.as_deref() == Some("permission_prompt") {
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Permission,
                    detail: None,
                })
            } else {
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Input,
                    detail: None,
                })
            }
        }
        (_, "Stop") => Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: None,
        }),
        (AgentKind::Claude, "SessionEnd") => Status::Ended,
        _ => Status::Working(WorkingStatus {
            tool: event.tool_name.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::api::AgentKind;

    fn make_event(hook_event_name: &str, tool_name: Option<&str>) -> HookEvent {
        HookEvent {
            agent_kind: AgentKind::Claude,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            hook_event_name: hook_event_name.into(),
            tool_name: tool_name.map(String::from),
            tool_input: None,
            notification_type: None,
        }
    }

    #[test]
    fn session_start_derives_working_no_tool() {
        let event = make_event("SessionStart", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn other_hook_with_tool_derives_working_with_tool() {
        let event = make_event("PreToolUse", Some("Bash"));
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Working(WorkingStatus {
                tool: Some("Bash".into())
            })
        );
    }

    #[test]
    fn user_prompt_submit_derives_working_no_tool() {
        let event = make_event("UserPromptSubmit", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn pre_tool_use_with_tool_derives_working_with_tool() {
        let event = make_event("PreToolUse", Some("Bash"));
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Working(WorkingStatus {
                tool: Some("Bash".into())
            })
        );
    }

    #[test]
    fn post_tool_use_clears_tool() {
        let event = make_event("PostToolUse", Some("Bash"));
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn notification_permission_prompt_derives_waiting_permission() {
        let mut event = make_event("Notification", None);
        event.notification_type = Some("permission_prompt".into());
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Permission,
                detail: None
            })
        );
    }

    #[test]
    fn notification_idle_prompt_derives_waiting_input() {
        let mut event = make_event("Notification", None);
        event.notification_type = Some("idle_prompt".into());
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Input,
                detail: None
            })
        );
    }

    #[test]
    fn notification_no_type_derives_waiting_input() {
        let event = make_event("Notification", None);
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Input,
                detail: None
            })
        );
    }

    #[test]
    fn stop_derives_waiting_input() {
        let event = make_event("Stop", None);
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Input,
                detail: None
            })
        );
    }

    #[test]
    fn session_end_derives_ended() {
        let event = make_event("SessionEnd", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Ended);
    }

    #[test]
    fn claude_unknown_permission_request_still_defaults_to_working() {
        let json = r#"{
            "session_id": "claude-session",
            "cwd": "/work/project",
            "hook_event_name": "PermissionRequest",
            "tool_input": {
                "description": "Codex-shaped data should not alter Claude behavior"
            }
        }"#;

        let event = parse_hook_event(AgentKind::Claude, json).unwrap();
        let status = derive_status(&event);

        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn session_start_hook_event_parses_from_json() {
        let json = r#"{
            "session_id": "abc",
            "cwd": "/tmp",
            "hook_event_name": "SessionStart",
            "permission_mode": "default",
            "transcript_path": "/tmp/t"
        }"#;
        let event = parse_hook_event(AgentKind::Claude, json).unwrap();
        assert_eq!(event.session_id, "abc");
        assert_eq!(event.hook_event_name, "SessionStart");
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn claude_parser_normalizes_existing_session_start_payload() {
        let json = r#"{
            "session_id": "abc",
            "cwd": "/tmp",
            "hook_event_name": "SessionStart",
            "permission_mode": "default",
            "transcript_path": "/tmp/t"
        }"#;

        let event = parse_hook_event(AgentKind::Claude, json).unwrap();

        assert_eq!(event.session_id, "abc");
        assert_eq!(event.cwd, "/tmp");
        assert_eq!(event.hook_event_name, "SessionStart");
        assert_eq!(
            derive_status(&event),
            Status::Working(WorkingStatus { tool: None })
        );
    }

    #[test]
    fn codex_parser_does_not_apply_claude_notification_permission_shape() {
        let json = r#"{
            "session_id": "codex-session",
            "cwd": "/work/project",
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt"
        }"#;

        let event = parse_hook_event(AgentKind::Codex, json).unwrap();
        let status = derive_status(&event);

        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Input,
                detail: None
            })
        );
    }
}
