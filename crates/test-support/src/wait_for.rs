//! Awaiting an SSE snapshot with a timeout.

use std::time::{Duration, Instant};

use common::api::SessionView;
use common::sse::SseClient;

/// Poll `SseClient::sessions()` every 50ms until `predicate` returns `Some(T)`,
/// or panic with a timeout message after `timeout`.
///
/// `SseClient::start` runs the SSE stream on its own OS thread (not the
/// tokio runtime), so this can safely be driven from any async test.
pub async fn wait_for<F, T>(sse: &SseClient, timeout: Duration, mut predicate: F) -> T
where
    F: FnMut(&[SessionView]) -> Option<T>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let sessions = sse.sessions();
        if let Some(result) = predicate(&sessions) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "timeout after {timeout:?}; last sessions: {sessions:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
