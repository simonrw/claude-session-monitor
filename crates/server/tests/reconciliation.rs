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
use test_support::{locate_bin, sandbox_home, start_test_server, wait_for};

// --- Helpers ---

fn busy_status() -> serde_json::Value {
    serde_json::json!({ "type": "busy", "tool": null })
}

fn waiting_status() -> serde_json::Value {
    serde_json::json!({ "type": "waiting", "detail": null })
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
            "status": { "type": "busy", "tool": null },
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
            snapshot_session("c1", "/tmp/c1", busy_status()),
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
    assert!(matches!(c1_updated.status, Status::Waiting { .. }));

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
            snapshot_session("d1", "/tmp/d1", busy_status()),
            snapshot_session("d2", "/tmp/d2", busy_status()),
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
        vec![snapshot_session("d1", "/tmp/d1", busy_status())],
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
            busy_status(),
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
        vec![snapshot_session("host-a-1", "/tmp/host-a", busy_status())],
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

    let sessions = vec![snapshot_session("s1", "/tmp/project", busy_status())];
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
        vec![snapshot_session("s1", "/tmp/other", busy_status())],
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
            snapshot_session("f1", "/tmp/f1", busy_status()),
            snapshot_session("f2", "/tmp/f2", busy_status()),
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
    let only_f1 = vec![snapshot_session("f1", "/tmp/f1", busy_status())];
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
            busy_status(),
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
            busy_status(),
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

// --- Host status (PRO-211): `GET /api/hosts` ---
//
// `POST /api/hosts/{hostname}/sessions` is idempotent and already covered
// above; these tests cover the separate, additive `host_status` tracking
// PRO-211 layers on top of it, which exists so a client can eventually tell
// "this host genuinely has zero live sessions" apart from "this host's
// watcher has stopped reporting" - see `common::api::HostStatus`'s doc
// comment. Recording last-seen must never depend on whether the snapshot
// changed anything or contained any sessions at all.

async fn get_hosts(base_url: &str) -> Vec<common::api::HostStatus> {
    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/hosts"))
        .send()
        .await
        .expect("GET /api/hosts");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.expect("parse host status response")
}

#[tokio::test]
async fn no_hosts_reported_before_any_snapshot_is_posted() {
    let (base_url, handle) = start_test_server().await;
    assert!(get_hosts(&base_url).await.is_empty());
    handle.abort();
}

#[tokio::test]
async fn a_snapshot_records_its_host_even_with_zero_sessions() {
    let (base_url, handle) = start_test_server().await;

    // An empty sessions list is a legitimate, honest snapshot (a host with
    // no live sessions right now) - it must still count as "seen".
    post_snapshot(&base_url, "host-empty", "claude", vec![]).await;

    let statuses = get_hosts(&base_url).await;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].hostname, "host-empty");
    assert_eq!(statuses[0].agent_kind, common::api::AgentKind::Claude);

    handle.abort();
}

#[tokio::test]
async fn repeat_unchanged_snapshot_still_advances_last_seen_at() {
    let (base_url, handle) = start_test_server().await;

    post_snapshot(
        &base_url,
        "host-h",
        "claude",
        vec![snapshot_session("h1", "/tmp/h1", busy_status())],
    )
    .await;
    let first = get_hosts(&base_url).await;
    let first_seen = first
        .iter()
        .find(|s| s.hostname == "host-h")
        .expect("host-h present after first snapshot")
        .last_seen_at;

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Byte-identical republish: `apply_snapshot` reports no change, but the
    // host has still just been heard from and must be recorded as such.
    post_snapshot(
        &base_url,
        "host-h",
        "claude",
        vec![snapshot_session("h1", "/tmp/h1", busy_status())],
    )
    .await;
    let second = get_hosts(&base_url).await;
    let second_seen = second
        .iter()
        .find(|s| s.hostname == "host-h")
        .expect("host-h present after second snapshot")
        .last_seen_at;

    assert_eq!(
        second.len(),
        1,
        "a repeat snapshot from the same host/agent kind must update the existing row, not add another"
    );
    assert!(
        second_seen > first_seen,
        "last_seen_at must advance even when the snapshot changed nothing"
    );

    handle.abort();
}

#[tokio::test]
async fn distinct_hosts_and_agent_kinds_are_tracked_independently() {
    let (base_url, handle) = start_test_server().await;

    post_snapshot(&base_url, "host-i", "claude", vec![]).await;
    post_snapshot(&base_url, "host-i", "codex", vec![]).await;
    post_snapshot(&base_url, "host-j", "claude", vec![]).await;

    let statuses = get_hosts(&base_url).await;
    assert_eq!(statuses.len(), 3);

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
// carry its name as set by `/rename`. `write_registry_entry` below writes a
// real `name` into the fixture (see
// `watcher_publishes_a_live_interactive_session`), which is genuinely
// parsed by `registry::RegistryEntry`, carried into `SnapshotSession` and
// the publish payload the watcher sends, persisted by the server, and
// surfaced on `SessionView` (PRO-215) - so
// `watcher_publishes_a_live_interactive_session` asserts on `session.name`
// through `SseClient`, closing the gap this comment used to describe.

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
/// real value so `name` is exercised end to end - registry parse, publish
/// payload, the stored column, and finally the `SessionView` that test
/// asserts on after reading it back over SSE.
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
///
/// `HOME` is pointed at a fresh, throwaway `sandbox_home()` (dropped, and so
/// deleted, once this function returns - fine here since the child has
/// already exited by then, unlike the long-running `daemon` helpers below):
/// `csm-watcher` derives its log directory from `$HOME` (see
/// `crates/watcher/src/main.rs`'s `default_log_dir`), and without this every
/// call here would instead append into the developer's real
/// `~/.local/share/claude-session-monitor/` (PRO-218). This does not affect
/// git detection for a real repo under a test `cwd` (see
/// `watcher_reports_git_branch_and_remote_for_a_cwd_inside_a_real_repo`
/// below): branch/remote lookups read `.git/config` inside that repo, not
/// global git configuration under `$HOME`.
async fn run_watcher_once(base_url: &str, registry_dirs: &[&Path]) {
    use tokio::process::Command;

    let joined = std::env::join_paths(registry_dirs.iter().map(|p| p.as_os_str()))
        .expect("join registry dirs");
    let home = sandbox_home();
    let status = Command::new(locate_bin("csm-watcher"))
        .arg("--once")
        .env("CLAUDE_MONITOR_URL", base_url)
        .env("CSM_WATCHER_REGISTRY_DIRS", joined)
        .env("HOME", home.path())
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
    assert_eq!(session.status, Status::Busy { tool: None });
    // Closes the gap PRO-207 could not test (see the comment above this
    // test group): the registry's `name` field, as set by `/rename`,
    // reaches a connected SSE client end to end.
    assert_eq!(session.name, Some("captain-marvel".into()));

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

    // `idle`'s `waiting_for` is deliberately non-`None` here (and expected
    // to be ignored): under the new straight pass-through mapping (see
    // `common::session::Status::from_registry`), `Idle` carries no detail at
    // all, so a registry entry claiming to be idle while also setting
    // `waitingFor` must still map to plain `Idle`, not leak that value
    // anywhere.
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
        Status::Busy { tool: None },
        "registry's busy must pass straight through to Busy (tool is always None for Claude)"
    );
    assert_eq!(
        get("watcher-status-1").status,
        Status::Shell,
        "registry's shell must pass straight through to Shell"
    );
    assert_eq!(
        get("watcher-status-2").status,
        Status::Idle,
        "registry's idle must pass straight through to Idle, ignoring waitingFor"
    );
    assert_eq!(
        get("watcher-status-3").status,
        Status::Waiting {
            detail: Some("Allow Bash to run cargo test?".into()),
        },
        "registry's waiting must pass straight through to Waiting, carrying waitingFor as detail"
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

/// Run `csm-watcher --once` against `registry_dirs`, returning the exit
/// status rather than asserting it succeeded - unlike `run_watcher_once`,
/// used by tests (PRO-211) that expect the sweep to fail outright.
async fn run_watcher_once_expect_status(
    base_url: &str,
    registry_dirs: &[&Path],
) -> std::process::ExitStatus {
    use tokio::process::Command;

    let joined = std::env::join_paths(registry_dirs.iter().map(|p| p.as_os_str()))
        .expect("join registry dirs");
    // See `run_watcher_once`'s doc comment: sandboxes the log directory
    // only, away from the developer's real one.
    let home = sandbox_home();
    Command::new(locate_bin("csm-watcher"))
        .arg("--once")
        .env("CLAUDE_MONITOR_URL", base_url)
        .env("CSM_WATCHER_REGISTRY_DIRS", joined)
        .env("HOME", home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .expect("failed to spawn csm-watcher")
}

/// PRO-211: a sweep that cannot read a *discovered* registry directory (one
/// that exists but is unreadable - distinct from one that simply does not
/// exist yet, see `watcher_treats_missing_registry_directory_as_empty_not_an_error`
/// just above) must publish nothing at all and end no session, not fall
/// back to whatever the other, readable directories happened to find.
#[tokio::test]
async fn watcher_refuses_to_publish_when_a_discovered_registry_directory_cannot_be_read() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    // Seed a baseline session via a normal, successful sweep first, so
    // there is something a wrongly-empty publish could wrongly end.
    let good_registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        good_registry.path(),
        "entry.json",
        "watcher-unreadable-dir-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/watcher-unreadable-dir-1",
        None,
    );
    run_watcher_once(&base_url, &[good_registry.path()]).await;
    let baseline = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-unreadable-dir-1")
            .cloned()
    })
    .await;

    // A second registry directory whose `sessions` subdirectory exists but
    // cannot be read - not merely absent - so `registry::read_entries`
    // must surface a real `ReadError::Dir` rather than treat it as empty.
    let unreadable_registry = tempfile::tempdir().unwrap();
    let unreadable_sessions_dir = unreadable_registry.path().join("sessions");
    std::fs::create_dir_all(&unreadable_sessions_dir).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &unreadable_sessions_dir,
            std::fs::Permissions::from_mode(0o000),
        )
        .expect("chmod the sessions dir unreadable");
    }

    let status = run_watcher_once_expect_status(
        &base_url,
        &[good_registry.path(), unreadable_registry.path()],
    )
    .await;

    // Restore permissions so the tempdir can be cleaned up on drop.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &unreadable_sessions_dir,
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("restore sessions dir permissions for cleanup");
    }

    assert!(
        !status.success(),
        "csm-watcher must exit non-zero when a discovered registry directory cannot be read, got {status}"
    );

    tokio::time::sleep(SETTLE).await;
    let after = sse
        .sessions()
        .into_iter()
        .find(|s| s.session_id == "watcher-unreadable-dir-1")
        .expect("baseline session must still be present after a refused publish");
    assert_eq!(
        after.updated_at, baseline.updated_at,
        "a failed sweep must not touch a previously-published session"
    );

    handle.abort();
}

/// PRO-211 second-round review finding 1: a `sessions` directory that is
/// *listable but not stat-able* - readable (`r--`) but not executable/
/// searchable, e.g. mode `0o444` - is a different failure shape from the
/// fully unreadable directory above (mode `0o000`, where `read_dir` itself
/// fails to open). Here `read_dir` succeeds and yields every entry's name,
/// but a per-entry `stat` (which needs search permission on the parent, not
/// just read) fails with `EACCES` for every one of them. Before this fix,
/// `read_entries` decided whether an entry was worth reading via
/// `path.is_file()`, which calls `fs::metadata` and folds any `Err` into
/// `false` - so every entry looked like "not a file", the loop skipped all
/// of them, and the sweep returned a successful, empty result: no warning,
/// no error, exit code 0, and the previously-published session below would
/// be silently ended.
#[tokio::test]
async fn watcher_refuses_to_publish_when_a_registry_directory_is_listable_but_not_readable() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    // Seed a baseline session via a normal, successful sweep first, so
    // there is something a wrongly-empty publish could wrongly end.
    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "good.json",
        "watcher-listable-not-statable-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/watcher-listable-not-statable-1",
        None,
    );
    run_watcher_once(&base_url, &[registry.path()]).await;
    let baseline = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-listable-not-statable-1")
            .cloned()
    })
    .await;

    // Now make the `sessions` directory readable-but-not-executable: names
    // can still be listed, but nothing inside can be stat'd or opened.
    let sessions_dir = registry.path().join("sessions");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sessions_dir, std::fs::Permissions::from_mode(0o444))
            .expect("chmod the sessions dir readable-but-not-searchable");
    }

    let status = run_watcher_once_expect_status(&base_url, &[registry.path()]).await;

    // Restore permissions so the tempdir can be cleaned up on drop.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sessions_dir, std::fs::Permissions::from_mode(0o755))
            .expect("restore sessions dir permissions for cleanup");
    }

    assert!(
        !status.success(),
        "csm-watcher must exit non-zero when a registry directory's entries cannot be \
         stat'd, got {status}"
    );

    tokio::time::sleep(SETTLE).await;
    let after = sse
        .sessions()
        .into_iter()
        .find(|s| s.session_id == "watcher-listable-not-statable-1")
        .expect("baseline session must still be present after a refused publish");
    assert_eq!(
        after.updated_at, baseline.updated_at,
        "a failed sweep must not touch a previously-published session"
    );

    handle.abort();
}

/// PRO-211 review finding 4: an *individual* registry file that exists but
/// cannot be read at all (EACCES, EIO, ...) must refuse the whole sweep, the
/// same as an unreadable registry *directory* just above - not be folded
/// into the same lenient skip a malformed/empty JSON file gets (see
/// `watcher_skips_malformed_registry_files_and_still_publishes_the_rest`).
/// Before this fix, `registry::parse_file` treated a read failure and a
/// parse failure identically: both logged a warning and returned `None`, so
/// an unreadable file silently vanished from the swept set exactly like a
/// malformed one - even though "malformed" means Claude Code wrote
/// something this project doesn't understand, while "unreadable" means this
/// sweep cannot know whether that session is live at all.
#[tokio::test]
async fn watcher_refuses_to_publish_when_a_registry_file_cannot_be_read() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    // Seed a baseline session via a normal, successful sweep first, so
    // there is something a wrongly-empty publish could wrongly end.
    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "good.json",
        "watcher-unreadable-file-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/watcher-unreadable-file-1",
        None,
    );
    run_watcher_once(&base_url, &[registry.path()]).await;
    let baseline = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-unreadable-file-1")
            .cloned()
    })
    .await;

    // A second, individual file in the same directory whose bytes cannot be
    // read - the directory itself and `read_dir` iteration both succeed, so
    // this exercises `registry::ReadError::File`, not `ReadError::Dir`.
    let sessions_dir = registry.path().join("sessions");
    let unreadable_file = sessions_dir.join("unreadable.json");
    std::fs::write(&unreadable_file, "{}").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unreadable_file, std::fs::Permissions::from_mode(0o000))
            .expect("chmod the registry file unreadable");
    }

    let status = run_watcher_once_expect_status(&base_url, &[registry.path()]).await;

    // Restore permissions so the tempdir can be cleaned up on drop.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unreadable_file, std::fs::Permissions::from_mode(0o644))
            .expect("restore registry file permissions for cleanup");
    }

    assert!(
        !status.success(),
        "csm-watcher must exit non-zero when a registry file cannot be read, got {status}"
    );

    tokio::time::sleep(SETTLE).await;
    let after = sse
        .sessions()
        .into_iter()
        .find(|s| s.session_id == "watcher-unreadable-file-1")
        .expect("baseline session must still be present after a refused publish");
    assert_eq!(
        after.updated_at, baseline.updated_at,
        "a failed sweep must not touch a previously-published session"
    );

    handle.abort();
}

/// PRO-211 second-round review finding 1: a registry file that vanishes
/// between `read_dir` listing it and `registry::parse_file` reading its
/// bytes is a benign, expected race - not a whole-sweep failure. Claude Code
/// deletes `<pid>.json` the instant a session ends, so this happens
/// routinely in real use; treating it as `ReadError::File` (the same as an
/// EACCES/EIO failure just above) meant a session ending during a sweep
/// could take the *entire* sweep down with it, publishing nothing and
/// triggering backoff, purely from an ordinary process exit racing an
/// ordinary directory listing.
///
/// A real unlink-during-`read_dir` race is inherently timing-dependent, so
/// this reproduces the exact failure shape deterministically instead: a
/// dangling symlink. `DirEntry::file_type()` resolves a symlink without
/// following it (so the entry is not skipped as "not a file" - it is not a
/// directory), and falls through to `parse_file`'s own `read_to_string`,
/// which - because the symlink's target does not exist - fails with exactly
/// the same `ErrorKind::NotFound` a genuinely deleted regular file would
/// produce. This is the identical code path a real deletion race exercises;
/// only the mechanism producing `ENOENT` differs.
#[tokio::test]
async fn watcher_skips_a_registry_file_that_vanishes_before_it_can_be_read_and_still_publishes() {
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "good.json",
        "watcher-vanished-race-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        "/tmp/watcher-vanished-race-1",
        None,
    );

    // A dangling symlink: `read_dir` lists it, `file_type()` resolves it
    // without an error (it is not a directory), but reading its bytes fails
    // with ENOENT because the target does not exist - deterministically
    // reproducing "listed, then gone by the time it's read" without an
    // actual race.
    let sessions_dir = registry.path().join("sessions");
    std::os::unix::fs::symlink(
        sessions_dir.join("this-target-does-not-exist.json"),
        sessions_dir.join("vanished.json"),
    )
    .expect("create dangling symlink fixture");

    // Before this fix this would exit non-zero and publish nothing at all;
    // `run_watcher_once` itself asserts a successful exit, which is the core
    // of this acceptance criterion.
    run_watcher_once(&base_url, &[registry.path()]).await;

    let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "watcher-vanished-race-1")
            .cloned()
    })
    .await;
    assert_eq!(session.cwd, "/tmp/watcher-vanished-race-1");

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

#[tokio::test]
async fn watcher_reports_no_git_enrichment_for_a_cwd_outside_any_repository() {
    // Uses the real system `git` (via `run_watcher_once`'s inherited PATH,
    // not a stub): a cwd that is a real, existing directory but not inside
    // any git repository must still publish successfully, with git_branch
    // and git_remote both absent rather than the sweep failing.
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let non_repo_cwd = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "entry.json",
        "git-none-e2e-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        non_repo_cwd.path().to_str().unwrap(),
        None,
    );

    run_watcher_once(&base_url, &[registry.path()]).await;

    let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "git-none-e2e-1")
            .cloned()
    })
    .await;
    assert_eq!(session.git_branch, None);
    assert_eq!(session.git_remote, None);

    handle.abort();
}

#[tokio::test]
async fn watcher_reports_git_branch_and_remote_for_a_cwd_inside_a_real_repo() {
    // Real system `git`, real repo: `git init`, one commit (git refuses to
    // resolve HEAD on a totally empty repo), and a remote, then confirms the
    // watcher's own git detection (independent of `crates/reporter`) reports
    // both.
    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let registry = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let run_git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo_dir.path())
            .status()
            .expect("failed to spawn git while seeding fixture repo");
        assert!(
            status.success(),
            "git {args:?} failed while seeding fixture repo"
        );
    };
    run_git(&["init", "--initial-branch=pro-209-fixture-branch"]);
    run_git(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=test",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);
    run_git(&[
        "remote",
        "add",
        "origin",
        "git@example.com:acme/pro-209-fixture.git",
    ]);

    let pid = std::process::id();
    let proc_start = registry_proc_start_for(pid);
    write_registry_entry(
        registry.path(),
        "entry.json",
        "git-real-e2e-1",
        pid,
        &proc_start,
        "interactive",
        "busy",
        None,
        repo_dir.path().to_str().unwrap(),
        None,
    );

    run_watcher_once(&base_url, &[registry.path()]).await;

    let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "git-real-e2e-1")
            .cloned()
    })
    .await;
    assert_eq!(
        session.git_branch.as_deref(),
        Some("pro-209-fixture-branch")
    );
    assert_eq!(
        session.git_remote.as_deref(),
        Some("git@example.com:acme/pro-209-fixture.git")
    );

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

    /// Write an executable `/bin/sh` stub at `path`, whose body is `script`.
    fn write_stub_executable(path: &Path, script: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{script}\n")).expect("write stub executable");
        let mut perms = std::fs::metadata(path)
            .expect("stat stub executable")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod stub executable");
    }

    /// Write an executable `/bin/sh` stub named `ps` at `<bin_dir>/ps`,
    /// whose body is `script`. Ignores whatever arguments the real
    /// `ps -Eww -ax -o pid=,command=` invocation passes - every case here
    /// wants a fixed, canned response regardless of the exact flags.
    fn write_stub_ps(bin_dir: &Path, script: &str) {
        write_stub_executable(&bin_dir.join("ps"), script);
    }

    /// Write an executable `/bin/sh` stub named `tmux` at `<bin_dir>/tmux`,
    /// whose body is `script`. Used (PRO-209) to intercept the watcher's
    /// `tmux list-panes -a` invocation, both to hand back a canned pane
    /// listing and to prove it is called at most once per sweep by having
    /// the stub log its own invocations.
    fn write_stub_tmux(bin_dir: &Path, script: &str) {
        write_stub_executable(&bin_dir.join("tmux"), script);
    }

    /// Write an executable `/bin/sh` stub named `git` at `<bin_dir>/git`,
    /// whose body is `script`. Used (PRO-209) to intercept the watcher's
    /// git branch/remote lookups: a canned response independent of the real
    /// filesystem, and (like `write_stub_tmux`) an invocation log to prove
    /// the per-`cwd` cache collapses repeated lookups.
    fn write_stub_git(bin_dir: &Path, script: &str) {
        write_stub_executable(&bin_dir.join("git"), script);
    }

    /// This test process's own uid, for stamping into stub `ps` lines'
    /// `uid=` column (PRO-211 second-round review finding 2: `ps -Eww -ax -o
    /// pid=,uid=,command=` grew a `uid=` column so `discovery::
    /// build_claude_processes` can tell a foreign-uid process's unreadable
    /// environment apart from a genuine same-uid read failure). Every stub
    /// `ps` line below must report *this* uid, or the watcher child process
    /// (which also runs as this same uid) treats its own stubbed Claude
    /// process as foreign and silently skips it rather than reading its
    /// environment.
    fn current_uid() -> u32 {
        // SAFETY: `getuid()` takes no arguments, dereferences no pointers,
        // and cannot fail.
        unsafe { libc::getuid() }
    }

    /// Run `csm-watcher --once` with discovery live: `CSM_WATCHER_REGISTRY_DIRS`
    /// unset (so discovery, not the explicit override, is exercised),
    /// `PATH` pointed only at `bin_dir` (so the child's `ps`/`tmux`/`git`
    /// resolve to whatever stubs were written there), `HOME` pointed at
    /// `home_dir` (so the unconditionally-seeded default config directory -
    /// see `discovery::union_discovery` - is an isolated, empty one rather
    /// than this developer's real `~/.claude`), and any `extra_envs` set on
    /// top (used to point a stub `tmux`/`git` at an invocation-count log
    /// file it can find in its own environment).
    async fn run_watcher_once_with_stub_ps_and_envs(
        base_url: &str,
        bin_dir: &Path,
        home_dir: &Path,
        extra_envs: &[(&str, &str)],
    ) -> std::process::ExitStatus {
        use tokio::process::Command;
        let mut cmd = Command::new(locate_bin("csm-watcher"));
        cmd.arg("--once")
            .env("CLAUDE_MONITOR_URL", base_url)
            .env_remove("CSM_WATCHER_REGISTRY_DIRS")
            .env("PATH", bin_dir)
            .env("HOME", home_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (key, value) in extra_envs {
            cmd.env(key, value);
        }
        cmd.status().await.expect("failed to spawn csm-watcher")
    }

    async fn run_watcher_once_with_stub_ps(
        base_url: &str,
        bin_dir: &Path,
        home_dir: &Path,
    ) -> std::process::ExitStatus {
        run_watcher_once_with_stub_ps_and_envs(base_url, bin_dir, home_dir, &[]).await
    }

    /// Run `csm-watcher --once` with the explicit `CSM_WATCHER_REGISTRY_DIRS`
    /// override set (directory *discovery* bypassed) but `PATH` still
    /// pointed at `bin_dir`'s stubs, so a stub `ps`/`tmux` there still runs
    /// for pane capture. Used to prove finding 3 from the PRO-209 review:
    /// the override bypasses directory discovery only, not tmux enrichment.
    async fn run_watcher_once_with_override_and_stub_ps(
        base_url: &str,
        registry_dirs: &[&Path],
        bin_dir: &Path,
        home_dir: &Path,
    ) -> std::process::ExitStatus {
        use tokio::process::Command;
        let joined = std::env::join_paths(registry_dirs.iter().map(|p| p.as_os_str()))
            .expect("join registry dirs");
        Command::new(locate_bin("csm-watcher"))
            .arg("--once")
            .env("CLAUDE_MONITOR_URL", base_url)
            .env("CSM_WATCHER_REGISTRY_DIRS", joined)
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
                busy_status(),
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
        //
        // `bin_dir` here carries no `tmux` or `git` stub at all (PATH is
        // pointed exclusively at it), so this also doubles as PRO-209's
        // "tools unavailable" degrade case: `TMUX_PANE=%3` is captured by
        // discovery, but with no `tmux` binary to resolve it against, and no
        // `git` binary to derive branch/remote from `cwd`, the session must
        // still publish successfully with all three enrichment fields
        // simply absent - never a sweep failure.
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
                "echo '{pid} {uid} claude CLAUDE_CONFIG_DIR={registry_dir} TMUX_PANE=%3'",
                uid = current_uid(),
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
        assert_eq!(
            session.tmux_target, None,
            "tmux is unavailable on PATH; must degrade, not fail"
        );
        assert_eq!(
            session.git_branch, None,
            "git is unavailable on PATH; must degrade, not fail"
        );
        assert_eq!(
            session.git_remote, None,
            "git is unavailable on PATH; must degrade, not fail"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_discovery_skips_a_foreign_uid_claude_process_instead_of_failing_forever() {
        // PRO-211 second-round review finding 2: `ps -Eww` prints full argv
        // for a process owned by another user while silently omitting its
        // environment - the exact same shape as a genuine same-uid read
        // failure. Before this fix, `discovery::build_claude_processes`
        // could not tell the two apart and turned *any* Claude-matched line
        // with zero environment tokens into `DiscoveryError::
        // UnreadableEnvironment`. Since a foreign-uid process's environment
        // can never become readable to this watcher - not on retry, not
        // ever - that made discovery fail on *every single sweep* for as
        // long as the foreign process stayed alive, publishing nothing
        // indefinitely: a permanent, self-inflicted outage, not a transient
        // blip a normal backoff-and-retry could recover from.
        //
        // The stub `ps` output here reports two lines: pid 99999, owned by
        // uid 0, named `claude`, with no environment at all (standing in
        // for `sudo claude`, or any Claude process on a shared host owned by
        // a different user) - and this test process's own real pid, owned
        // by this test's own uid, with a real `CLAUDE_CONFIG_DIR`. If the
        // foreign-uid line still caused a discovery failure, the watcher
        // would exit non-zero and the real session below would never
        // publish; if it is correctly skipped instead, discovery still
        // succeeds and the real session publishes normally.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "foreign-uid-e2e-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/foreign-uid-e2e-1",
            None,
        );

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '99999     0 claude'\necho '{pid} {uid} claude CLAUDE_CONFIG_DIR={registry_dir}'",
                uid = current_uid(),
                registry_dir = registry.path().display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully - a foreign-uid Claude process with an \
             unreadable environment must be skipped, not treated as a discovery failure, got \
             {status}"
        );

        let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            sessions
                .iter()
                .find(|s| s.session_id == "foreign-uid-e2e-1")
                .cloned()
        })
        .await;
        assert_eq!(session.cwd, "/tmp/foreign-uid-e2e-1");

        handle.abort();
    }

    /// PRO-211 third-round review finding 2: a `ps` line that cannot be
    /// parsed must fail discovery outright, not be silently dropped.
    /// Reproduces the reviewer's exact demonstration: two live profiles,
    /// only the second's `uid=` column malformed. Before this fix,
    /// `discovery::parse_ps_output`'s `filter_map` silently discarded the
    /// unparseable line, so discovery still succeeded - using only profile
    /// A's config directory - and the watcher exited 0, publishing a
    /// snapshot that omitted every session in profile B and thereby ended
    /// them, with no error and no warning at all.
    #[tokio::test]
    async fn watcher_refuses_to_publish_when_a_ps_lines_uid_column_is_malformed() {
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let baseline = seed_baseline(&base_url, &sse, "malformed-uid-e2e-1").await;

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            dir_a.path(),
            "entry.json",
            "profile-a-should-not-publish-alone",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/profile-a",
            None,
        );
        write_registry_entry(
            dir_b.path(),
            "entry.json",
            "profile-b-must-not-be-silently-ended",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/profile-b",
            None,
        );

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '{pid} {uid} claude CLAUDE_CONFIG_DIR={dir_a}'\n\
                 echo '{pid} not-a-uid claude CLAUDE_CONFIG_DIR={dir_b}'",
                uid = current_uid(),
                dir_a = dir_a.path().display(),
                dir_b = dir_b.path().display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            !status.success(),
            "csm-watcher must exit non-zero when a ps line's uid column cannot be parsed, got \
             {status}"
        );

        tokio::time::sleep(SETTLE).await;
        let sessions = sse.sessions();
        let after = sessions
            .iter()
            .find(|s| s.session_id == "malformed-uid-e2e-1")
            .expect("baseline session must still be present after a refused publish");
        assert_eq!(
            after.updated_at, baseline.updated_at,
            "a discovery failure must not touch a previously-published session"
        );
        assert!(
            sessions
                .iter()
                .all(|s| s.session_id != "profile-a-should-not-publish-alone"),
            "profile A must not be silently published while profile B's ps line fails to parse"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_resolves_a_default_config_dir_from_a_processs_own_home_not_the_watchers() {
        // PRO-211 second-round review finding 3, pre-existing since PRO-208:
        // `discovery::default_config_dir` resolved `~/.claude` from the
        // *watcher's own* `$HOME`, never the Claude process's, even though
        // that process's own `HOME` is already sitting in the same
        // environment `CLAUDE_CONFIG_DIR` and `TMUX_PANE` are read from. A
        // watcher whose `$HOME` differs from the session owner's - a
        // service account, a `sudo`/`su`-launched watcher, a differently
        // configured shell - swept the *wrong* default profile, with a
        // successful exit, silently ending the real one.
        //
        // Reproduced directly here: `home_dir` (the watcher's own `$HOME`,
        // via `run_watcher_once_with_stub_ps`) is an empty, unrelated
        // tempdir with no registry at all. The stub `ps` line reports no
        // `CLAUDE_CONFIG_DIR` at all, only `HOME=<process_home>` - a
        // *different* directory that actually holds the registry. If
        // discovery still resolved against the watcher's own `$HOME`
        // (`home_dir`), it would sweep an empty directory and never publish
        // this session; resolving against the process's own `HOME` instead
        // must find and publish it.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let process_home = tempfile::tempdir().unwrap();
        let registry = process_home.path().join(".claude");
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            &registry,
            "entry.json",
            "process-home-e2e-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/process-home-e2e-1",
            None,
        );

        let bin_dir = tempfile::tempdir().unwrap();
        // Deliberately a different, empty directory from `process_home` -
        // the watcher's own $HOME must not be where this session is found.
        let watcher_home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '{pid} {uid} claude HOME={process_home}'",
                uid = current_uid(),
                process_home = process_home.path().display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), watcher_home_dir.path()).await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully, got {status}"
        );

        let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            sessions
                .iter()
                .find(|s| s.session_id == "process-home-e2e-1")
                .cloned()
        })
        .await;
        assert_eq!(
            session.cwd, "/tmp/process-home-e2e-1",
            "the session must be found via the Claude process's own HOME, not the watcher's"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_resolves_tmux_target_from_pane_id_end_to_end() {
        // Positive-path counterpart to the degrade case above: a stub `tmux`
        // on PATH answers `list-panes -a -F ...` with one pane matching the
        // `TMUX_PANE` the stub `ps` line reports, and the published session
        // must carry the resulting `session:window.pane` activation target.
        //
        // The stub also records its own argv (finding 8 from the PRO-209
        // review): a canned `echo` response answers *any* invocation
        // regardless of flags, so nothing previously pinned the actual
        // `-F <format>` string `tmux.rs`'s `LIST_PANES_FORMAT` sends - a
        // wrong format string (say, a typo'd field name) would still pass
        // this whole suite as long as the stub kept echoing the same fixed
        // line. Asserting the recorded argv closes that gap.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "tmux-e2e-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/tmux-e2e-1",
            None,
        );

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();
        let argv_log_path = log_dir.path().join("tmux-argv.log");
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '{pid} {uid} claude CLAUDE_CONFIG_DIR={registry_dir} TMUX_PANE=%3'",
                uid = current_uid(),
                registry_dir = registry.path().display(),
            ),
        );
        write_stub_tmux(
            bin_dir.path(),
            &format!(
                "for a in \"$@\"; do echo \"$a\" >> '{log}'; done\necho '%3 my-session:0.1'",
                log = argv_log_path.display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully, got {status}"
        );

        let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            sessions
                .iter()
                .find(|s| s.session_id == "tmux-e2e-1")
                .cloned()
        })
        .await;
        assert_eq!(session.tmux_target.as_deref(), Some("my-session:0.1"));

        let argv = std::fs::read_to_string(&argv_log_path).unwrap_or_default();
        let argv: Vec<&str> = argv.lines().collect();
        assert_eq!(
            argv,
            vec![
                "list-panes",
                "-a",
                "-F",
                "#{pane_id} #{session_name}:#{window_index}.#{pane_index}",
            ],
            "the exact tmux list-panes invocation, including its -F format string, must be \
             pinned, not just some canned response the stub happens to answer"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_still_captures_tmux_panes_when_the_explicit_registry_dirs_override_is_set() {
        // Finding 3 from the PRO-209 review: `CSM_WATCHER_REGISTRY_DIRS`
        // bypasses directory *discovery* only. PRO-204 documents that
        // variable as a permanent, supported escape hatch, not scaffolding,
        // so a session published while it is set must still resolve a
        // `tmux_target` exactly like one found via normal discovery -
        // before this fix, the override path reported an empty pane map
        // unconditionally and every session published under it silently
        // lost jump-to-session.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "override-tmux-e2e-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/override-tmux-e2e-1",
            None,
        );

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        // The stub `ps` line's `CLAUDE_CONFIG_DIR` is deliberately
        // irrelevant here: the override, not discovery, decides
        // `registry_dirs`. `TMUX_PANE` is what pane capture must still
        // pick up even though directory discovery never runs.
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '{pid} {uid} claude CLAUDE_CONFIG_DIR=/irrelevant TMUX_PANE=%9'",
                uid = current_uid(),
            ),
        );
        write_stub_tmux(bin_dir.path(), "echo '%9 override-session:0.2'");

        let status = run_watcher_once_with_override_and_stub_ps(
            &base_url,
            &[registry.path()],
            bin_dir.path(),
            home_dir.path(),
        )
        .await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully, got {status}"
        );

        let session = wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            sessions
                .iter()
                .find(|s| s.session_id == "override-tmux-e2e-1")
                .cloned()
        })
        .await;
        assert_eq!(session.cwd, "/tmp/override-tmux-e2e-1");
        assert_eq!(
            session.tmux_target.as_deref(),
            Some("override-session:0.2"),
            "the explicit registry-dirs override must not silently drop tmux enrichment"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_resolves_tmux_panes_with_exactly_one_invocation_regardless_of_session_count() {
        // Acceptance criterion: pane resolution must cost exactly one `tmux
        // list-panes -a` invocation per sweep, no matter how many sessions
        // are being enriched. The stub `tmux` appends a line to an
        // invocation log every time it runs; with three live sessions this
        // sweep must still only touch the log once.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("tmux-invocations.log");

        let base_pid = std::process::id();
        let mut ps_lines = Vec::new();
        for (i, pane) in ["%1", "%2", "%3"].iter().enumerate() {
            // All three registry entries deliberately share this test
            // process's own real pid - they are not distinct pids at all,
            // synthetic or otherwise. `is_live`'s pid-existence check needs
            // a genuinely live process, and spawning three real children
            // just to get three distinct pids would add nothing this test
            // actually checks. One consequence worth being explicit about:
            // since discovery's `tmux_panes` map is keyed by pid, and the
            // stub `ps` below emits three lines all reporting this same
            // pid (each with a different `TMUX_PANE`), only the last one
            // survives insertion - the three stub pane ids below are never
            // separately distinguished by pid. That's immaterial to what
            // this test proves, which is the invocation *count* (still
            // exactly one `tmux list-panes` call for the whole sweep,
            // regardless of session count), not which specific pane each
            // session id resolves to.
            let pid = base_pid;
            let session_id = format!("tmux-count-e2e-{i}");
            let proc_start = registry_proc_start_for(pid);
            write_registry_entry(
                registry.path(),
                &format!("entry-{i}.json"),
                &session_id,
                pid,
                &proc_start,
                "interactive",
                "busy",
                None,
                &format!("/tmp/tmux-count-e2e-{i}"),
                None,
            );
            ps_lines.push(format!(
                "echo '{pid} {uid} claude CLAUDE_CONFIG_DIR={registry_dir} TMUX_PANE={pane}'",
                uid = current_uid(),
                registry_dir = registry.path().display(),
            ));
        }

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(bin_dir.path(), &ps_lines.join("\n"));
        write_stub_tmux(
            bin_dir.path(),
            &format!(
                "echo invoked >> '{log}'\necho '%1 s:0.0'\necho '%2 s:0.1'\necho '%3 s:0.2'",
                log = log_path.display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps_and_envs(&base_url, bin_dir.path(), home_dir.path(), &[])
                .await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully, got {status}"
        );

        // All three sessions share one real pid (this test process's), so
        // wait for all three session_ids to have been published at least
        // once before inspecting the invocation log.
        wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            let count = sessions
                .iter()
                .filter(|s| s.session_id.starts_with("tmux-count-e2e-"))
                .count();
            (count == 3).then_some(())
        })
        .await;

        let invocations = std::fs::read_to_string(&log_path).unwrap_or_default();
        let invocation_count = invocations.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(
            invocation_count, 1,
            "expected exactly one tmux list-panes invocation for the whole sweep, got: \
             {invocations:?}"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn watcher_git_lookups_collapse_across_sessions_sharing_one_cwd() {
        // Acceptance criterion: git lookups are cached by cwd, so sessions
        // sharing one cwd must collapse to a single git invocation per
        // command within one sweep. The stub `git` appends a line to an
        // invocation log every time it runs (once per subcommand it
        // handles), regardless of which subcommand was requested.
        let (base_url, handle) = start_test_server().await;
        let sse = SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("git-invocations.log");
        // Must be a real, existing directory: `command::run` sets it as
        // the child process's `current_dir`, and `Command::spawn` fails
        // before ever executing anything if that directory does not exist -
        // which would make this test pass for the wrong reason (an
        // unrelated degrade path, not cache collapse).
        let shared_cwd_dir = tempfile::tempdir().unwrap();
        let shared_cwd = shared_cwd_dir.path().to_str().unwrap();

        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        for i in 0..2 {
            write_registry_entry(
                registry.path(),
                &format!("entry-{i}.json"),
                &format!("git-collapse-e2e-{i}"),
                pid,
                &proc_start,
                "interactive",
                "busy",
                None,
                shared_cwd,
                None,
            );
        }

        let bin_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        write_stub_ps(
            bin_dir.path(),
            &format!(
                "echo '{pid} {uid} claude CLAUDE_CONFIG_DIR={registry_dir}'",
                uid = current_uid(),
                registry_dir = registry.path().display(),
            ),
        );
        write_stub_git(
            bin_dir.path(),
            &format!(
                "echo invoked >> '{log}'\n\
                 case \"$1 $2\" in\n\
                 'rev-parse --abbrev-ref') echo main ;;\n\
                 'remote get-url') echo 'git@example.com:acme/repo.git' ;;\n\
                 esac",
                log = log_path.display(),
            ),
        );

        let status =
            run_watcher_once_with_stub_ps(&base_url, bin_dir.path(), home_dir.path()).await;
        assert!(
            status.success(),
            "csm-watcher must exit successfully, got {status}"
        );

        wait_for(&sse, WATCHER_TIMEOUT, |sessions| {
            let count = sessions
                .iter()
                .filter(|s| s.session_id.starts_with("git-collapse-e2e-"))
                .count();
            (count == 2).then_some(())
        })
        .await;

        let invocations = std::fs::read_to_string(&log_path).unwrap_or_default();
        let invocation_count = invocations.lines().filter(|l| !l.trim().is_empty()).count();
        // Two sessions sharing one cwd, but two distinct subcommands (branch,
        // remote) each queried once per cwd - not once per session - so the
        // git binary must run exactly twice for this sweep, not four times.
        assert_eq!(
            invocation_count, 2,
            "expected git lookups to collapse across sessions sharing one cwd, got: \
             {invocations:?}"
        );

        handle.abort();
    }
}

/// PRO-210: the watcher as a daemon (`csm-watcher` run without `--once`).
///
/// Unlike every other test in this file, these drive the real binary as a
/// long-running process rather than a single invocation - starting it,
/// observing it sweep on its own schedule, and stopping it - so they live
/// separately from `run_watcher_once`'s single-shot callers above.
mod daemon {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use test_support::{locate_bin, sandbox_home, start_test_server, wait_for};

    use super::{registry_proc_start_for, write_registry_entry};

    /// Spawn the real `csm-watcher` binary in daemon mode (no `--once`),
    /// pointed at `base_url` with the given poll interval (a humantime
    /// duration string, e.g. `"2s"`/`"500ms"` - `--interval` takes the same
    /// shape PRO-212's launchd/systemd unit files bake in, replacing the
    /// pre-PRO-210-review `--interval-ms` raw-millisecond flag), using
    /// `CSM_WATCHER_REGISTRY_DIRS` the same way `run_watcher_once` does.
    ///
    /// Returns the live `tokio::process::Child` so the caller can signal and
    /// await it, alongside the `tempfile::TempDir` sandboxing its `$HOME`
    /// (see `run_watcher_once`'s doc comment above `daemon`'s `mod` block for
    /// why this is needed at all) - the caller must keep this alive for as
    /// long as the daemon might still be running/logging, typically by
    /// binding it to a `_`-prefixed variable that lives to the end of the
    /// test; dropping it early deletes the sandboxed `$HOME` (and therefore
    /// the daemon's log directory) out from under a still-running child.
    /// `kill_on_drop(true)` is a safety net only, in case a test panics
    /// before it gets the chance to send SIGTERM itself - it must not be
    /// relied on as the normal way this stops, since `Child::kill` sends
    /// SIGKILL on Unix, which is exactly the clean-stop path these tests
    /// exist to *not* rely on.
    fn spawn_watcher_daemon(
        base_url: &str,
        registry_dirs: &[&Path],
        interval: &str,
    ) -> (tokio::process::Child, tempfile::TempDir) {
        use tokio::process::Command;

        let joined = std::env::join_paths(registry_dirs.iter().map(|p| p.as_os_str()))
            .expect("join registry dirs");
        let home = sandbox_home();
        let child = Command::new(locate_bin("csm-watcher"))
            .arg("--interval")
            .arg(interval)
            .env("CLAUDE_MONITOR_URL", base_url)
            .env("CSM_WATCHER_REGISTRY_DIRS", joined)
            .env("HOME", home.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn csm-watcher daemon");
        (child, home)
    }

    /// Send a real SIGTERM (not `Child::kill`'s SIGKILL) to a running child.
    fn send_sigterm(child: &tokio::process::Child) {
        send_signal(child, libc::SIGTERM);
    }

    /// Send a real SIGINT (not `Child::kill`'s SIGKILL) to a running child.
    /// PRO-204's acceptance criteria name SIGINT explicitly alongside
    /// SIGTERM as a clean-stop signal the watcher must handle; before the
    /// PRO-210 review this file only actually exercised SIGTERM.
    fn send_sigint(child: &tokio::process::Child) {
        send_signal(child, libc::SIGINT);
    }

    fn send_signal(child: &tokio::process::Child, signal: libc::c_int) {
        let pid = child.id().expect("child has not already exited");
        // SAFETY: `pid` is a valid process id for a child this process just
        // spawned and has not yet reaped; `libc::kill` is a plain signal
        // send, not a memory operation.
        let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
        assert_eq!(
            rc,
            0,
            "libc::kill failed: {}",
            std::io::Error::last_os_error()
        );
    }

    const DAEMON_TIMEOUT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn daemon_sweeps_immediately_on_startup_and_exits_cleanly_on_sigterm() {
        let (base_url, handle) = start_test_server().await;
        let sse = common::sse::SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "daemon-immediate-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/daemon-immediate-1",
            None,
        );

        // A long interval, so that a session appearing well inside it can
        // only be explained by the first sweep running immediately on
        // startup rather than after waiting out one interval first (PRO-204
        // user story 27).
        let (mut child, _home) = spawn_watcher_daemon(&base_url, &[registry.path()], "60s");

        wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
            sessions
                .iter()
                .any(|s| s.session_id == "daemon-immediate-1")
                .then_some(())
        })
        .await;

        send_sigterm(&child);
        let status = tokio::time::timeout(DAEMON_TIMEOUT, child.wait())
            .await
            .expect("watcher daemon did not exit within the timeout after SIGTERM")
            .expect("failed to await watcher daemon");
        assert!(
            status.success(),
            "watcher daemon should exit cleanly (status 0) on SIGTERM, got {status}"
        );

        handle.abort();
    }

    /// Identical in shape to
    /// `daemon_sweeps_immediately_on_startup_and_exits_cleanly_on_sigterm`,
    /// but with SIGINT: PRO-204's acceptance criteria name SIGINT
    /// explicitly, alongside SIGTERM, as a signal the daemon must exit
    /// cleanly on (finding 9, PRO-210 review) - before this fix only
    /// SIGTERM was ever actually exercised by this file.
    #[tokio::test]
    async fn daemon_sweeps_immediately_on_startup_and_exits_cleanly_on_sigint() {
        let (base_url, handle) = start_test_server().await;
        let sse = common::sse::SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "daemon-immediate-sigint-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/daemon-immediate-sigint-1",
            None,
        );

        let (mut child, _home) = spawn_watcher_daemon(&base_url, &[registry.path()], "60s");

        wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
            sessions
                .iter()
                .any(|s| s.session_id == "daemon-immediate-sigint-1")
                .then_some(())
        })
        .await;

        send_sigint(&child);
        let status = tokio::time::timeout(DAEMON_TIMEOUT, child.wait())
            .await
            .expect("watcher daemon did not exit within the timeout after SIGINT")
            .expect("failed to await watcher daemon");
        assert!(
            status.success(),
            "watcher daemon should exit cleanly (status 0) on SIGINT, got {status}"
        );

        handle.abort();
    }

    /// Bind to an ephemeral port and immediately drop the listener, freeing
    /// the port back to the OS. This alone cannot guarantee nothing else
    /// claims the port before a caller gets to it - see
    /// `daemon_backs_off_against_an_unreachable_server_and_recovers_once_it_returns`,
    /// the only caller that rebinds a previously-reserved port later, for
    /// how it copes with that race (finding 10, PRO-210 review: a real, if
    /// narrow, source of flakiness in a parallel test suite, since another
    /// test's own `reserve_free_port` can win the window between this
    /// function's drop and a later rebind).
    async fn reserve_free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to random port");
        listener.local_addr().unwrap().port()
    }

    /// Like `test_support::start_test_server`, but binds the exact `port`
    /// given rather than an OS-chosen one - needed here so the daemon can be
    /// pointed at a server address *before* the server exists, then have a
    /// real server appear under it later without restarting the daemon.
    ///
    /// Returns `Err` rather than panicking on a bind failure (finding 10,
    /// PRO-210 review), so the caller can retry end-to-end with a fresh
    /// port instead of failing the whole test outright on what is usually a
    /// narrow, transient race rather than a real problem with the code
    /// under test.
    async fn start_test_server_on_port(port: u16) -> std::io::Result<tokio::task::JoinHandle<()>> {
        let conn = server::store::open_db(":memory:").expect("in-memory DB");
        let app = server::build_app(conn, None);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        Ok(tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server error");
        }))
    }

    /// Accept and immediately drop every TCP connection made to `port`,
    /// recording each accept's `Instant` into the returned shared `Vec`.
    ///
    /// This stands in for "the server is unreachable" in the backoff test
    /// below - dropping the connection without ever writing an HTTP
    /// response fails `publish`'s `reqwest` call (a reset or truncated-
    /// response error) exactly like a genuinely unreachable server would
    /// from the watcher's point of view, since `run_cycle` only
    /// distinguishes success from failure, never why. Unlike a plain
    /// unbound port, it gives the test a precise, jitter-free record of
    /// exactly when each failed publish attempt happened - what finding 9
    /// (PRO-210 review) needed: the previous version of that test only
    /// slept 400ms and asserted the daemon was still alive, which proves it
    /// did not crash but never actually observes the backoff widening the
    /// gap between attempts at all.
    ///
    /// Returns `Err` on a bind failure for the same reason
    /// `start_test_server_on_port` does - so the caller can retry with a
    /// fresh port rather than treat a narrow port-reuse race as a real
    /// failure.
    async fn spawn_connection_recorder(
        port: u16,
    ) -> std::io::Result<(tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Instant>>>)> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let timestamps = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&timestamps);
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        recorded.lock().unwrap().push(Instant::now());
                        drop(stream);
                    }
                    Err(_) => break,
                }
            }
        });
        Ok((handle, timestamps))
    }

    /// Poll `timestamps` until it holds at least `len` entries, or panic
    /// once `timeout` elapses.
    async fn wait_for_len(timestamps: &Arc<Mutex<Vec<Instant>>>, len: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if timestamps.lock().unwrap().len() >= len {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {timeout:?} waiting for {len} recorded connection attempts, \
                 got {}",
                timestamps.lock().unwrap().len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn daemon_backs_off_against_an_unreachable_server_and_recovers_once_it_returns() {
        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "daemon-backoff-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/daemon-backoff-1",
            None,
        );

        // Retries the whole reserve-port-through-rebind sequence with a
        // fresh port on a bind failure (finding 10, PRO-210 review) rather
        // than failing outright on what is normally a narrow, transient
        // race between `reserve_free_port` freeing a port and this test
        // rebinding it.
        const ATTEMPTS: usize = 5;
        let mut last_bind_err = None;
        for attempt in 0..ATTEMPTS {
            let port = reserve_free_port().await;
            let base_url = format!("http://127.0.0.1:{port}");

            let (recorder, timestamps) = match spawn_connection_recorder(port).await {
                Ok(pair) => pair,
                Err(e) if attempt + 1 < ATTEMPTS => {
                    last_bind_err = Some(e);
                    continue;
                }
                Err(e) => {
                    panic!("failed to bind the connection recorder after {ATTEMPTS} attempts: {e}")
                }
            };

            // MIN_INTERVAL itself (100ms) as the base, so a handful of
            // failed cycles - and their widening gaps - are observable well
            // within this test's budget.
            let (mut child, _home) = spawn_watcher_daemon(&base_url, &[registry.path()], "100ms");

            // Collect enough failed-publish timestamps to observe several
            // widening gaps: `Backoff::fail` doubles on each consecutive
            // failure (100ms, 200ms, 400ms, 800ms, ... - see its doc
            // comment), so 5 recorded attempts yield 4 gaps to compare.
            wait_for_len(&timestamps, 5, Duration::from_secs(5)).await;
            // `abort()` alone only *requests* cancellation - it returns
            // immediately, before the task (and therefore its `TcpListener`)
            // has actually been dropped, which raced `start_test_server_on_
            // port`'s later rebind of the same port below into a spurious
            // "Address already in use". Awaiting the handle blocks until the
            // task is actually torn down, so the port is genuinely free by
            // the time this function returns.
            recorder.abort();
            let _ = recorder.await;

            assert!(
                child
                    .try_wait()
                    .expect("failed to poll watcher daemon status")
                    .is_none(),
                "watcher daemon must not exit just because the server is unreachable; it should \
                 back off and keep retrying"
            );

            let observed: Vec<Instant> = timestamps.lock().unwrap().clone();
            let gaps: Vec<Duration> = observed
                .windows(2)
                .map(|w| w[1].duration_since(w[0]))
                .collect();
            assert!(
                gaps.len() >= 4,
                "expected at least 4 gaps between 5 recorded attempts, got {gaps:?}"
            );
            // Each gap should be at least roughly as long as the previous
            // one (some slack for scheduling jitter) - this is what
            // actually observes the backoff *widening*, rather than merely
            // asserting the daemon is still alive after a fixed sleep,
            // which is all the previous version of this test did.
            for pair in gaps.windows(2) {
                assert!(
                    pair[1] >= pair[0].mul_f64(0.7),
                    "expected gaps between failed publish attempts to widen (doubling backoff), \
                     got {gaps:?}"
                );
            }
            assert!(
                *gaps.last().unwrap() >= gaps.first().unwrap().mul_f64(2.0),
                "expected the backoff to have visibly grown between the first and last observed \
                 gap, got {gaps:?}"
            );

            let server_handle = match start_test_server_on_port(port).await {
                Ok(handle) => handle,
                Err(e) if attempt + 1 < ATTEMPTS => {
                    last_bind_err = Some(e);
                    continue;
                }
                Err(e) => {
                    panic!("failed to bind the real test server after {ATTEMPTS} attempts: {e}")
                }
            };
            let sse = common::sse::SseClient::new(&format!("{base_url}/api/events"));
            sse.start();

            // Generous relative to the short backoff above: proves recovery
            // happens on its own, without restarting the watcher, well
            // within a small number of retries rather than requiring a
            // tight race with exactly when the server started listening.
            wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
                sessions
                    .iter()
                    .any(|s| s.session_id == "daemon-backoff-1")
                    .then_some(())
            })
            .await;

            send_sigterm(&child);
            let status = tokio::time::timeout(DAEMON_TIMEOUT, child.wait())
                .await
                .expect("watcher daemon did not exit within the timeout after SIGTERM")
                .expect("failed to await watcher daemon");
            assert!(
                status.success(),
                "watcher daemon should exit cleanly (status 0) on SIGTERM, got {status}"
            );

            server_handle.abort();
            return;
        }

        panic!("exhausted all {ATTEMPTS} attempts, last bind error: {last_bind_err:?}");
    }

    // --- Cross-sweep debounce (PRO-211) ---
    //
    // `csm-watcher`'s daemon loop must not end a session the very first
    // sweep it is absent from a registry: only a session absent from two
    // *consecutive successful* sweeps is actually ended (`watcher::debounce`).
    // These drive the real daemon against a real registry directory, mutate
    // the registry file on disk between sweeps (exactly what removing a
    // session or an unreadable directory looks like from the daemon's own
    // point of view), and assert what a real SSE client observes - proving
    // the debounce end to end rather than against `Debounce::apply` directly.
    //
    // Sweeps are not directly observable, so these rely on a short, fixed
    // `--interval` and wall-clock timing to bracket "at least one sweep must
    // have happened by now" windows, the same technique
    // `daemon_backs_off_against_an_unreachable_server_and_recovers_once_it_returns`
    // above uses (there, via a connection-attempt recorder instead of direct
    // session assertions).
    const DEBOUNCE_INTERVAL: Duration = Duration::from_millis(300);

    #[tokio::test]
    async fn a_session_absent_from_one_sweep_survives_and_is_ended_only_after_a_second_consecutive_absence()
     {
        let (base_url, handle) = start_test_server().await;
        let sse = common::sse::SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        let registry = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            registry.path(),
            "entry.json",
            "debounce-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/debounce-1",
            None,
        );

        let (mut child, _home) = spawn_watcher_daemon(
            &base_url,
            &[registry.path()],
            "300ms", // matches DEBOUNCE_INTERVAL
        );

        // Sweep 0 (immediate, on startup): publishes the session.
        wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
            sessions
                .iter()
                .any(|s| s.session_id == "debounce-1")
                .then_some(())
        })
        .await;

        // Remove the registry entry - from the daemon's point of view this
        // is indistinguishable from the underlying `claude` process having
        // exited without cleaning up after itself in time.
        std::fs::remove_file(registry.path().join("sessions").join("entry.json"))
            .expect("remove registry entry");

        // By now sweep 1 (~300ms after sweep 0) must have already run and
        // found the session missing - but this is its *first* consecutive
        // absence, so the debounce must still republish it (frozen). Well
        // before sweep 2 (~600ms after sweep 0) would run.
        tokio::time::sleep(DEBOUNCE_INTERVAL + Duration::from_millis(150)).await;
        assert!(
            sse.sessions().iter().any(|s| s.session_id == "debounce-1"),
            "a session absent from exactly one sweep must still be published"
        );

        // Sweep 2 is now this session's *second* consecutive absence, so the
        // debounce must stop republishing it, and the server must end it.
        wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
            (!sessions.iter().any(|s| s.session_id == "debounce-1")).then_some(())
        })
        .await;

        send_sigterm(&child);
        let _ = tokio::time::timeout(DAEMON_TIMEOUT, child.wait()).await;

        handle.abort();
    }

    #[tokio::test]
    async fn a_failed_sweep_between_two_absences_does_not_advance_the_debounce() {
        let (base_url, handle) = start_test_server().await;
        let sse = common::sse::SseClient::new(&format!("{base_url}/api/events"));
        sse.start();

        // Two registry directories: `entry_dir` carries the session under
        // test; `breakable_dir` starts out empty and readable, and is made
        // unreadable partway through to force a whole-sweep failure without
        // ever touching `entry_dir`.
        let entry_dir = tempfile::tempdir().unwrap();
        let breakable_dir = tempfile::tempdir().unwrap();
        let breakable_sessions_dir = breakable_dir.path().join("sessions");
        std::fs::create_dir_all(&breakable_sessions_dir).unwrap();

        let pid = std::process::id();
        let proc_start = registry_proc_start_for(pid);
        write_registry_entry(
            entry_dir.path(),
            "entry.json",
            "debounce-failed-sweep-1",
            pid,
            &proc_start,
            "interactive",
            "busy",
            None,
            "/tmp/debounce-failed-sweep-1",
            None,
        );

        let (mut child, _home) = spawn_watcher_daemon(
            &base_url,
            &[entry_dir.path(), breakable_dir.path()],
            "300ms", // matches DEBOUNCE_INTERVAL
        );

        // Sweep 0: publishes the session.
        wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
            sessions
                .iter()
                .any(|s| s.session_id == "debounce-failed-sweep-1")
                .then_some(())
        })
        .await;

        std::fs::remove_file(entry_dir.path().join("sessions").join("entry.json"))
            .expect("remove registry entry");

        // Sweep 1 (~300ms after sweep 0): the session's first consecutive
        // absence, still successful overall (`breakable_dir` is still
        // readable) - still republished, frozen.
        tokio::time::sleep(DEBOUNCE_INTERVAL + Duration::from_millis(150)).await;
        assert!(
            sse.sessions()
                .iter()
                .any(|s| s.session_id == "debounce-failed-sweep-1"),
            "must survive its first consecutive absence"
        );

        // Break `breakable_dir` so the *next* sweep fails outright - this
        // must not be allowed to count as the session's second absence.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &breakable_sessions_dir,
                std::fs::Permissions::from_mode(0o000),
            )
            .expect("chmod breakable dir unreadable");
        }

        // Sweep 2 (~600ms after sweep 0) now fails outright. Wait past when
        // it must have run and past when a *third* sweep at the normal
        // cadence would have run (a failed sweep only triggers `Backoff`,
        // whose first `fail()` waits exactly one more base interval - see
        // `Backoff::fail`'s doc comment - so the next attempt lands at
        // roughly the same ~900ms mark a healthy cadence would have anyway).
        // The session must still be present throughout: a failed sweep
        // publishes nothing at all, so nothing about its state can change.
        tokio::time::sleep(DEBOUNCE_INTERVAL).await;
        assert!(
            sse.sessions()
                .iter()
                .any(|s| s.session_id == "debounce-failed-sweep-1"),
            "a failed sweep must not end, or otherwise progress the debounce for, a session \
             that was already in its one-sweep grace period"
        );

        // Restore `breakable_dir` so sweeps succeed again. The very next
        // successful sweep is the session's genuine *second* consecutive
        // absence (the failed sweep in between counted for nothing), so it
        // must now be ended.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &breakable_sessions_dir,
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("restore breakable dir permissions");
        }

        wait_for(&sse, DAEMON_TIMEOUT, |sessions| {
            (!sessions
                .iter()
                .any(|s| s.session_id == "debounce-failed-sweep-1"))
            .then_some(())
        })
        .await;

        send_sigterm(&child);
        let _ = tokio::time::timeout(DAEMON_TIMEOUT, child.wait()).await;

        handle.abort();
    }
}
