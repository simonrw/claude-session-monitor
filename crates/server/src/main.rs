use server::store;

#[tokio::main]
async fn main() {
    let conn = {
        let path = std::env::var("CLAUDE_MONITOR_DB").unwrap_or_else(|_| {
            let data_dir = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            format!("{data_dir}/claude-session-monitor.db")
        });
        store::open_db(&path).expect("failed to open database")
    };

    let app = server::build_app(conn);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:7685")
        .await
        .expect("failed to bind");

    axum::serve(listener, app).await.expect("server error");
}
