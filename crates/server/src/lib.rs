pub mod error;
pub mod store;

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, post};
use common::api::{ReportPayload, SessionView};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::trace::TraceLayer;

use error::AppError;
use store::SessionStore;

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<rusqlite::Connection>>,
    tx: broadcast::Sender<Vec<SessionView>>,
}

pub fn build_app(conn: rusqlite::Connection) -> Router {
    let (tx, _) = broadcast::channel(64);
    let state = AppState {
        store: Arc::new(Mutex::new(conn)),
        tx,
    };
    Router::new()
        .route("/api/sessions", post(post_session))
        .route("/api/sessions/{session_id}", delete(delete_session))
        .route("/api/events", get(get_events))
        .route("/api/health", get(get_health))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn get_health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let conn = state.store.lock().map_err(|_| AppError::LockPoisoned)?;
    let found = conn.delete_session(&session_id)?;
    if !found {
        tracing::debug!(session_id, "session not found for deletion");
        return Ok(StatusCode::NOT_FOUND);
    }
    let sessions = conn.list_active_sessions()?;
    drop(conn);
    tracing::debug!(
        session_id,
        session_count = sessions.len(),
        "deleted session, broadcasting update"
    );
    // A broadcast send only fails when there are no receivers; that's not an
    // error condition for the server, so we swallow it here.
    let _ = state.tx.send(sessions);
    Ok(StatusCode::NO_CONTENT)
}

async fn post_session(
    State(state): State<AppState>,
    Json(payload): Json<ReportPayload>,
) -> Result<StatusCode, AppError> {
    tracing::debug!(
        session_id = payload.session_id,
        status = ?payload.status,
        "upserting session"
    );
    let conn = state.store.lock().map_err(|_| AppError::LockPoisoned)?;
    conn.upsert_session(&payload)?;
    let sessions = conn.list_active_sessions()?;
    drop(conn);
    tracing::debug!(
        session_count = sessions.len(),
        "broadcasting session update"
    );
    // See note in delete_session: no receivers is not an error.
    let _ = state.tx.send(sessions);
    Ok(StatusCode::NO_CONTENT)
}

async fn get_events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    tracing::debug!("SSE client subscribed");
    let current = {
        let conn = match state.store.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
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
