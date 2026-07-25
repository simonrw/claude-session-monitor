//! Integration tests for the reporter → server → SSE pipeline.
//!
//! These tests start a real server (in-memory SQLite, random port), spawn the
//! reporter binary to POST hook events, and assert the resulting state via
//! SseClient — the same interface the GUI uses.
//!
//! Reduced to the Codex path (PRO-213): Claude Code sessions are no longer
//! tracked by `csm-reporter` at all - see `crates/server/tests/reconciliation.rs`
//! for the watcher's registry-polling path, and the reporter-rejection tests
//! below for the guardrail that keeps a stale Claude Code hook from silently
//! double-reporting alongside `csm-watcher`.
//!
//! The reporter binary must be built before running these tests.
//! `cargo test --workspace` handles this automatically; otherwise run
//! `cargo build --workspace` first.

use std::time::Duration;

use common::api::AgentKind;
use common::session::Status;
use common::sse::SseClient;
use test_support::{locate_bin, sandbox_home, start_test_server, wait_for};

// --- Helpers ---

/// `csm-reporter` derives its log directory from `$HOME` (see
/// `crates/reporter/src/main.rs`'s `setup_tracing`), so every spawn here
/// gets a fresh `sandbox_home()` for `HOME` - otherwise every call would
/// append into the developer's real
/// `~/.local/share/claude-session-monitor/` (PRO-218). Dropped once this
/// function returns, which is fine: the child has already exited by then.
async fn run_reporter_with_args(base_url: &str, args: &[&str], hook_event_json: &str) {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let home = sandbox_home();
    let mut child = Command::new(locate_bin("csm-reporter"))
        .args(args)
        .env("CLAUDE_MONITOR_URL", base_url)
        .env("HOME", home.path())
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

/// Spawn the reporter with no server configured and capture its exit status
/// and stderr, for asserting the `--agent claude` rejection path never
/// reaches the network.
async fn run_reporter_expect_rejection(args: &[&str]) -> std::process::Output {
    use tokio::process::Command;

    // See `run_reporter_with_args`'s doc comment: sandboxes the log
    // directory away from the developer's real one.
    let home = sandbox_home();
    let child = Command::new(locate_bin("csm-reporter"))
        .args(args)
        .env("HOME", home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn reporter");

    child.wait_with_output().await.expect("wait reporter")
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
async fn codex_busy_lifecycle_appears_via_sse() {
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
    })
    .await;
    assert_eq!(s.agent_kind, AgentKind::Codex);
    assert_eq!(s.model.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(s.status, Status::Busy { tool: None });

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
                    && matches!(&s.status, Status::Busy { tool } if tool.as_deref() == Some("Bash"))
            })
            .cloned()
    })
    .await;
    assert_eq!(
        s.status,
        Status::Busy {
            tool: Some("Bash".into())
        }
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
                    && matches!(&s.status, Status::Busy { tool } if tool.is_none())
            })
            .cloned()
    })
    .await;
    assert_eq!(s.status, Status::Busy { tool: None });

    // `Stop` maps to `Idle` (turn finished, sitting at the prompt), not
    // `Waiting` - see `csm-reporter`'s `hook::derive_status` doc comment.
    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("codex-1", "Stop"),
    )
    .await;
    let s = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "codex-1" && matches!(&s.status, Status::Idle))
            .cloned()
    })
    .await;
    assert_eq!(s.status, Status::Idle);

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
            .find(|s| {
                s.session_id == "codex-permission" && matches!(&s.status, Status::Waiting { .. })
            })
            .cloned()
    })
    .await;
    assert_eq!(s.agent_kind, AgentKind::Codex);
    assert_eq!(
        s.status,
        Status::Waiting {
            detail: Some("Allow Bash to run cargo test?".into()),
        }
    );

    handle.abort();
}

#[tokio::test]
async fn multiple_sessions_tracked_independently() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("sess-a", "SessionStart"),
    )
    .await;
    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("sess-b", "SessionStart"),
    )
    .await;

    wait_for(&sse, TIMEOUT, |sessions| {
        let has_a = sessions.iter().any(|s| s.session_id == "sess-a");
        let has_b = sessions.iter().any(|s| s.session_id == "sess-b");
        (has_a && has_b).then_some(())
    })
    .await;

    // End sess-a via the same endpoint csm-codex uses; sess-b must survive.
    let resp = reqwest::Client::new()
        .post(format!("{base_url}/api/sessions/sess-a/end"))
        .send()
        .await
        .expect("POST /api/sessions/sess-a/end");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    wait_for(&sse, TIMEOUT, |sessions| {
        let a_gone = sessions.iter().all(|s| s.session_id != "sess-a");
        let b_alive = sessions.iter().any(|s| s.session_id == "sess-b");
        (a_gone && b_alive).then_some(())
    })
    .await;

    handle.abort();
}

#[tokio::test]
async fn delete_session_removes_from_sse() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    run_reporter_with_args(
        &base_url,
        &["--agent", "codex"],
        &codex_hook_event("sess-del", "SessionStart"),
    )
    .await;
    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "sess-del")
            .map(|_| ())
    })
    .await;

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
    })
    .await;

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
    })
    .await;

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
    })
    .await;

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

// --- Claude rejection (PRO-213: split-brain must be impossible) ---

#[tokio::test]
async fn reporter_rejects_explicit_claude_agent_naming_the_watcher() {
    // Never touches a server: the rejection must happen before any network
    // call, config load, or stdin read, so a stale Claude Code hook fails
    // fast and loud instead of racing csm-watcher.
    let output = run_reporter_expect_rejection(&["--agent", "claude"]).await;

    assert!(
        !output.status.success(),
        "reporter should exit non-zero for --agent claude, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("csm-watcher"),
        "rejection message should name csm-watcher, got: {stderr}"
    );
}

#[tokio::test]
async fn reporter_rejects_bare_invocation_with_no_agent_flag() {
    // A stale Claude Code hook never passes --agent at all - it was
    // registered back when the reporter defaulted to Claude. The default
    // must still resolve to the rejected Claude path, not silently succeed
    // or silently mis-parse as Codex.
    let output = run_reporter_expect_rejection(&[]).await;

    assert!(
        !output.status.success(),
        "bare invocation (no --agent) should exit non-zero, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("csm-watcher"),
        "rejection message should name csm-watcher, got: {stderr}"
    );
}
