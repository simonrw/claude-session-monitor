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
use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};
use common::sse::{SseClient, SseUpdateHandler};
use test_support::{locate_bin, start_test_server, wait_for};

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

// --- Watcher-driven integration tests (PRO-207) ---
//
// These drive the real `csm-watcher --once` binary against a tempdir
// registry (via `CSM_WATCHER_REGISTRY_DIRS`) and a real test server,
// asserting through `SseClient` exactly like the snapshot-endpoint tests
// above assert the endpoint itself. Registry entries are never asserted on
// directly - the registry's file format belongs to Claude Code, not this
// project, and will change.
//
// On `name`: PRO-207's acceptance criteria call for a registry entry to
// carry its name as set by `/rename`. `write_registry_entry` below does
// write a real `name` into the fixture (see `watcher_publishes_a_live_interactive_session`),
// so it is genuinely parsed by `registry::RegistryEntry` and carried into
// `SnapshotSession` and the publish payload the watcher sends - but the
// chain stops there: `SessionView`, the server's stored/broadcast shape
// that `SseClient` (and these tests) observe, has no `name` field yet, so
// nothing here can assert on it past the wire. No ticket under PRO-204
// currently wires `name` through to `SessionView`/clients; until one does,
// this field is exercised but unverified end to end.

use std::path::Path;

/// Format a pid's OS-recorded start time the way Claude Code formats
/// `procStart` in the registry: a ctime-style string in UTC (e.g.
/// `"Fri Jul 24 20:55:59 2026"`). Reuses `common::process::start_time` (the
/// production pid -> epoch lookup the watcher itself relies on) rather than
/// re-deriving it, so this fixture can only drift from reality in step with
/// the watcher's own liveness check.
fn registry_proc_start_for(pid: u32) -> String {
    let epoch = common::process::start_time(pid as i32)
        .expect("failed to read OS start time for pid; is it still alive?");
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .expect("valid unix timestamp")
        .format("%a %b %e %H:%M:%S %Y")
        .to_string()
}

/// Write one registry entry file at `<registry_root>/sessions/<file_name>`.
///
/// `name` mirrors the registry's own `name` field, as set by `/rename`.
/// Most call sites pass `None`, since the field is incidental to what
/// they're testing; `watcher_publishes_a_live_interactive_session` passes a
/// real value so `name` is exercised end to end through parsing and the
/// publish payload - see the note on `SessionView` above about why it can't
/// (yet) be asserted on past that point.
#[allow(clippy::too_many_arguments)]
fn write_registry_entry(
    registry_root: &Path,
    file_name: &str,
    session_id: &str,
    pid: u32,
    proc_start: &str,
    kind: &str,
    status: &str,
    waiting_for: Option<&str>,
    cwd: &str,
    name: Option<&str>,
) {
    let sessions_dir = registry_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let body = serde_json::json!({
        "sessionId": session_id,
        "pid": pid,
        "procStart": proc_start,
        "kind": kind,
        "status": status,
        "waitingFor": waiting_for,
        "cwd": cwd,
        "name": name
    });
    std::fs::write(sessions_dir.join(file_name), body.to_string())
        .expect("write registry fixture file");
}

/// Run the real `csm-watcher --once` binary against `registry_dirs`,
/// pointed at the test server. Asserts the process exits successfully -
/// itself part of the "missing directory"/"malformed file" acceptance
/// criteria, since a sweep must never fail the whole process over a single
/// bad entry or an absent directory.
async fn run_watcher_once(base_url: &str, registry_dirs: &[&Path]) {
    use tokio::process::Command;

    let joined = std::env::join_paths(registry_dirs.iter().map(|p| p.as_os_str()))
        .expect("join registry dirs");
    let status = Command::new(locate_bin("csm-watcher"))
        .arg("--once")
        .env("CLAUDE_MONITOR_URL", base_url)
        .env("CSM_WATCHER_REGISTRY_DIRS", joined)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .expect("failed to spawn csm-watcher");
    assert!(status.success(), "csm-watcher exited with {status}");
}

const WATCHER_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn watcher_publishes_a_live_interactive_session() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "entry.json",
        "watcher-live-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/watcher-live-1",
        Some("captain-marvel"),
    );

    run_watcher_once(&base_url, &[registry.path()]).await;

    let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-live-1")
            .cloned()
    })
    .await;
    assert_eq!(session.cwd, "/tmp/watcher-live-1");
    assert_eq!(
        session.status,
        Status::Working(WorkingStatus { tool: None })
    );

    handle.abort();
}

#[tokio::test]
async fn watcher_never_publishes_non_interactive_kinds() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);

    write_registry_entry(
        registry.path(),
        "anchor.json",
        "watcher-anchor-b",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/anchor",
        None,
    );
    for (file, id, kind) in [
        ("bg.json", "watcher-bg-b", "bg"),
        ("daemon.json", "watcher-daemon-b", "daemon"),
        ("worker.json", "watcher-worker-b", "daemon-worker"),
    ] {
        write_registry_entry(
            registry.path(),
            file,
            id,
            pid,
            &proc_start,
            kind,
            "busy",
            None,
            "/tmp/non-interactive",
            None,
        );
    }

    run_watcher_once(&base_url, &[registry.path()]).await;

    wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .any(|s| s.session_id == "watcher-anchor-b")
            .then_some(())
    })
    .await;

    let sessions = sse.sessions();
    assert!(sessions.iter().all(|s| s.session_id != "watcher-bg-b"));
    assert!(sessions.iter().all(|s| s.session_id != "watcher-daemon-b"));
    assert!(sessions.iter().all(|s| s.session_id != "watcher-worker-b"));

    handle.abort();
}

#[tokio::test]
async fn watcher_never_publishes_a_dead_pid() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let live_pid = std::process::id();
    let live_proc_start = registry_proc_start_for(live_pid);
    write_registry_entry(
        registry.path(),
        "anchor.json",
        "watcher-anchor-c",
        live_pid,
        &live_proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/anchor",
        None,
    );

    // A genuinely dead pid: spawn a trivial child and wait for it to exit.
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn `true`");
    let dead_pid = child.id();
    child.wait().expect("wait for child");
    write_registry_entry(
        registry.path(),
        "dead.json",
        "watcher-dead-c",
        dead_pid,
        "Mon Jan 1 00:00:00 2020",
        "interactive",
        "busy",
        None,
        "/tmp/dead",
        None,
    );

    run_watcher_once(&base_url, &[registry.path()]).await;

    wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .any(|s| s.session_id == "watcher-anchor-c")
            .then_some(())
    })
    .await;

    let sessions = sse.sessions();
    assert!(sessions.iter().all(|s| s.session_id != "watcher-dead-c"));

    handle.abort();
}

#[tokio::test]
async fn watcher_never_publishes_a_reused_pid_with_mismatched_proc_start() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let real_proc_start = registry_proc_start_for(pid);

    write_registry_entry(
        registry.path(),
        "anchor.json",
        "watcher-anchor-d",
        pid,
        &real_proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/anchor",
        None,
    );
    // Same (live) pid, but a fabricated procStart nowhere near reality: the
    // pid-reuse defense must reject this as if the pid had been recycled.
    write_registry_entry(
        registry.path(),
        "reused.json",
        "watcher-reused-d",
        pid,
        "Mon Jan 1 00:00:00 2020",
        "interactive",
        "busy",
        None,
        "/tmp/reused",
        None,
    );

    run_watcher_once(&base_url, &[registry.path()]).await;

    wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .any(|s| s.session_id == "watcher-anchor-d")
            .then_some(())
    })
    .await;

    let sessions = sse.sessions();
    assert!(sessions.iter().all(|s| s.session_id != "watcher-reused-d"));

    handle.abort();
}

#[tokio::test]
async fn watcher_maps_registry_statuses_to_expected_session_states() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);

    let cases: [(&str, Option<&str>); 4] = [
        ("busy", None),
        ("shell", None),
        ("idle", Some("thinking")),
        ("waiting", Some("Allow Bash to run cargo test?")),
    ];
    for (idx, (status, waiting_for)) in cases.into_iter().enumerate() {
        write_registry_entry(
            registry.path(),
            &format!("s{idx}.json"),
            &format!("watcher-status-{idx}"),
            pid,
            &proc_start,
            "interactive",
            status,
            waiting_for,
            "/tmp/status",
            None,
        );
    }

    run_watcher_once(&base_url, &[registry.path()]).await;

    let sessions = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        let all_present = (0..4).all(|idx| {
            sessions
                .iter()
                .any(|s| s.session_id == format!("watcher-status-{idx}"))
        });
        all_present.then(|| sessions.to_vec())
    })
    .await;

    let get = |id: &str| sessions.iter().find(|s| s.session_id == id).unwrap();
    assert_eq!(
        get("watcher-status-0").status,
        Status::Working(WorkingStatus { tool: None }),
        "busy must map to Working"
    );
    assert_eq!(
        get("watcher-status-1").status,
        Status::Working(WorkingStatus { tool: None }),
        "shell must map to Working"
    );
    assert_eq!(
        get("watcher-status-2").status,
        Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: Some("thinking".into()),
        }),
        "idle must map to Waiting(Input) carrying waitingFor as detail"
    );
    assert_eq!(
        get("watcher-status-3").status,
        Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: Some("Allow Bash to run cargo test?".into()),
        }),
        "waiting must map to Waiting(Input) carrying waitingFor as detail"
    );

    handle.abort();
}

#[tokio::test]
async fn watcher_skips_malformed_registry_files_and_still_publishes_the_rest() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let sessions_dir = registry.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(sessions_dir.join("garbage.json"), "not json at all").unwrap();
    std::fs::write(sessions_dir.join("empty.json"), "").unwrap();

    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "good.json",
        "watcher-good-f",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/good",
        None,
    );

    run_watcher_once(&base_url, &[registry.path()]).await;

    let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-good-f")
            .cloned()
    })
    .await;
    assert_eq!(session.cwd, "/tmp/good");

    handle.abort();
}

#[tokio::test]
async fn watcher_treats_missing_registry_directory_as_empty_not_an_error() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let real_registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        real_registry.path(),
        "entry.json",
        "watcher-good-g",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/good-g",
        None,
    );

    let missing = real_registry.path().join("this-directory-does-not-exist");

    // `run_watcher_once` itself asserts the process exits successfully;
    // that assertion is the core of this acceptance criterion - a missing
    // registry directory must not fail the sweep or the binary.
    run_watcher_once(&base_url, &[missing.as_path(), real_registry.path()]).await;

    let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-good-g")
            .cloned()
    })
    .await;
    assert_eq!(session.cwd, "/tmp/good-g");

    handle.abort();
}

#[tokio::test]
async fn watcher_aggregates_sessions_from_multiple_registry_directories() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);

    write_registry_entry(
        dir_a.path(),
        "entry.json",
        "watcher-multi-a",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/multi-a",
        None,
    );
    write_registry_entry(
        dir_b.path(),
        "entry.json",
        "watcher-multi-b",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/multi-b",
        None,
    );

    run_watcher_once(&base_url, &[dir_a.path(), dir_b.path()]).await;

    wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        let has_a = sessions.iter().any(|s| s.session_id == "watcher-multi-a");
        let has_b = sessions.iter().any(|s| s.session_id == "watcher-multi-b");
        (has_a && has_b).then_some(())
    })
    .await;

    handle.abort();
}

// --- Discovery-path integration tests (PRO-208 review fixes) ---
//
// `watcher_refuses_to_publish_when_no_registry_dirs_are_configured` and
// `watcher_refuses_to_publish_when_registry_dirs_is_blank` used to live
// here, asserting that `csm-watcher --once` with `CSM_WATCHER_REGISTRY_DIRS`
// unset or blank exits non-zero and touches nothing. PRO-208 deleted both
// on the theory that driving real discovery through this binary would
// touch this machine's real processes and real hostname, and that the
// safety-critical distinction (a failed read must never publish an empty
// snapshot) was covered well enough by the `resolve_registry_dirs_*` unit
// tests in `crates/watcher/src/main.rs` alone.
//
// That trade was rejected on review: those unit tests exercise
// `resolve_registry_dirs` against an injected closure, which proves nothing
// about `run_once`'s actual exit-before-publish glue in `main.rs` (the code
// that turns a discovery error into "exit non-zero without calling
// `publish`") - that glue had, and until this file's version, has, zero
// coverage. And the "must touch real processes" premise is avoidable:
// `Command::new("ps")` (the only OS interface `discovery::discover` uses on
// macOS) resolves through the *child's own* `PATH`, so a stub `ps` binary
// placed alone in a tempdir, with the child's `PATH` pointed at nothing
// else, fully intercepts discovery without ever touching this machine's
// real processes. `HOME` is likewise pointed at an isolated, empty tempdir
// so the unconditional default-config-dir seed added as part of these same
// review fixes (see `discovery::union_discovery`) never accidentally
// sweeps this developer machine's real `~/.claude` registry. This is fully
// deterministic, and - because every test in this file talks to its own
// dedicated in-memory server via `start_test_server()` - it is safe to run
// in parallel with everything else here even though it necessarily
// publishes under this machine's real hostname (`common::hostname::resolve`
// is not overridable): there is no shared server for a real hostname to
// collide on, only the fake `host-*` names used elsewhere in this file
// avoid a *different* problem (colliding with each other on a shared
// server, which is not the situation here).
//
// macOS-only: Linux discovery reads `/proc/<pid>/{cmdline,environ}`
// directly rather than shelling out to `ps` at all, so this interception
// technique has no Linux equivalent, and - per `discovery.rs`'s own module
// doc comment - the impure half of Linux discovery cannot be exercised
// outside real Linux regardless.
#[cfg(target_os = "macos")]
mod discovery_path {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write an executable `/bin/sh` stub named `ps` at `<bin_dir>/ps`,
    /// whose body is `script`. Ignores whatever arguments the real
    /// `ps -Eww -ax -o pid=,command=` invocation passes - every case here
    /// wants a fixed, canned response regardless of the exact flags.
    fn write_stub_ps(bin_dir: &Path, script: &str) {
        let path = bin_dir.join("ps");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write stub ps");
        let mut perms = std::fs::metadata(&path)
            .expect("stat stub ps")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod stub ps");
    }

    /// Run `csm-watcher --once` with discovery live: `CSM_WATCHER_REGISTRY_DIRS`
    /// unset (so discovery, not the explicit override, is exercised),
    /// `PATH` pointed only at `bin_dir` (so the child's `ps` resolves to
    /// the stub written there), and `HOME` pointed at `home_dir` (so the
    /// unconditionally-seeded default config directory - see
    /// `discovery::union_discovery` - is an isolated, empty one rather than
    /// this developer's real `~/.claude`).
    async fn run_watcher_once_with_stub_ps(
        base_url: &str,
        bin_dir: &Path,
        home_dir: &Path,
    ) -> std::process::ExitStatus {
        use tokio::process::Command;
        Command::new(locate_bin("csm-watcher"))
            .arg("--once")
            .env("CLAUDE_MONITOR_URL", base_url)
            .env_remove("CSM_WATCHER_REGISTRY_DIRS")
            .env("PATH", bin_dir)
            .env("HOME", home_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .expect("failed to spawn csm-watcher")
    }

    /// Seed one baseline session under this machine's real hostname (via
    /// the snapshot endpoint directly, exactly like the non-watcher tests
    /// above), and wait for it to appear over SSE. Every test below expects
    /// this session to survive a refused publish untouched.
    async fn seed_baseline(base_url: &str, sse: &SseClient, session_id: &str) -> SessionView {
        let hostname = common::hostname::resolve().expect("resolve local hostname");
        post_snapshot(
            base_url,
            &hostname,
            "claude",
            vec![snapshot_session(
                session_id,
                &format!("/tmp/{session_id}"),
                working_status(),
            )],
        )
        .await;
        let session_id = session_id.to_string();
        wait_for(sse, WATCHER_TIMEOUT, move |sessions| {
            sessions
                .iter()
                .find(|s| s.session_id == session_id)
                .cloned()
        })
        .await
    }

    #[tokio::test]
    async fn watcher_refuses_to_publish_when_ps_fails_outright() {
        // Restores the assertion PRO-208 deleted: `ps` itself failing
        // (a non-zero exit) must make `discovery::discover` return `Err`,
        // which `run_once` must turn into a non-zero exit and no publish
        // call at all - never an empty snapshot.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let baseline = seed_baseline(&base_url, &sse, "discovery-ps-fails-1").await;

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(bin_dir.path(), "exit 7");

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            !status.success(),
            "csm-watcher must exit non-zero when process enumeration fails outright, got {status}"
        );

        tokio::time::sleep(SETTLE).await;
        let after = sse
            .sessions()
            .into_iter()
            .find(|s| s.session_id == "discovery-ps-fails-1")
            .expect("baseline session must still be present after a refused publish");
        assert_eq!(
            after.updated_at, baseline.updated_at,
            "a discovery failure must not touch a previously-published session"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_refuses_to_publish_when_ps_reports_zero_processes_total() {
        // Covers finding 1 from the PRO-208 review: a `ps` that exits 0
        // printing nothing at all - the exact stub the reviewer used to
        // reproduce a silent wipe of every live session on the host - must
        // be treated as a broken enumerator (see
        // `discovery::DiscoveryError::EmptyProcessList`), not as "zero live
        // Claude processes found". Before that fix, this stub made the
        // pre-filter process list and the post-filter Claude-process list
        // both empty in exactly the same way, so the watcher could not
        // distinguish "no Claude processes" from "ps is broken", reported
        // "no live Claude Code processes found", and proceeded to publish
        // (and thereby end) every real session on the host.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let baseline = seed_baseline(&base_url, &sse, "discovery-zero-procs-1").await;

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(bin_dir.path(), "exit 0");

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            !status.success(),
            "csm-watcher must exit non-zero when ps reports zero processes total, got {status}"
        );

        tokio::time::sleep(SETTLE).await;
        let after = sse
            .sessions()
            .into_iter()
            .find(|s| s.session_id == "discovery-zero-procs-1")
            .expect("baseline session must still be present after a refused publish");
        assert_eq!(
            after.updated_at, baseline.updated_at,
            "a zero-process ps result must not touch a previously-published session"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_discovers_registry_directory_from_process_environment_end_to_end() {
        // Exercises the whole discovery path end to end: a stub `ps` line
        // shaped like a real Claude process, carrying `CLAUDE_CONFIG_DIR`
        // (pointing discovery at a tempdir registry with no explicit
        // override at all) and `TMUX_PANE`. This is also the only coverage
        // in this file of `run_once`'s success path through discovery
        // rather than through the `CSM_WATCHER_REGISTRY_DIRS` escape hatch
        // every other watcher-driven test above uses.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "discovery-e2e-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/discovery-e2e-1",
            None,
        );

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '{pid} claude CLAUDE_CONFIG_DIR={registry_dir} TMUX_PANE=%3'",
                registry_dir = registry.path().display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully when discovery finds a real Claude process, \
             got {status}"
        );

        let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            sessions
                .iter()
                .find(|s| s.session_id == "discovery-e2e-1")
                .cloned()
        })
        .await;
        assert_eq!(session.cwd, "/tmp/discovery-e2e-1");

        handle.abort();
    }
}
