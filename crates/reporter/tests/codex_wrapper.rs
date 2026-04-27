use std::time::{Duration, Instant};

use common::api::SessionView;
use common::sse::SseClient;
use tokio::task::JoinHandle;

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

async fn wait_for<F, T>(sse: &SseClient, timeout: Duration, mut predicate: F) -> T
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn codex_wrapper_ends_recorded_session_when_child_exits() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::process::Command;

    let (base_url, handle) = start_test_server().await;
    let sse = SseClient::new(&format!("{base_url}/api/events"));
    sse.start();

    let temp = tempfile::tempdir().expect("tempdir");
    let fake_codex = temp.path().join("fake-codex");
    std::fs::write(
        &fake_codex,
        r#"#!/bin/sh
printf '%s' '{"session_id":"wrapped-codex","cwd":"/tmp","hook_event_name":"SessionStart","model":"gpt-5.1-codex"}' | "$CSM_REPORTER_BIN" --agent codex
sleep 1
exit 7
"#,
    )
    .expect("write fake codex");
    let mut perms = std::fs::metadata(&fake_codex)
        .expect("fake codex metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_codex, perms).expect("chmod fake codex");

    let mut child = Command::new(env!("CARGO_BIN_EXE_csm-codex"))
        .arg("--codex-bin")
        .arg(&fake_codex)
        .env("CLAUDE_MONITOR_URL", &base_url)
        .env("CSM_REPORTER_BIN", env!("CARGO_BIN_EXE_csm-reporter"))
        .env("CSM_CODEX_RUN_STATE_DIR", temp.path().join("run-state"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn csm-codex");

    wait_for(&sse, Duration::from_secs(5), |sessions| {
        sessions
            .iter()
            .find(|s| s.session_id == "wrapped-codex")
            .cloned()
    })
    .await;

    let status = child.wait().await.expect("wait csm-codex");
    assert_eq!(status.code(), Some(7));

    wait_for(&sse, Duration::from_secs(5), |sessions| {
        sessions
            .iter()
            .all(|s| s.session_id != "wrapped-codex")
            .then_some(())
    })
    .await;

    handle.abort();
}
