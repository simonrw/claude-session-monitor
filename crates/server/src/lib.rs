pub mod error;
pub mod store;

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, post};
use common::api::{HostStatus, ReportPayload, SessionView, SnapshotPayload};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use error::AppError;
use store::SessionStore;

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<rusqlite::Connection>>,
    tx: broadcast::Sender<Vec<SessionView>>,
}

pub fn build_app(conn: rusqlite::Connection, static_dir: Option<PathBuf>) -> Router {
    let (tx, _) = broadcast::channel(64);
    let state = AppState {
        store: Arc::new(Mutex::new(conn)),
        tx,
    };
    let api = Router::new()
        .route("/api/sessions", post(post_session))
        .route("/api/sessions/{session_id}/end", post(end_session))
        .route("/api/sessions/{session_id}", delete(delete_session))
        .route("/api/hosts/{hostname}/sessions", post(post_snapshot))
        .route("/api/hosts", get(get_hosts))
        .route("/api/events", get(get_events))
        .route("/api/health", get(get_health));

    let router = if let Some(dir) = static_dir {
        let serve = ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")));
        api.fallback_service(serve)
    } else {
        api
    };

    router.layer(TraceLayer::new_for_http()).with_state(state)
}

async fn get_health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
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

async fn end_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let conn = state.store.lock().map_err(|_| AppError::LockPoisoned)?;
    let found = conn.end_session(&session_id)?;
    if !found {
        tracing::debug!(session_id, "session not found for end");
        return Ok(StatusCode::NOT_FOUND);
    }
    let sessions = conn.list_active_sessions()?;
    drop(conn);
    tracing::debug!(
        session_id,
        session_count = sessions.len(),
        "ended session, broadcasting update"
    );
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

/// `POST /api/hosts/{hostname}/sessions` - a host publishes the complete set
/// of sessions it currently observes for one agent kind.
///
/// The server's view of that host's sessions for that agent kind is
/// reconciled to match the snapshot exactly: every session in it is
/// upserted, and every session absent from it is ended. Rows with a null
/// hostname, rows belonging to another host, and rows of another agent kind
/// are never touched, so this is safe to call from a single host mid
/// rollout.
///
/// A broadcast only fires when the snapshot actually changed a row, so an
/// idle watcher republishing the same snapshot generates no SSE traffic.
async fn post_snapshot(
    State(state): State<AppState>,
    Path(hostname): Path<String>,
    Json(payload): Json<SnapshotPayload>,
) -> Result<StatusCode, AppError> {
    tracing::debug!(
        hostname,
        agent_kind = ?payload.agent_kind,
        session_count = payload.sessions.len(),
        "applying snapshot"
    );
    let conn = state.store.lock().map_err(|_| AppError::LockPoisoned)?;
    let changed = conn.apply_snapshot(&hostname, payload.agent_kind, &payload.sessions)?;
    // Recorded unconditionally - whether or not the snapshot changed a row,
    // and whether or not it contained any sessions - so a client can later
    // tell "this host has zero live sessions" apart from "this host's
    // watcher has stopped reporting" (PRO-211; see `HostStatus`'s doc
    // comment). This is deliberately separate from, and does not alter,
    // `apply_snapshot`'s own reconciliation logic or its `changed` result.
    //
    // Not throttled (PRO-211 review, finding 5), even though this means an
    // unchanged republish at the default 2s interval still runs one upsert
    // write transaction per POST - on the order of 43,000 writes/day per
    // host. Left alone deliberately rather than guessed at: this is a local
    // SQLite write inside a request already doing one (`apply_snapshot`'s
    // own transaction, just above), there is no broadcast or other
    // observable cost riding on it (verified: only `apply_snapshot`'s
    // `changed` result drives the broadcast below, and this call cannot
    // affect that), and there is no consumer of `list_host_status` yet to
    // measure a real staleness/write-cost trade-off against - PRO-214 is
    // that consumer. Throttling now would be tuning a cost against a UI
    // that doesn't exist; revisit once PRO-214 defines how fresh
    // `last_seen_at` actually needs to be.
    conn.record_host_seen(&hostname, payload.agent_kind)?;
    if !changed {
        tracing::debug!(hostname, "snapshot changed nothing, not broadcasting");
        return Ok(StatusCode::NO_CONTENT);
    }
    let sessions = conn.list_active_sessions()?;
    drop(conn);
    tracing::debug!(
        session_count = sessions.len(),
        "broadcasting session update after snapshot"
    );
    let _ = state.tx.send(sessions);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/hosts` - the last-accepted-snapshot time for every host and
/// agent kind that has ever published one, most recently seen first.
///
/// See [`HostStatus`]'s doc comment for why this exists: it is the piece of
/// information a client needs to distinguish "this host genuinely has no
/// live sessions" from "this host's watcher has stopped reporting", which
/// `SessionView`'s empty-list shape cannot express by itself.
async fn get_hosts(State(state): State<AppState>) -> Result<Json<Vec<HostStatus>>, AppError> {
    let conn = state.store.lock().map_err(|_| AppError::LockPoisoned)?;
    let statuses = conn.list_host_status()?;
    Ok(Json(statuses))
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
