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

/// POST `sessions` as one snapshot for the local host to `server_url`, using
/// `client`.
///
/// `client` is built once by the caller (`main.rs`) and reused across every
/// cycle of the daemon loop, rather than constructed fresh here on every
/// call: a new `reqwest::blocking::Client` builds a new runtime thread and a
/// new TCP connection each time, so building one per publish defeats
/// keep-alive at a poll interval that can be as tight as a couple of
/// seconds. See `main.rs`'s `build_http_client` for why the client also
/// carries an explicit request timeout rather than `reqwest`'s 30-second
/// default.
///
/// The host is resolved via `common::hostname::resolve()`; if it can't be
/// determined this fails with `PublishError::NoHostname` rather than
/// publishing under some placeholder, since the server scopes snapshot
/// reconciliation by hostname - publishing under the wrong host would let
/// this snapshot end sessions it doesn't own.
pub fn publish(
    client: &reqwest::blocking::Client,
    server_url: &str,
    agent_kind: AgentKind,
    sessions: Vec<SnapshotSession>,
) -> Result<(), PublishError> {
    let hostname = common::hostname::resolve().ok_or(PublishError::NoHostname)?;
    let payload = SnapshotPayload {
        agent_kind,
        observed_at: chrono::Utc::now(),
        sessions,
    };
    let url = format!("{server_url}/api/hosts/{hostname}/sessions");
    tracing::debug!(url = %url, session_count = payload.sessions.len(), "publishing snapshot");
    let resp = client.post(&url).json(&payload).send()?;
    if !resp.status().is_success() {
        return Err(PublishError::Rejected {
            status: resp.status(),
        });
    }
    Ok(())
}
