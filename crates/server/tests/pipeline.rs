//! Integration tests for the reporter → server → SSE pipeline.
//!
//! These tests start a real server (in-memory SQLite, random port), spawn the
//! reporter binary to POST hook events, and assert the resulting state via
//! SseClient — the same interface the GUI uses.
//!
//! The reporter binary must be built before running these tests.
//! `cargo test --workspace` handles this automatically; otherwise run
//! `cargo build -p csm-reporter` first.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::api::{AgentKind, SessionView};
use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};
use common::sse::SseClient;
use tokio::task::JoinHandle;

// --- Helpers ---

fn reporter_bin() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .unwrap()
        .to_path_buf();
    // test binary is at <target_dir>/debug/deps/pipeline-<hash>
    // go up one level to reach <target_dir>/debug/
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("csm-reporter");
    assert!(
        path.exists(),
        "reporter binary not found at {path:?} -- run `cargo build -p csm-reporter` first"
    );
    path
}

async fn start_test_server() -> (String, JoinHandle<()>) {
    let conn = server::store::open_db(":memory:").expect("in-memory DB");
    let app = server::build_app(conn, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{port}");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (base_url, handle)
}

async fn run_reporter(base_url: &str, hook_event_json: &str) {
    run_reporter_with_args(base_url, &[], hook_event_json).await;
}

async fn run_reporter_with_args(base_url: &str, args: &[&str], hook_event_json: &str) {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let mut child = Command::new(reporter_bin())
        .args(args)
        .env("CLAUDE_MONITOR_URL", base_url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn reporter");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(hook_event_json.as_bytes())
        .await
        .expect("write stdin");

    let status = child.wait().await.expect("wait reporter");
    assert!(status.success(), "reporter exited with {status}");
}

fn hook_event(session_id: &str, hook_event_name: &str) -> String {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp",
        "hook_event_name": hook_event_name
    })
    .to_string()
}

fn hook_event_with_tool(session_id: &str, tool_name: &str) -> String {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name
    })
    .to_string()
}

fn codex_hook_event(session_id: &str, hook_event_name: &str) -> String {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp",
        "hook_event_name": hook_event_name,
        "model": "gpt-5.1-codex"
    })
    .to_string()
}

fn codex_hook_event_with_tool(session_id: &str, tool_name: &str) -> String {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "model": "gpt-5.1-codex"
    })
    .to_string()
}

fn codex_permission_request(session_id: &str, description: Option<&str>) -> String {
    let mut event = serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp",
        "hook_event_name": "PermissionRequest",
        "model": "gpt-5.1-codex"
    });
    if let Some(description) = description {
        event["tool_input"] = serde_json::json!({
            "description": description
        });
    }
    event.to_string()
}

fn hook_event_notification(session_id: &str, notification_type: &str) -> String {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp",
        "hook_event_name": "Notification",
        "notification_type": notification_type
    })
    .to_string()
}

/// Poll `SseClient::sessions()` every 50ms until `predicate` returns `Some(T)`,
/// or panic with a timeout message after `timeout`.
fn wait_for<F, T>(sse: &SseClient, timeout: Duration, mut predicate: F) -> T
where
    F: FnMut(&[SessionView]) -> Option<T>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let sessions = sse.sessions();
        if let Some(result) = predicate(&sessions) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "timeout after {timeout:?}; last sessions: {sessions:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

const TIMEOUT: Duration = Duration::from_secs(5);

// --- Tests ---

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let (base_url, handle) = start_test_server().await;

    let resp = reqwest::get(format!("{base_url}/api/health"))
        .await
        .expect("GET /api/health");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["status"], "ok");

    handle.abort();
}

#[tokio::test]
async fn session_start_appears_via_sse() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter(&base_url, &hook_event("sess-1", "SessionStart")).await;

    let session = wait_for(&sse, TIMEOUT, |sessions| {
        sessions.iter().find(|s| s.session_id == "sess-1").cloned()
    });
    assert_eq!(
        session.status,
        Status::Working(WorkingStatus { tool: None })
    );

    handle.abort();
}

#[tokio::test]
async fn status_transitions_working_to_waiting_to_ended() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    // SessionStart → Working
    run_reporter(&base_url, &hook_event("sess-2", "SessionStart")).await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions.iter().find(|s| s.session_id == "sess-2").cloned()
    });
    assert!(matches!(s.status, Status::Working(_)));

    // PreToolUse(Bash) → Working(tool: Bash)
    run_reporter(&base_url, &hook_event_with_tool("sess-2", "Bash")).await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| {
                s.session_id == "sess-2"
                    && matches!(&s.status, Status::Working(w) if w.tool.as_deref() == Some("Bash"))
            })
            .cloned()
    });
    assert_eq!(
        s.status,
        Status::Working(WorkingStatus {
            tool: Some("Bash".into())
        })
    );

    // Notification(permission_prompt) → Waiting(Permission)
    run_reporter(
        &base_url,
        &hook_event_notification("sess-2", "permission_prompt"),
    )
    .await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "sess-2" && matches!(&s.status, Status::Waiting(_)))
            .cloned()
    });
    assert_eq!(
        s.status,
        Status::Waiting(WaitingStatus {
            reason: WaitingReason::Permission,
            detail: None,
        })
    );

    // SessionEnd → session removed from active list
    run_reporter(&base_url, &hook_event("sess-2", "SessionEnd")).await;
    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .all(|s| s.session_id != "sess-2")
            .then_some(())
    });

    handle.abort();
}

#[tokio::test]
async fn codex_working_lifecycle_appears_via_sse() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("codex-1", "SessionStart"),
    )
    .await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions.iter().find(|s| s.session_id == "codex-1").cloned()
    });
    assert_eq!(s.agent_kind, AgentKind::Codex);
    assert_eq!(s.model.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(s.status, Status::Working(WorkingStatus { tool: None }));

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event_with_tool("codex-1", "Bash"),
    )
    .await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| {
                s.session_id == "codex-1"
                    && matches!(&s.status, Status::Working(w) if w.tool.as_deref() == Some("Bash"))
            })
            .cloned()
    });
    assert_eq!(
        s.status,
        Status::Working(WorkingStatus {
            tool: Some("Bash".into())
        })
    );

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("codex-1", "PostToolUse"),
    )
    .await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| {
                s.session_id == "codex-1"
                    && matches!(&s.status, Status::Working(w) if w.tool.is_none())
            })
            .cloned()
    });
    assert_eq!(s.status, Status::Working(WorkingStatus { tool: None }));

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("codex-1", "Stop"),
    )
    .await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "codex-1" && matches!(&s.status, Status::Waiting(_)))
            .cloned()
    });
    assert_eq!(
        s.status,
        Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: None,
        })
    );

    handle.abort();
}

#[tokio::test]
async fn codex_permission_request_appears_via_sse() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_permission_request("codex-permission", Some("Allow Bash to run cargo test?")),
    )
    .await;

    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "codex-permission" && matches!(&s.status, Status::Waiting(_)))
            .cloned()
    });
    assert_eq!(s.agent_kind, AgentKind::Codex);
    assert_eq!(
        s.status,
        Status::Waiting(WaitingStatus {
            reason: WaitingReason::Permission,
            detail: Some("Allow Bash to run cargo test?".into()),
        })
    );

    handle.abort();
}

#[tokio::test]
async fn multiple_sessions_tracked_independently() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter(&base_url, &hook_event("sess-a", "SessionStart")).await;
    run_reporter(&base_url, &hook_event("sess-b", "SessionStart")).await;

    wait_for(&sse, TIMEOUT, |sessions| {
        let has_a = sessions.iter().any(|s| s.session_id == "sess-a");
        let has_b = sessions.iter().any(|s| s.session_id == "sess-b");
        (has_a && has_b).then_some(())
    });

    // End sess-a; sess-b must survive
    run_reporter(&base_url, &hook_event("sess-a", "SessionEnd")).await;
    wait_for(&sse, TIMEOUT, |sessions| {
        let a_gone = sessions.iter().all(|s| s.session_id != "sess-a");
        let b_alive = sessions.iter().any(|s| s.session_id == "sess-b");
        (a_gone && b_alive).then_some(())
    });

    handle.abort();
}

#[tokio::test]
async fn delete_session_removes_from_sse() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter(&base_url, &hook_event("sess-del", "SessionStart")).await;
    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "sess-del")
            .map(|_| ())
    });

    // DELETE via HTTP — same as what the GUI does
    let resp = reqwest::Client::new()
        .delete(format!("{base_url}/api/sessions/sess-del"))
        .send()
        .await
        .expect("DELETE request");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .all(|s| s.session_id != "sess-del")
            .then_some(())
    });

    handle.abort();
}

#[tokio::test]
async fn end_session_removes_from_sse() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("codex-endpoint", "SessionStart"),
    )
    .await;
    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "codex-endpoint")
            .map(|_| ())
    });

    let resp = reqwest::Client::new()
        .post(format!("{base_url}/api/sessions/codex-endpoint/end"))
        .send()
        .await
        .expect("POST /api/sessions/codex-endpoint/end");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .all(|s| s.session_id != "codex-endpoint")
            .then_some(())
    });

    handle.abort();
}

#[tokio::test]
async fn delete_nonexistent_returns_404() {
    let (base_url, handle) = start_test_server().await;

    let resp = reqwest::Client::new()
        .delete(format!("{base_url}/api/sessions/nonexistent"))
        .send()
        .await
        .expect("DELETE request");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    handle.abort();
}

#[tokio::test]
async fn end_nonexistent_returns_404() {
    let (base_url, handle) = start_test_server().await;

    let resp = reqwest::Client::new()
        .post(format!("{base_url}/api/sessions/nonexistent/end"))
        .send()
        .await
        .expect("POST /api/sessions/nonexistent/end");

    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    handle.abort();
}
