use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct HookEvent {
    pub session_id: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub notification_type: Option<String>,
    // Fields present in some hooks but not used for status derivation
    #[serde(flatten)]
    pub _extra: std::collections::HashMap<String, serde_json::Value>,
}

pub fn derive_status(event: &HookEvent) -> Status {
    match event.hook_event_name.as_str() {
        "SessionStart" | "UserPromptSubmit" => Status::Working(WorkingStatus { tool: None }),
        "PreToolUse" => Status::Working(WorkingStatus {
            tool: event.tool_name.clone(),
        }),
        "Notification" => {
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
        "Stop" => Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: None,
        }),
        "SessionEnd" => Status::Ended,
        _ => Status::Working(WorkingStatus {
            tool: event.tool_name.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(hook_event_name: &str, tool_name: Option<&str>) -> HookEvent {
        HookEvent {
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            hook_event_name: hook_event_name.into(),
            tool_name: tool_name.map(String::from),
            tool_input: None,
            notification_type: None,
            _extra: Default::default(),
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
    fn session_start_hook_event_parses_from_json() {
        let json = r#"{
            "session_id": "abc",
            "cwd": "/tmp",
            "hook_event_name": "SessionStart",
            "permission_mode": "default",
            "transcript_path": "/tmp/t"
        }"#;
        let event: HookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.session_id, "abc");
        assert_eq!(event.hook_event_name, "SessionStart");
        let status = derive_status(&event);
        assert_eq!(status, Status::Working(WorkingStatus { tool: None }));
    }
}
