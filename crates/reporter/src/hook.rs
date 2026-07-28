use common::api::AgentKind;
use common::session::Status;
use serde::Deserialize;

#[derive(Debug)]
pub struct NormalizedHookEvent {
    pub agent_kind: AgentKind,
    pub session_id: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
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
            model: event.model,
        }
    }
}

pub fn parse_hook_event(input: &str) -> Result<NormalizedHookEvent, serde_json::Error> {
    serde_json::from_str::<CodexHookEvent>(input).map(Into::into)
}

/// Map a Codex hook event onto the same five-state vocabulary
/// `common::session::Status::from_registry` uses for the registry-polling
/// (Claude) path, so nothing about Codex's own reporting is lost by
/// standardizing on the registry's vocabulary (see PRO-214).
///
/// `Notification` no longer branches on a `notification_type` of
/// `"permission_prompt"`: Claude Code hooks are never parsed by this
/// reporter any more (see `CodexHookEvent` above), Codex's own permission
/// prompt is the dedicated `PermissionRequest` event below, not
/// `Notification`, and the previous `Permission`/`Input` distinction this
/// fed has no equivalent in the new model regardless - see
/// `common::session::Status`'s doc comment.
///
/// `Stop` maps to `Idle`, not `Waiting`: it means the turn has finished and
/// the session is sitting at the prompt with no specific thing it's
/// blocked on - exactly `Idle`'s definition - rather than `Waiting`, which
/// is reserved for a concrete block on the user (a permission prompt, or
/// the registry's own `waitingFor`).
pub fn derive_status(event: &HookEvent) -> Status {
    match (event.agent_kind, event.hook_event_name.as_str()) {
        (_, "SessionStart") | (_, "UserPromptSubmit") => Status::Busy { tool: None },
        (_, "PreToolUse") => Status::Busy {
            tool: event.tool_name.clone(),
        },
        (_, "PostToolUse") => Status::Busy { tool: None },
        (_, "Notification") => Status::Waiting { detail: None },
        (AgentKind::Codex, "PermissionRequest") => Status::Waiting {
            detail: tool_input_description(event),
        },
        (_, "Stop") => Status::Idle,
        // Codex has no distinct SessionEnd status; the wrapper (csm-codex)
        // is what marks a Codex session ended, not the hook itself. Any
        // other/unrecognized hook_event_name falls back here too.
        _ => Status::Busy {
            tool: event.tool_name.clone(),
        },
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
            model: None,
        }
    }

    #[test]
    fn session_start_derives_busy_no_tool() {
        let event = make_event("SessionStart", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Busy { tool: None });
    }

    #[test]
    fn other_hook_with_tool_derives_busy_with_tool() {
        let event = make_event("PreToolUse", Some("Bash"));
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Busy {
                tool: Some("Bash".into())
            }
        );
    }

    #[test]
    fn user_prompt_submit_derives_busy_no_tool() {
        let event = make_event("UserPromptSubmit", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Busy { tool: None });
    }

    #[test]
    fn pre_tool_use_with_tool_derives_busy_with_tool() {
        let event = make_event("PreToolUse", Some("Bash"));
        let status = derive_status(&event);
        assert_eq!(
            status,
            Status::Busy {
                tool: Some("Bash".into())
            }
        );
    }

    #[test]
    fn post_tool_use_clears_tool() {
        let event = make_event("PostToolUse", Some("Bash"));
        let status = derive_status(&event);
        assert_eq!(status, Status::Busy { tool: None });
    }

    #[test]
    fn notification_derives_waiting_with_no_detail() {
        let event = make_event("Notification", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Waiting { detail: None });
    }

    #[test]
    fn stop_derives_idle() {
        let event = make_event("Stop", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Idle);
    }

    #[test]
    fn session_end_falls_back_to_busy_no_tool() {
        // Codex has no distinct SessionEnd status; the wrapper (csm-codex)
        // is what marks a Codex session ended, not the hook itself.
        let event = make_event("SessionEnd", None);
        let status = derive_status(&event);
        assert_eq!(status, Status::Busy { tool: None });
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
        assert_eq!(status, Status::Busy { tool: None });
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
        assert_eq!(derive_status(&event), Status::Busy { tool: None });
    }

    #[test]
    fn codex_working_and_tool_lifecycle_events_derive_expected_statuses() {
        let cases = [
            ("SessionStart", None, Status::Busy { tool: None }),
            ("UserPromptSubmit", None, Status::Busy { tool: None }),
            (
                "PreToolUse",
                Some("Bash"),
                Status::Busy {
                    tool: Some("Bash".into()),
                },
            ),
            ("PostToolUse", Some("Bash"), Status::Busy { tool: None }),
            ("Stop", None, Status::Idle),
            ("SessionEnd", None, Status::Busy { tool: None }),
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
            Status::Waiting {
                detail: Some("Allow Bash to run cargo test?".into())
            }
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

        assert_eq!(status, Status::Waiting { detail: None });
    }
}
