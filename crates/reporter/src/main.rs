mod hook;

use common::api::{ReportPayload, resolve_server_url};

fn main() {
    let input = match read_stdin() {
        Ok(s) => s,
        Err(_) => return,
    };

    let event: hook::HookEvent = match serde_json::from_str(&input) {
        Ok(e) => e,
        Err(_) => return,
    };

    let status = hook::derive_status(&event);

    let payload = ReportPayload {
        session_id: event.session_id,
        cwd: event.cwd,
        status,
        hook_event_name: event.hook_event_name,
        tool_name: event.tool_name,
        tool_input: event.tool_input,
        notification_type: event.notification_type,
    };

    let url = format!("{}/api/sessions", resolve_server_url(None));
    let _ = reqwest::blocking::Client::new()
        .post(&url)
        .json(&payload)
        .send();
}

fn read_stdin() -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
