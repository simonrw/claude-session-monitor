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
    pub model: Option<String>,
}

pub type HookEvent = NormalizedHookEvent;

/// Codex's hook event shape. Claude Code sessions are tracked by
/// `csm-watcher` instead (see PRO-213); the reporter only ever parses
/// Codex events now, and `--agent claude` is rejected before parsing is
/// reached (see `main.rs`).
#[derive(Debug, Deserialize)]
struct CodexHookEvent {
    session_id: String,
    cwd: String,
    hook_event_name: String,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    model: Option<String>,
    #[serde(flatten)]
    _extra: std::collections::HashMap<String, serde_json::Value>,
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
            model: event.model,
        }
    }
}

pub fn parse_hook_event(input: &str) -> Result<NormalizedHookEvent, serde_json::Error> {
    serde_json::from_str::<CodexHookEvent>(input).map(Into::into)
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
        (AgentKind::Codex, "PermissionRequest") => Status::Waiting(WaitingStatus {
            reason: WaitingReason::Permission,
            detail: tool_input_description(event),
        }),
        (_, "Stop") => Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: None,
        }),
        _ => Status::Working(WorkingStatus {
            tool: event.tool_name.clone(),
        }),
    }
}

fn tool_input_description(event: &HookEvent) -> Option<String> {
    event
        .tool_input
        .as_ref()
        .and_then(|input| input.get("description"))
        .and_then(|description| description.as_str())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::api::AgentKind;

    fn make_event(hook_event_name: &str, tool_name: Option<&str>) -> HookEvent {
        HookEvent {
            agent_kind: AgentKind::Codex,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            hook_event_name: hook_event_name.into(),
            tool_name: tool_name.map(String::from),
            tool_input: None,
            notification_type: None,
            model: None,
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
    fn session_end_falls_back_to_working_no_tool() {
        // Codex has no distinct SessionEnd status; the wrapper (csm-codex)
        // is what marks a Codex session ended, not the hook itself.
        let event = make_event("SessionEnd", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn notification_permission_prompt_shape_is_never_emitted_by_codex() {
        // Codex never sets notification_type on its Notification hook, so
        // this always falls through to Waiting(Input) rather than
        // Waiting(Permission) - PermissionRequest, not Notification, is
        // Codex's permission-prompt event.
        let json = r#"{
            "session_id": "codex-session",
            "cwd": "/work/project",
            "hook_event_name": "Notification"
        }"#;

        let event = parse_hook_event(json).unwrap();
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
    fn session_start_hook_event_parses_from_json() {
        let json = r#"{
            "session_id": "abc",
            "cwd": "/tmp",
            "hook_event_name": "SessionStart",
            "model": "gpt-5.1-codex"
        }"#;
        let event = parse_hook_event(json).unwrap();
        assert_eq!(event.session_id, "abc");
        assert_eq!(event.hook_event_name, "SessionStart");
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn codex_parser_carries_model_metadata_when_present() {
        let json = r#"{
            "session_id": "codex-session",
            "cwd": "/work/project",
            "hook_event_name": "SessionStart",
            "model": "gpt-5.1-codex"
        }"#;

        let event = parse_hook_event(json).unwrap();

        assert_eq!(event.session_id, "codex-session");
        assert_eq!(event.cwd, "/work/project");
        assert_eq!(event.hook_event_name, "SessionStart");
        assert_eq!(event.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(
            derive_status(&event),
            Status::Working(WorkingStatus { tool: None })
        );
    }

    #[test]
    fn codex_working_and_tool_lifecycle_events_derive_expected_statuses() {
        let cases = [
            (
                "SessionStart",
                None,
                Status::Working(WorkingStatus { tool: None }),
            ),
            (
                "UserPromptSubmit",
                None,
                Status::Working(WorkingStatus { tool: None }),
            ),
            (
                "PreToolUse",
                Some("Bash"),
                Status::Working(WorkingStatus {
                    tool: Some("Bash".into()),
                }),
            ),
            (
                "PostToolUse",
                Some("Bash"),
                Status::Working(WorkingStatus { tool: None }),
            ),
            (
                "Stop",
                None,
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Input,
                    detail: None,
                }),
            ),
            (
                "SessionEnd",
                None,
                Status::Working(WorkingStatus { tool: None }),
            ),
        ];

        for (hook_event_name, tool_name, expected) in cases {
            let mut json = serde_json::json!({
                "session_id": "codex-session",
                "cwd": "/work/project",
                "hook_event_name": hook_event_name
            });
            if let Some(tool_name) = tool_name {
                json["tool_name"] = serde_json::Value::String(tool_name.into());
            }

            let event = parse_hook_event(&json.to_string()).unwrap();
            assert_eq!(derive_status(&event), expected, "{hook_event_name}");
        }
    }

    #[test]
    fn codex_permission_request_uses_description_as_waiting_detail() {
        let json = r#"{
            "session_id": "codex-session",
            "cwd": "/work/project",
            "hook_event_name": "PermissionRequest",
            "tool_input": {
                "description": "Allow Bash to run cargo test?"
            }
        }"#;

        let event = parse_hook_event(json).unwrap();
        let status = derive_status(&event);

        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Permission,
                detail: Some("Allow Bash to run cargo test?".into())
            })
        );
    }

    #[test]
    fn codex_permission_request_accepts_missing_detail() {
        let json = r#"{
            "session_id": "codex-session",
            "cwd": "/work/project",
            "hook_event_name": "PermissionRequest"
        }"#;

        let event = parse_hook_event(json).unwrap();
        let status = derive_status(&event);

        assert_eq!(
            status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Permission,
                detail: None
            })
        );
    }
}
