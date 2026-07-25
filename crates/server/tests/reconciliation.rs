//! Integration tests for the snapshot endpoint,
//! `POST /api/hosts/{hostname}/sessions`.
//!
//! These tests start a real server (in-memory SQLite, random port) and
//! assert the resulting state via `SseClient` - the same interface the GUI
//! uses - following the structure of `pipeline.rs` exactly.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::api::SessionView;
use common::session::Status;
use common::sse::{SseClient, SseUpdateHandler};
use test_support::{start_test_server, wait_for};

// --- Helpers ---

fn working_status() -> serde_json::Value {
    serde_json::json!({ "type": "working", "tool": null })
}

fn waiting_status() -> serde_json::Value {
    serde_json::json!({ "type": "waiting", "reason": "input", "detail": null })
}

fn snapshot_session(session_id: &str, cwd: &str, status: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "cwd": cwd,
        "status": status,
        "name": null,
        "git_branch": null,
        "git_remote": null,
        "tmux_target": null,
        "model": null
    })
}

fn snapshot_session_with_enrichment(
    session_id: &str,
    cwd: &str,
    status: serde_json::Value,
    git_branch: Option<&str>,
    tmux_target: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "cwd": cwd,
        "status": status,
        "name": null,
        "git_branch": git_branch,
        "git_remote": null,
        "tmux_target": tmux_target,
        "model": null
    })
}

/// Counts SSE broadcasts actually delivered to a client, via
/// `SseClient::set_handler`. `on_update` fires once per SSE message received
/// (`connected` is `true` for those) and once when the connection drops
/// (`connected` is `false`, which we don't count), so this counts real
/// server broadcasts - including the initial snapshot sent on subscribe -
/// rather than inferring "no update" from an unrelated field being
/// unchanged. That inference is unsound: a broadcast-always implementation
/// would still resend byte-identical `SessionView` rows, which the
/// assertions in these tests would not otherwise notice.
#[derive(Default)]
struct UpdateCounter(AtomicUsize);

impl UpdateCounter {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl SseUpdateHandler for UpdateCounter {
    fn on_update(&self, _sessions: Vec<SessionView>, connected: bool) {
        if connected {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

async fn post_snapshot(
    base_url: &str,
    hostname: &str,
    agent_kind: &str,
    sessions: Vec<serde_json::Value>,
) {
    let resp = reqwest::Client::new()
        .post(format!("{base_url}/api/hosts/{hostname}/sessions"))
        .json(&serde_json::json!({
            "agent_kind": agent_kind,
            "observed_at": chrono::Utc::now().to_rfc3339(),
            "sessions": sessions
        }))
        .send()
        .await
        .expect("POST snapshot");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
}

/// Seed a session via the legacy `POST /api/sessions` (`ReportPayload`)
/// endpoint, used here only to set up sessions the snapshot endpoint must
/// never touch: a null hostname, or a different agent kind on the same
/// host.
async fn post_report(base_url: &str, session_id: &str, hostname: Option<&str>, agent_kind: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base_url}/api/sessions"))
        .json(&serde_json::json!({
            "session_id": session_id,
            "cwd": "/tmp",
            "status": { "type": "working", "tool": null },
            "agent_kind": agent_kind,
            "model": null,
            "hook_event_name": "SessionStart",
            "tool_name": null,
            "tool_input": null,
            "notification_type": null,
            "hostname": hostname,
            "git_branch": null,
            "git_remote": null,
            "tmux_target": null
        }))
        .send()
        .await
        .expect("POST /api/sessions");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
}

const TIMEOUT: Duration = Duration::from_secs(5);
/// Long enough that a broadcast the implementation should not send would
/// have arrived and been observed via `SseClient::sessions()`.
const SETTLE: Duration = Duration::from_millis(250);

// --- Tests ---

#[tokio::test]
async fn snapshot_creates_and_updates_sessions() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    post_snapshot(
        &base_url,
        "host-c",
        "claude",
        vec![
            snapshot_session("c1", "/tmp/c1", working_status()),
            snapshot_session("c2", "/tmp/c2", waiting_status()),
        ],
    )
    .await;

    wait_for(&sse, TIMEOUT, |sessions| {
        let has_c1 = sessions.iter().any(|s| s.session_id == "c1");
        let has_c2 = sessions.iter().any(|s| s.session_id == "c2");
        (has_c1 && has_c2).then_some(())
    })
    .await;

    // A later snapshot updates c1's cwd and status.
    post_snapshot(
        &base_url,
        "host-c",
        "claude",
        vec![
            snapshot_session("c1", "/tmp/c1-moved", waiting_status()),
            snapshot_session("c2", "/tmp/c2", waiting_status()),
        ],
    )
    .await;

    let c1_updated = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "c1" && s.cwd == "/tmp/c1-moved")
            .cloned()
    })
    .await;
    assert!(matches!(c1_updated.status, Status::Waiting(_)));

    handle.abort();
}

#[tokio::test]
async fn session_absent_from_snapshot_is_ended() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    post_snapshot(
        &base_url,
        "host-d",
        "claude",
        vec![
            snapshot_session("d1", "/tmp/d1", working_status()),
            snapshot_session("d2", "/tmp/d2", working_status()),
        ],
    )
    .await;
    wait_for(&sse, TIMEOUT, |sessions| {
        let has_both = sessions.iter().any(|s| s.session_id == "d1")
            && sessions.iter().any(|s| s.session_id == "d2");
        has_both.then_some(())
    })
    .await;

    // d2 is absent from this later snapshot.
    post_snapshot(
        &base_url,
        "host-d",
        "claude",
        vec![snapshot_session("d1", "/tmp/d1", working_status())],
    )
    .await;

    wait_for(&sse, TIMEOUT, |sessions| {
        let d2_gone = sessions.iter().all(|s| s.session_id != "d2");
        let d1_present = sessions.iter().any(|s| s.session_id == "d1");
        (d2_gone && d1_present).then_some(())
    })
    .await;

    handle.abort();
}

#[tokio::test]
async fn snapshot_never_touches_other_host_null_hostname_or_other_agent_kind() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    // A session on a different host.
    post_snapshot(
        &base_url,
        "host-other",
        "claude",
        vec![snapshot_session(
            "other-host-1",
            "/tmp/other-host",
            working_status(),
        )],
    )
    .await;

    // A session with a null hostname (as a legacy pre-hostname reporter
    // would publish, or a host whose hostname resolution failed).
    post_report(&base_url, "null-host-1", None, "claude").await;

    // A Codex session on the SAME host we're about to snapshot.
    post_report(&base_url, "codex-1", Some("host-a"), "codex").await;

    let baseline = wait_for(&sse, TIMEOUT, |sessions| {
        let other_host = sessions
            .iter()
            .find(|s| s.session_id == "other-host-1")
            .cloned();
        let null_host = sessions
            .iter()
            .find(|s| s.session_id == "null-host-1")
            .cloned();
        let codex = sessions.iter().find(|s| s.session_id == "codex-1").cloned();
        match (other_host, null_host, codex) {
            (Some(a), Some(b), Some(c)) => Some((a, b, c)),
            _ => None,
        }
    })
    .await;
    let (other_host_before, null_host_before, codex_before) = baseline;

    // A Claude snapshot for host-a containing an unrelated session, and not
    // containing any of the three sessions above.
    post_snapshot(
        &base_url,
        "host-a",
        "claude",
        vec![snapshot_session(
            "host-a-1",
            "/tmp/host-a",
            working_status(),
        )],
    )
    .await;
    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .any(|s| s.session_id == "host-a-1")
            .then_some(())
    })
    .await;
    tokio::time::sleep(SETTLE).await;

    let after = sse.sessions();
    let other_host_after = after
        .iter()
        .find(|s| s.session_id == "other-host-1")
        .cloned()
        .expect("other-host-1 must still be present");
    let null_host_after = after
        .iter()
        .find(|s| s.session_id == "null-host-1")
        .cloned()
        .expect("null-host-1 must still be present");
    let codex_after = after
        .iter()
        .find(|s| s.session_id == "codex-1")
        .cloned()
        .expect("codex-1 must still be present");

    assert_eq!(other_host_after.updated_at, other_host_before.updated_at);
    assert_eq!(null_host_after.updated_at, null_host_before.updated_at);
    assert_eq!(codex_after.updated_at, codex_before.updated_at);
    assert_eq!(other_host_after.status, other_host_before.status);
    assert_eq!(null_host_after.status, null_host_before.status);
    assert_eq!(codex_after.status, codex_before.status);

    handle.abort();
}

#[tokio::test]
async fn unchanged_snapshot_produces_no_sse_update() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    let counter = UpdateCounter::new();
    sse.set_handler(counter.clone());
    sse.start();

    let sessions = vec![snapshot_session("s1", "/tmp/project", working_status())];
    post_snapshot(&base_url, "host-e", "claude", sessions.clone()).await;

    wait_for(&sse, TIMEOUT, |sessions| {
        sessions.iter().find(|s| s.session_id == "s1").cloned()
    })
    .await;

    // Let the counter settle after the creating snapshot (and the initial
    // subscribe snapshot) before taking a baseline reading.
    tokio::time::sleep(SETTLE).await;
    let baseline = counter.count();

    // Republish the identical snapshot. This must not deliver any further
    // SSE message at all - not even one carrying byte-identical data.
    post_snapshot(&base_url, "host-e", "claude", sessions).await;
    tokio::time::sleep(SETTLE).await;

    assert_eq!(
        counter.count(),
        baseline,
        "an unchanged republish must not broadcast an SSE update"
    );

    // Sanity check: a genuinely different snapshot DOES broadcast, proving
    // the counter would have caught a broadcast-always implementation.
    post_snapshot(
        &base_url,
        "host-e",
        "claude",
        vec![snapshot_session("s1", "/tmp/other", working_status())],
    )
    .await;
    wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "s1" && s.cwd == "/tmp/other")
            .cloned()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    assert!(
        counter.count() > baseline,
        "a genuinely changed snapshot must broadcast"
    );

    handle.abort();
}

#[tokio::test]
async fn already_ended_session_left_alone_on_repeat_absence() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    let counter = UpdateCounter::new();
    sse.set_handler(counter.clone());
    sse.start();

    post_snapshot(
        &base_url,
        "host-f",
        "claude",
        vec![
            snapshot_session("f1", "/tmp/f1", working_status()),
            snapshot_session("f2", "/tmp/f2", working_status()),
        ],
    )
    .await;
    wait_for(&sse, TIMEOUT, |sessions| {
        let has_both = sessions.iter().any(|s| s.session_id == "f1")
            && sessions.iter().any(|s| s.session_id == "f2");
        has_both.then_some(())
    })
    .await;

    // f2 is absent: it becomes ended.
    let only_f1 = vec![snapshot_session("f1", "/tmp/f1", working_status())];
    post_snapshot(&base_url, "host-f", "claude", only_f1.clone()).await;
    wait_for(&sse, TIMEOUT, |sessions| {
        let f2_gone = sessions.iter().all(|s| s.session_id != "f2");
        let f1_present = sessions.iter().any(|s| s.session_id == "f1");
        (f2_gone && f1_present).then_some(())
    })
    .await;

    // Once ended, f2 is invisible to SseClient (it's excluded from
    // `list_active_sessions`), so a spurious re-end of it would go
    // undetected by any assertion on session content. Only a broadcast
    // counter observes it.
    tokio::time::sleep(SETTLE).await;
    let baseline = counter.count();

    // Republish with f2 still absent (and already ended). This must not
    // churn anything: no broadcast at all, f1 unaffected, f2 stays gone.
    post_snapshot(&base_url, "host-f", "claude", only_f1).await;
    tokio::time::sleep(SETTLE).await;

    assert_eq!(
        counter.count(),
        baseline,
        "an already-ended session absent again must not cause a broadcast"
    );

    let after_repeat = sse.sessions();
    assert!(after_repeat.iter().any(|s| s.session_id == "f1"));
    assert!(after_repeat.iter().all(|s| s.session_id != "f2"));

    handle.abort();
}

#[tokio::test]
async fn enrichment_fields_survive_snapshot_and_participate_in_change_detection() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    let counter = UpdateCounter::new();
    sse.set_handler(counter.clone());
    sse.start();

    post_snapshot(
        &base_url,
        "host-g",
        "claude",
        vec![snapshot_session_with_enrichment(
            "g1",
            "/tmp/g1",
            working_status(),
            Some("feature/foo"),
            Some("main:0.1"),
        )],
    )
    .await;

    let g1 = wait_for(&sse, TIMEOUT, |sessions| {
        sessions.iter().find(|s| s.session_id == "g1").cloned()
    })
    .await;
    assert_eq!(g1.git_branch.as_deref(), Some("feature/foo"));
    assert_eq!(g1.tmux_target.as_deref(), Some("main:0.1"));

    tokio::time::sleep(SETTLE).await;
    let baseline = counter.count();

    // Republish with only git_branch changed; tmux_target stays the same.
    post_snapshot(
        &base_url,
        "host-g",
        "claude",
        vec![snapshot_session_with_enrichment(
            "g1",
            "/tmp/g1",
            working_status(),
            Some("feature/bar"),
            Some("main:0.1"),
        )],
    )
    .await;

    let g1_after = wait_for(&sse, TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "g1" && s.git_branch.as_deref() == Some("feature/bar"))
            .cloned()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    assert!(
        counter.count() > baseline,
        "a change to git_branch alone must broadcast"
    );
    assert_eq!(g1_after.tmux_target.as_deref(), Some("main:0.1"));

    handle.abort();
}
