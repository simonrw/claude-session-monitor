//! Headless view-model for the session monitor UI.
//!
//! Owns SSE lifecycle, session store, delete-session HTTP, connection state,
//! and the derived [`MenuBarSummary`]. UI layers (egui, Swift/AppKit) consume
//! the state via the [`SessionObserver`] callback trait — they do not own
//! networking or config.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::api::{HostStatus, SessionView, resolve_server_url};
use crate::config;
use crate::session::Status;
use crate::sse::{SseClient, SseUpdateHandler};

/// Connection state to the coordination server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
}

/// Pre-computed summary for menu-bar style surfaces.
///
/// `busy` counts sessions actively doing something: [`Status::Busy`] (Codex's
/// per-tool `tool` is irrelevant here, only its presence counts) and
/// [`Status::Shell`] both count - a foreground shell command is exactly as
/// much "doing something" as a tool call or the model thinking, and nothing
/// about it needs the user's attention any more than either does, so it
/// belongs with `busy` rather than vanishing from the summary. [`Status::Idle`]
/// does not count as busy: the turn has finished and the session is sitting
/// at the prompt, doing nothing - that is the whole distinction PRO-214
/// introduced `Idle` to make.
///
/// `waiting` counts [`Status::Waiting`] - blocked on the user - regardless of
/// `detail`. The previous Permission/Input split (`waiting_input` +
/// `waiting_permission`) is gone along with `WaitingReason`: the registry
/// carries no structured signal distinguishing the two, so keeping two
/// counts here would still just be guessing dressed up as two numbers
/// instead of one. See `common::session::Status`'s doc comment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MenuBarSummary {
    pub busy: u32,
    pub waiting: u32,
}

impl MenuBarSummary {
    pub fn from_sessions(sessions: &[SessionView]) -> Self {
        let mut s = Self::default();
        for session in sessions {
            match &session.status {
                Status::Busy { .. } | Status::Shell => s.busy += 1,
                Status::Waiting { .. } => s.waiting += 1,
                Status::Idle | Status::Ended => {}
            }
        }
        s
    }
}

/// Callback interface for UI layers that want push-style updates.
///
/// Implementations must be cheap and non-blocking — callbacks fire on the SSE
/// thread (`on_sessions_changed`/`on_connection_changed`/`on_summary_changed`)
/// or the host-status poll thread (`on_host_status_changed`).
pub trait SessionObserver: Send + Sync {
    fn on_sessions_changed(&self, sessions: Vec<SessionView>);
    fn on_connection_changed(&self, state: ConnectionState);
    fn on_summary_changed(&self, summary: MenuBarSummary);

    /// Fires on **every** successful `GET /api/hosts` poll, whether or not
    /// anything changed (see [`CoreHandle`]'s host-status poll thread), and
    /// once immediately on [`CoreHandle::subscribe`] with whatever is already
    /// known.
    ///
    /// Firing unconditionally is deliberate and load-bearing. A watcher that
    /// died right after its last publish freezes `last_seen_at`, so the polled
    /// value stops changing at exactly the moment a client most needs to
    /// re-check whether it has gone stale. Some clients (the mac popover) have
    /// no other ticker and re-render solely because of this callback.
    ///
    /// This is what lets a client distinguish "this host genuinely has zero
    /// live sessions right now" from "no watcher has ever reported for this
    /// host at all" (PRO-211's `HostStatus`, inherited into PRO-214's
    /// acceptance criteria) - an empty `sessions()`/`on_sessions_changed`
    /// list alone cannot tell those apart. A default no-op implementation is
    /// provided so existing observers keep compiling without adopting this
    /// immediately.
    fn on_host_status_changed(&self, _hosts: Vec<HostStatus>) {}
}

struct SharedState {
    sessions: Vec<SessionView>,
    connection: ConnectionState,
    summary: MenuBarSummary,
    hosts: Vec<HostStatus>,
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

/// How often the background thread polls `GET /api/hosts`. This is purely
/// about detecting "a watcher has never reported" / "a watcher has gone
/// silent", not about session freshness (SSE already pushes that instantly),
/// so a slow poll is fine - see [`CoreHandle::with_server_url`].
const HOST_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Timeout for each individual `GET /api/hosts` poll request, so a hung
/// connection can't wedge the poll thread forever - the same defect noted on
/// `delete_session`'s client below, avoided here from the start.
const HOST_STATUS_POLL_TIMEOUT: Duration = Duration::from_secs(5);

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
                hosts: Vec::new(),
                observers: HashMap::new(),
                next_id: 0,
            }),
        });

        let sse = Arc::new(SseClient::new(&sse_url));
        sse.set_handler(Arc::new(Bridge {
            inner: Arc::clone(&inner),
        }));
        sse.start();

        spawn_host_status_poller(Arc::clone(&inner));

        Self { inner, _sse: sse }
    }

    /// Current server URL. Useful for logging and for building REST URLs.
    pub fn server_url(&self) -> &str {
        &self.inner.server_url
    }

    /// Subscribe for push updates. The observer is notified immediately with
    /// the current snapshot so subscribers don't need to poll.
    pub fn subscribe(&self, observer: Arc<dyn SessionObserver>) -> SubscriptionHandle {
        let (id, sessions, state, summary, hosts) = {
            let mut s = self.inner.shared.lock().unwrap();
            let id = s.next_id;
            s.next_id += 1;
            s.observers.insert(id, Arc::clone(&observer));
            (
                id,
                s.sessions.clone(),
                s.connection,
                s.summary,
                s.hosts.clone(),
            )
        };
        observer.on_sessions_changed(sessions);
        observer.on_connection_changed(state);
        observer.on_summary_changed(summary);
        observer.on_host_status_changed(hosts);

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

/// Background thread polling `GET /api/hosts` on [`HOST_STATUS_POLL_INTERVAL`],
/// feeding `SharedState::hosts` and `SessionObserver::on_host_status_changed`.
///
/// This is deliberately a poll, not a push: unlike sessions (which the
/// server broadcasts over SSE the instant they change), `GET /api/hosts` has
/// no broadcast channel, and adding one purely to detect "has this watcher
/// gone silent" - an inherently time-based question, not an edit to react to
/// - would be a bigger, less honest change than a slow poll.
///
/// Every successful poll notifies observers, even when the fetched
/// `Vec<HostStatus>` is byte-for-byte identical to what was already stored.
/// This is not an oversight: a client (e.g. `PopoverView`'s empty-state text)
/// judges "has this host's watcher gone silent" by comparing `last_seen_at`
/// against `now` (`common::api::host_is_stale`), a comparison whose answer
/// changes purely with the passage of time, with no new data required to
/// flip it. If this only notified on a content change, a watcher that died
/// right after its last publish would freeze `last_seen_at` forever, the
/// polled `Vec<HostStatus>` would stop changing, and no observer would ever
/// be told to re-check staleness again - the exact silent failure PRO-211
/// exists to prevent. Renotifying on every tick costs nothing extra over the
/// network (the `GET` already happens every tick regardless); it only
/// changes whether local observers get re-invoked.
///
/// The thread holds only a [`Weak`] reference to `inner`, upgraded fresh each
/// iteration, and exits as soon as the upgrade fails - i.e. once every strong
/// `Arc<CoreInner>` has been dropped.
///
/// Be clear about what that does and does not achieve today: `Bridge` holds a
/// strong `Arc<CoreInner>`, and `Bridge` is retained by the detached reconnect
/// thread `SseClient::start` spawns, which never releases it. So in the
/// current architecture the upgrade always succeeds and this loop does in fact
/// run for the life of the process - dropping the `CoreHandle` does not stop
/// it, which was measured. `Weak` is still the right choice: it is strictly
/// weaker than the `Arc` it replaced, it costs nothing, and it means this
/// thread stops the moment that retain cycle is broken rather than becoming a
/// second thing to remember. Every real client holds its `CoreHandle` for the
/// process lifetime anyway, so nothing observable depends on the difference.
fn spawn_host_status_poller(inner: Arc<CoreInner>) {
    let weak = Arc::downgrade(&inner);
    let url = format!("{}/api/hosts", inner.server_url);
    // Deliberately not held across the loop - see this function's doc
    // comment. Every iteration re-upgrades `weak` for just the duration of
    // that iteration's work.
    drop(inner);

    std::thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .timeout(HOST_STATUS_POLL_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "failed to build host-status poll client; polling disabled");
                return;
            }
        };

        loop {
            let Some(inner) = weak.upgrade() else {
                return;
            };

            let parsed = client
                .get(&url)
                .send()
                .and_then(|r| r.error_for_status())
                .map_err(|e| e.to_string())
                .and_then(|r| r.text().map_err(|e| e.to_string()))
                .and_then(|body| {
                    serde_json::from_str::<Vec<HostStatus>>(&body).map_err(|e| e.to_string())
                });
            match parsed {
                Ok(hosts) => {
                    let observers = {
                        let mut s = inner.shared.lock().unwrap();
                        s.hosts = hosts.clone();
                        s.observers.values().cloned().collect::<Vec<_>>()
                    };
                    for observer in observers {
                        observer.on_host_status_changed(hosts.clone());
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "host-status poll failed; will retry");
                }
            }

            // Drop the strong ref before sleeping, so a long sleep doesn't
            // needlessly keep `CoreInner` alive if every other owner has
            // already gone away.
            drop(inner);
            std::thread::sleep(HOST_STATUS_POLL_INTERVAL);
        }
    });
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
    fn summary_counts_busy_and_shell_together_and_waiting_separately() {
        let sessions = vec![
            session("a", Status::Waiting { detail: None }),
            session(
                "b",
                Status::Waiting {
                    detail: Some("Allow Bash?".into()),
                },
            ),
            session("c", Status::Busy { tool: None }),
            session(
                "d",
                Status::Busy {
                    tool: Some("Bash".into()),
                },
            ),
            session("e", Status::Shell),
            session("f", Status::Idle),
            session("g", Status::Ended),
        ];
        let summary = MenuBarSummary::from_sessions(&sessions);
        assert_eq!(summary.busy, 3, "Busy and Shell both count as busy");
        assert_eq!(summary.waiting, 2);
    }

    #[test]
    fn empty_sessions_gives_zero_summary() {
        assert_eq!(
            MenuBarSummary::from_sessions(&[]),
            MenuBarSummary::default()
        );
    }
}
