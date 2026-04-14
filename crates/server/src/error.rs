//! Axum error type for server route handlers.
//!
//! Handlers return `Result<T, AppError>`. `AppError` implements `IntoResponse`
//! producing HTTP 500 with a short body and captures the error in Sentry
//! exactly once. `From` impls convert common error sources so handlers can
//! use `?` directly.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use common::api::SessionView;
use tokio::sync::broadcast;

/// Unified error type for server handlers. Any variant becomes a 500 response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("broadcast send failed: {0}")]
    Broadcast(#[from] broadcast::error::SendError<Vec<SessionView>>),

    #[error("lock poisoned")]
    LockPoisoned,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self, "handler returned error; responding 500");
        common::sentry::capture_error(&self);
        (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
    }
}
