//! Publishing a sweep's survivors to the coordination server as one
//! snapshot: `POST /api/hosts/{hostname}/sessions`.

use common::api::{AgentKind, SnapshotPayload, SnapshotSession};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("hostname could not be determined")]
    NoHostname,
    #[error("failed to send snapshot: {0}")]
    Request(#[from] reqwest::Error),
    #[error("server rejected snapshot: {status}")]
    Rejected { status: reqwest::StatusCode },
}

/// POST `sessions` as one snapshot for the local host to `server_url`.
///
/// The host is resolved via `common::hostname::resolve()`; if it can't be
/// determined this fails with `PublishError::NoHostname` rather than
/// publishing under some placeholder, since the server scopes snapshot
/// reconciliation by hostname - publishing under the wrong host would let
/// this snapshot end sessions it doesn't own.
pub fn publish(server_url: &str, sessions: Vec<SnapshotSession>) -> Result<(), PublishError> {
    let hostname = common::hostname::resolve().ok_or(PublishError::NoHostname)?;
    let payload = SnapshotPayload {
        agent_kind: AgentKind::Claude,
        observed_at: chrono::Utc::now(),
        sessions,
    };
    let url = format!("{server_url}/api/hosts/{hostname}/sessions");
    tracing::debug!(url = %url, session_count = payload.sessions.len(), "publishing snapshot");
    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .json(&payload)
        .send()?;
    if !resp.status().is_success() {
        return Err(PublishError::Rejected {
            status: resp.status(),
        });
    }
    Ok(())
}
