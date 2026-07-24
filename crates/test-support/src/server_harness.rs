//! Starting a real server for integration tests to talk to.

use tokio::task::JoinHandle;

/// Start a real server on a random localhost port, backed by an in-memory
/// SQLite database and with no static file directory configured.
///
/// Returns the server's base URL and a handle to its serving task. Call
/// `handle.abort()` when the test is done with it.
pub async fn start_test_server() -> (String, JoinHandle<()>) {
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
