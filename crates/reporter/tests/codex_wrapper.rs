use std::time::Duration;

use common::sse::SseClient;
use test_support::{start_test_server, wait_for};

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
