mod store;

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use common::api::{ReportPayload, SessionView};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use store::SessionStore;

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<rusqlite::Connection>>,
    tx: broadcast::Sender<Vec<SessionView>>,
}

#[tokio::main]
async fn main() {
    let conn = {
        let path = std::env::var("CLAUDE_MONITOR_DB").unwrap_or_else(|_| {
            let data_dir = dirs_home();
            format!("{data_dir}/claude-session-monitor.db")
        });
        store::open_db(&path).expect("failed to open database")
    };

    let (tx, _) = broadcast::channel(64);

    let state = AppState {
        store: Arc::new(Mutex::new(conn)),
        tx,
    };

    let app = Router::new()
        .route("/api/sessions", post(post_session))
        .route("/api/events", get(get_events))
        .route("/api/health", get(get_health))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:7685")
        .await
        .expect("failed to bind");

    axum::serve(listener, app).await.expect("server error");
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

async fn get_health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn post_session(
    State(state): State<AppState>,
    Json(payload): Json<ReportPayload>,
) -> impl IntoResponse {
    let conn = state.store.lock().unwrap();
    conn.upsert_session(&payload).expect("upsert failed");
    let sessions = conn.list_active_sessions().expect("list failed");
    drop(conn);
    let _ = state.tx.send(sessions);
    axum::http::StatusCode::NO_CONTENT
}

async fn get_events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let current = {
        let conn = state.store.lock().unwrap();
        conn.list_active_sessions().unwrap_or_default()
    };

    let rx = state.tx.subscribe();
    let broadcast_stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(sessions) => Some(Ok(sessions)),
        Err(_) => None,
    });

    let initial = tokio_stream::once(Ok(current));
    let combined = initial.chain(broadcast_stream);

    let event_stream = combined.map(|result: Result<Vec<SessionView>, Infallible>| {
        let sessions = result.unwrap();
        let data = serde_json::to_string(&sessions).unwrap_or_else(|_| "[]".into());
        Ok::<Event, Infallible>(Event::default().data(data))
    });

    Sse::new(event_stream).keep_alive(KeepAlive::default())
}
