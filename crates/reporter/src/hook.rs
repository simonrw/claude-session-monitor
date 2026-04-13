use common::session::{Status, WorkingStatus};
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
        "SessionStart" => Status::Working(WorkingStatus { tool: None }),
        _ => Status::Working(WorkingStatus { tool: event.tool_name.clone() }),
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
        assert_eq!(status, Status::Working(WorkingStatus { tool: Some("Bash".into()) }));
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
