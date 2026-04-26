//! Tests for static file serving and SPA fallback.

use std::io::Write;
use std::path::Path;

use tokio::task::JoinHandle;

async fn start_server_with_static_dir(static_dir: &Path) -> (String, JoinHandle<()>) {
    let conn = server::store::open_db(":memory:").expect("in-memory DB");
    let app = server::build_app(conn, Some(static_dir.to_path_buf()));
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
async fn api_routes_work_with_static_dir_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base_url, handle) = start_server_with_static_dir(dir.path()).await;

    let resp = reqwest::get(format!("{base_url}/api/health"))
        .await
        .expect("GET /api/health");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["status"], "ok");

    handle.abort();
}

#[tokio::test]
async fn serves_static_file_from_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut f = std::fs::File::create(dir.path().join("test.txt")).expect("create file");
    f.write_all(b"hello from static").expect("write");
    drop(f);

    let (base_url, handle) = start_server_with_static_dir(dir.path()).await;

    let resp = reqwest::get(format!("{base_url}/test.txt"))
        .await
        .expect("GET /test.txt");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.expect("body text");
    assert_eq!(body, "hello from static");

    handle.abort();
}

#[tokio::test]
async fn spa_fallback_returns_index_html() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut f = std::fs::File::create(dir.path().join("index.html")).expect("create file");
    f.write_all(b"<html>spa</html>").expect("write");
    drop(f);

    let (base_url, handle) = start_server_with_static_dir(dir.path()).await;

    let resp = reqwest::get(format!("{base_url}/some/random/path"))
        .await
        .expect("GET /some/random/path");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.expect("body text");
    assert_eq!(body, "<html>spa</html>");

    handle.abort();
}

#[tokio::test]
async fn api_takes_precedence_over_static_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("api")).expect("mkdir api");
    let mut f = std::fs::File::create(dir.path().join("api/health")).expect("create file");
    f.write_all(b"not the api").expect("write");
    drop(f);

    let (base_url, handle) = start_server_with_static_dir(dir.path()).await;

    let resp = reqwest::get(format!("{base_url}/api/health"))
        .await
        .expect("GET /api/health");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["status"], "ok");

    handle.abort();
}
