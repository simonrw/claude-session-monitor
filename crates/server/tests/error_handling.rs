//! Tests that verify server route handlers return HTTP 500 (instead of
//! panicking) when the underlying DB operation fails.

use tokio::task::JoinHandle;

/// Start a server whose DB is missing the `sessions` table. Every route that
/// touches the DB will therefore fail, which is exactly what we want to
/// exercise the `AppError` → 500 conversion.
async fn start_broken_server() -> (String, JoinHandle<()>) {
    let conn = server::store::open_db(":memory:").expect("in-memory DB");
    // Drop the sessions table so subsequent queries fail with a rusqlite error.
    conn.execute("DROP TABLE sessions", []).expect("drop table");

    let app = server::build_app(conn);
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

#[tokio::test]
async fn post_session_returns_500_when_db_broken() {
    let (base_url, handle) = start_broken_server().await;

    // Full payload matching ReportPayload — required so the request passes
    // deserialization and actually hits the DB layer.
    let payload = serde_json::json!({
        "session_id": "err-1",
        "cwd": "/tmp",
        "status": { "type": "working", "tool": null },
        "hook_event_name": "SessionStart",
        "tool_name": null,
        "tool_input": null,
        "notification_type": null,
        "hostname": null,
        "git_branch": null,
        "git_remote": null
    });

    let resp = reqwest::Client::new()
        .post(format!("{base_url}/api/sessions"))
        .json(&payload)
        .send()
        .await
        .expect("POST /api/sessions");

    assert_eq!(resp.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

    // Server must still be alive: health endpoint must still respond.
    let health = reqwest::get(format!("{base_url}/api/health"))
        .await
        .expect("GET /api/health after failure");
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    handle.abort();
}

#[tokio::test]
async fn delete_session_returns_500_when_db_broken() {
    let (base_url, handle) = start_broken_server().await;

    let resp = reqwest::Client::new()
        .delete(format!("{base_url}/api/sessions/err-2"))
        .send()
        .await
        .expect("DELETE /api/sessions/err-2");

    assert_eq!(resp.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

    // Server must still be alive.
    let health = reqwest::get(format!("{base_url}/api/health"))
        .await
        .expect("GET /api/health after failure");
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    handle.abort();
}
