//! Headless view-model for the session monitor UI.
//!
//! Owns SSE lifecycle, session store, delete-session HTTP, connection state,
//! and the derived [`MenuBarSummary`]. UI layers (egui, Swift/AppKit) consume
//! the state via the [`SessionObserver`] callback trait — they do not own
//! networking or config.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::api::{SessionView, resolve_server_url};
use crate::config;
use crate::session::{Status, WaitingReason};
use crate::sse::{SseClient, SseUpdateHandler};

/// Connection state to the coordination server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
}

/// Pre-computed summary for menu-bar style surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MenuBarSummary {
    pub waiting_input: u32,
    pub waiting_permission: u32,
    pub working: u32,
}

impl MenuBarSummary {
    pub fn from_sessions(sessions: &[SessionView]) -> Self {
        let mut s = Self::default();
        for session in sessions {
            match &session.status {
                Status::Waiting(w) => match w.reason {
                    WaitingReason::Input => s.waiting_input += 1,
                    WaitingReason::Permission => s.waiting_permission += 1,
                },
                Status::Working(_) => s.working += 1,
                Status::Ended => {}
            }
        }
        s
    }
}

/// Callback interface for UI layers that want push-style updates.
///
/// Implementations must be cheap and non-blocking — callbacks fire on the SSE
/// thread.
pub trait SessionObserver: Send + Sync {
    fn on_sessions_changed(&self, sessions: Vec<SessionView>);
    fn on_connection_changed(&self, state: ConnectionState);
    fn on_summary_changed(&self, summary: MenuBarSummary);
}

struct SharedState {
    sessions: Vec<SessionView>,
    connection: ConnectionState,
    summary: MenuBarSummary,
    observers: HashMap<u64, Arc<dyn SessionObserver>>,
    next_id: u64,
}

struct CoreInner {
    server_url: String,
    shared: Mutex<SharedState>,
}

/// Handle to the headless core. Cheap to clone.
#[derive(Clone)]
pub struct CoreHandle {
    inner: Arc<CoreInner>,
    _sse: Arc<SseClient>,
}

/// RAII handle from [`CoreHandle::subscribe`]. Dropping it detaches the observer.
pub struct SubscriptionHandle {
    id: u64,
    inner: Weak<CoreInner>,
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.shared.lock().unwrap().observers.remove(&self.id);
        }
    }
}

impl CoreHandle {
    /// Construct a new core.
    ///
    /// `cli_server_url` overrides the config file value. Config is loaded from
    /// the platform default location. On config-load failure the error is
    /// logged and the compiled-in default URL is used.
    pub fn new(cli_server_url: Option<String>) -> Self {
        let file_url = match config::load() {
            Ok(c) => Some(c.server.url),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load config; using default server URL");
                None
            }
        };
        let server_url = resolve_server_url(cli_server_url.as_deref(), file_url.as_deref());
        Self::with_server_url(server_url)
    }

    /// Construct a core that bypasses config loading. Intended for tests and
    /// embeddings where the caller already has a URL.
    pub fn with_server_url(server_url: String) -> Self {
        let sse_url = format!("{}/api/events", server_url);
        tracing::info!(server_url, sse_url, "connecting to server");

        let inner = Arc::new(CoreInner {
            server_url,
            shared: Mutex::new(SharedState {
                sessions: Vec::new(),
                connection: ConnectionState::Connecting,
                summary: MenuBarSummary::default(),
                observers: HashMap::new(),
                next_id: 0,
            }),
        });

        let sse = Arc::new(SseClient::new(&sse_url));
        sse.set_handler(Arc::new(Bridge {
            inner: Arc::clone(&inner),
        }));
        sse.start();

        Self { inner, _sse: sse }
    }

    /// Current server URL. Useful for logging and for building REST URLs.
    pub fn server_url(&self) -> &str {
        &self.inner.server_url
    }

    /// Subscribe for push updates. The observer is notified immediately with
    /// the current snapshot so subscribers don't need to poll.
    pub fn subscribe(&self, observer: Arc<dyn SessionObserver>) -> SubscriptionHandle {
        let (id, sessions, state, summary) = {
            let mut s = self.inner.shared.lock().unwrap();
            let id = s.next_id;
            s.next_id += 1;
            s.observers.insert(id, Arc::clone(&observer));
            (id, s.sessions.clone(), s.connection, s.summary)
        };
        observer.on_sessions_changed(sessions);
        observer.on_connection_changed(state);
        observer.on_summary_changed(summary);

        SubscriptionHandle {
            id,
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Snapshot of current sessions. Prefer [`subscribe`] for UI; this is for
    /// pull-based renderers (e.g. egui).
    pub fn sessions(&self) -> Vec<SessionView> {
        self.inner.shared.lock().unwrap().sessions.clone()
    }

    /// Current connection state snapshot.
    pub fn connection_state(&self) -> ConnectionState {
        self.inner.shared.lock().unwrap().connection
    }

    /// Current menu-bar summary snapshot.
    pub fn summary(&self) -> MenuBarSummary {
        self.inner.shared.lock().unwrap().summary
    }

    /// Delete a session. Fires off an HTTP request on a background thread;
    /// errors are logged.
    pub fn delete_session(&self, session_id: String) {
        let url = format!("{}/api/sessions/{}", self.inner.server_url, session_id);
        tracing::info!(session_id, "deleting session");
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            match client.delete(&url).send() {
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                    tracing::warn!(session_id, "session not found for deletion");
                }
                Ok(resp) if !resp.status().is_success() => {
                    tracing::error!(session_id, status = %resp.status(), "delete session failed");
                }
                Err(e) => {
                    tracing::error!(error = %e, "delete request error");
                }
                Ok(_) => {
                    tracing::debug!(session_id, "session deleted successfully");
                }
            }
        });
    }
}

struct Bridge {
    inner: Arc<CoreInner>,
}

impl SseUpdateHandler for Bridge {
    fn on_update(&self, sessions: Vec<SessionView>, connected: bool) {
        let new_connection = if connected {
            ConnectionState::Connected
        } else {
            ConnectionState::Disconnected
        };
        let new_summary = MenuBarSummary::from_sessions(&sessions);

        let (sessions_changed, connection_changed, summary_changed, observers) = {
            let mut s = self.inner.shared.lock().unwrap();
            let sessions_changed = s.sessions != sessions;
            let connection_changed = s.connection != new_connection;
            let summary_changed = s.summary != new_summary;
            if sessions_changed {
                s.sessions = sessions.clone();
            }
            if connection_changed {
                s.connection = new_connection;
            }
            if summary_changed {
                s.summary = new_summary;
            }
            let observers: Vec<_> = s.observers.values().cloned().collect();
            (
                sessions_changed,
                connection_changed,
                summary_changed,
                observers,
            )
        };

        for observer in observers {
            if sessions_changed {
                observer.on_sessions_changed(sessions.clone());
            }
            if connection_changed {
                observer.on_connection_changed(new_connection);
            }
            if summary_changed {
                observer.on_summary_changed(new_summary);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::AgentKind;
    use crate::session::{WaitingStatus, WorkingStatus};
    use chrono::Utc;

    fn session(id: &str, status: Status) -> SessionView {
        SessionView {
            session_id: id.into(),
            cwd: "/tmp".into(),
            status,
            agent_kind: AgentKind::Claude,
            model: None,
            updated_at: Utc::now(),
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        }
    }

    #[test]
    fn summary_counts_each_status_kind() {
        let sessions = vec![
            session(
                "a",
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Input,
                    detail: None,
                }),
            ),
            session(
                "b",
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Permission,
                    detail: None,
                }),
            ),
            session("c", Status::Working(WorkingStatus { tool: None })),
            session("d", Status::Working(WorkingStatus { tool: None })),
            session("e", Status::Ended),
        ];
        let summary = MenuBarSummary::from_sessions(&sessions);
        assert_eq!(summary.waiting_input, 1);
        assert_eq!(summary.waiting_permission, 1);
        assert_eq!(summary.working, 2);
    }

    #[test]
    fn empty_sessions_gives_zero_summary() {
        assert_eq!(
            MenuBarSummary::from_sessions(&[]),
            MenuBarSummary::default()
        );
    }
}
