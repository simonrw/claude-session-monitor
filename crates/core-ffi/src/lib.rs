//! UniFFI bridge exposing the headless session-monitor view-model to Swift.
//!
//! Builds as `staticlib` + `cdylib`; linked into the macOS app bundle via the
//! XCFramework produced in PRO-125. Linux/Windows egui builds do not depend
//! on this crate, so they avoid the UniFFI codegen and runtime.
//!
//! All types are bridged rather than re-exported so the FFI surface is
//! explicit. `SessionView::updated_at` is converted to `SystemTime` (mapped to
//! Swift `Date` by UniFFI) at the boundary; internally the core keeps the
//! original `chrono::DateTime<Utc>`.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

uniffi::setup_scaffolding!();

// ---- Records -------------------------------------------------------------

/// Pre-computed menu-bar summary. Re-derived on the Rust side and pushed via
/// [`SessionObserver::on_summary_changed`].
#[derive(uniffi::Record, Clone, Copy, PartialEq, Eq)]
pub struct MenuBarSummary {
    pub waiting_input: u32,
    pub waiting_permission: u32,
    pub working: u32,
}

impl From<common::view_model::MenuBarSummary> for MenuBarSummary {
    fn from(s: common::view_model::MenuBarSummary) -> Self {
        Self {
            waiting_input: s.waiting_input,
            waiting_permission: s.waiting_permission,
            working: s.working,
        }
    }
}

#[derive(uniffi::Record, Clone)]
pub struct SessionView {
    pub session_id: String,
    pub cwd: String,
    pub status: Status,
    pub updated_at: SystemTime,
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
}

impl From<common::api::SessionView> for SessionView {
    fn from(v: common::api::SessionView) -> Self {
        Self {
            session_id: v.session_id,
            cwd: v.cwd,
            status: v.status.into(),
            updated_at: v.updated_at.into(),
            hostname: v.hostname,
            git_branch: v.git_branch,
            git_remote: v.git_remote,
        }
    }
}

// ---- Enums ---------------------------------------------------------------

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
}

impl From<common::view_model::ConnectionState> for ConnectionState {
    fn from(s: common::view_model::ConnectionState) -> Self {
        match s {
            common::view_model::ConnectionState::Connecting => Self::Connecting,
            common::view_model::ConnectionState::Connected => Self::Connected,
            common::view_model::ConnectionState::Disconnected => Self::Disconnected,
        }
    }
}

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReason {
    Permission,
    Input,
}

impl From<common::session::WaitingReason> for WaitingReason {
    fn from(r: common::session::WaitingReason) -> Self {
        match r {
            common::session::WaitingReason::Permission => Self::Permission,
            common::session::WaitingReason::Input => Self::Input,
        }
    }
}

/// Session status. Flattened from Rust's nested `Status::Working(WorkingStatus)`
/// etc. so UniFFI emits a clean Swift enum.
#[derive(uniffi::Enum, Clone)]
pub enum Status {
    Working {
        tool: Option<String>,
    },
    Waiting {
        reason: WaitingReason,
        detail: Option<String>,
    },
    Ended,
}

impl From<common::session::Status> for Status {
    fn from(s: common::session::Status) -> Self {
        match s {
            common::session::Status::Working(w) => Self::Working { tool: w.tool },
            common::session::Status::Waiting(w) => Self::Waiting {
                reason: w.reason.into(),
                detail: w.detail,
            },
            common::session::Status::Ended => Self::Ended,
        }
    }
}

// ---- Callback interface --------------------------------------------------

/// Foreign callback interface. Swift (or any UniFFI target) implements this;
/// Rust invokes callbacks from the SSE worker thread.
#[uniffi::export(with_foreign)]
pub trait SessionObserver: Send + Sync {
    fn on_sessions_changed(&self, sessions: Vec<SessionView>);
    fn on_connection_changed(&self, state: ConnectionState);
    fn on_summary_changed(&self, summary: MenuBarSummary);
}

/// Adapts a foreign [`SessionObserver`] into the Rust-side trait, converting
/// types at the boundary.
struct ObserverAdapter {
    foreign: Arc<dyn SessionObserver>,
}

impl common::view_model::SessionObserver for ObserverAdapter {
    fn on_sessions_changed(&self, sessions: Vec<common::api::SessionView>) {
        let converted = sessions.into_iter().map(SessionView::from).collect();
        self.foreign.on_sessions_changed(converted);
    }
    fn on_connection_changed(&self, state: common::view_model::ConnectionState) {
        self.foreign.on_connection_changed(state.into());
    }
    fn on_summary_changed(&self, summary: common::view_model::MenuBarSummary) {
        self.foreign.on_summary_changed(summary.into());
    }
}

// ---- Objects -------------------------------------------------------------

/// RAII handle returned by [`CoreHandle::subscribe`]. Dropping it detaches the
/// observer. Holding the Arc keeps the observer callback registered.
#[derive(uniffi::Object)]
pub struct SubscriptionHandle {
    // Hold the inner subscription in a Mutex<Option<...>> so Drop can take
    // ownership without unsafe. (uniffi::Object is required to be Send+Sync,
    // and SubscriptionHandle is !Sync alone.)
    inner: Mutex<Option<common::view_model::SubscriptionHandle>>,
}

#[uniffi::export]
impl SubscriptionHandle {
    /// Explicitly detach this observer. Equivalent to dropping the Swift
    /// instance, but deterministic in environments where ARC timing is
    /// unclear.
    pub fn cancel(&self) {
        self.inner.lock().unwrap().take();
    }
}

/// Headless core handle. Owns SSE, session store, config. Cheap to clone
/// between threads (internally `Arc<..>`).
#[derive(uniffi::Object)]
pub struct CoreHandle {
    inner: common::view_model::CoreHandle,
}

#[uniffi::export]
impl CoreHandle {
    /// Construct a core, starting SSE in the background.
    ///
    /// `server_url` overrides the config file's server URL; pass `None` to use
    /// the default resolution order (config → env → compiled-in).
    #[uniffi::constructor]
    pub fn new(server_url: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            inner: common::view_model::CoreHandle::new(server_url),
        })
    }

    /// Subscribe for push updates. The observer is notified immediately with
    /// the current snapshot. Keep the returned handle alive — dropping it
    /// detaches the observer.
    pub fn subscribe(&self, observer: Arc<dyn SessionObserver>) -> Arc<SubscriptionHandle> {
        let adapter = Arc::new(ObserverAdapter { foreign: observer });
        let sub = self.inner.subscribe(adapter);
        Arc::new(SubscriptionHandle {
            inner: Mutex::new(Some(sub)),
        })
    }

    /// Fire-and-forget delete. Errors are logged; no result is returned.
    pub fn delete_session(&self, session_id: String) {
        self.inner.delete_session(session_id);
    }

    /// Current connection state snapshot.
    pub fn connection_state(&self) -> ConnectionState {
        self.inner.connection_state().into()
    }

    /// Server URL this core is talking to.
    pub fn server_url(&self) -> String {
        self.inner.server_url().to_string()
    }
}

// ---- Telemetry -----------------------------------------------------------

/// RAII guard keeping the tracing subscriber alive. Drop on app shutdown to
/// flush the non-blocking writer.
#[derive(uniffi::Object)]
pub struct TelemetryGuard {
    _guard: common::telemetry::Guard,
}

/// Install the global tracing subscriber. `app_label` names the log file;
/// `log_level` is a `tracing_subscriber::EnvFilter` directive (e.g. `"info"`).
/// Overridden by the `RUST_LOG` env var if set.
///
/// Must be called at most once per process; subsequent calls are no-ops on the
/// Rust side (the global subscriber can only be set once).
#[uniffi::export]
pub fn init_telemetry(app_label: String, log_level: String) -> Arc<TelemetryGuard> {
    Arc::new(TelemetryGuard {
        _guard: common::telemetry::init(&app_label, &log_level),
    })
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use common::view_model::CoreHandle as InnerCore;

    /// Test observer that captures callback invocations for assertions.
    struct Recorder {
        sessions: Mutex<Vec<Vec<SessionView>>>,
        connections: Mutex<Vec<ConnectionState>>,
        summaries: Mutex<Vec<MenuBarSummary>>,
    }

    impl Recorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sessions: Mutex::new(Vec::new()),
                connections: Mutex::new(Vec::new()),
                summaries: Mutex::new(Vec::new()),
            })
        }
    }

    impl SessionObserver for Recorder {
        fn on_sessions_changed(&self, sessions: Vec<SessionView>) {
            self.sessions.lock().unwrap().push(sessions);
        }
        fn on_connection_changed(&self, state: ConnectionState) {
            self.connections.lock().unwrap().push(state);
        }
        fn on_summary_changed(&self, summary: MenuBarSummary) {
            self.summaries.lock().unwrap().push(summary);
        }
    }

    #[test]
    fn subscribe_replays_current_snapshot() {
        // CoreHandle::new would load config; use with_server_url to avoid that.
        let inner = InnerCore::with_server_url("http://127.0.0.1:1".into());
        let core = CoreHandle { inner };
        let recorder = Recorder::new();
        let core_arc = Arc::new(core);
        let _sub = CoreHandle::subscribe(&core_arc, recorder.clone() as Arc<dyn SessionObserver>);
        // Initial snapshot fires exactly once per event type.
        assert_eq!(recorder.sessions.lock().unwrap().len(), 1);
        assert_eq!(recorder.connections.lock().unwrap().len(), 1);
        assert_eq!(recorder.summaries.lock().unwrap().len(), 1);
    }

    #[test]
    fn session_view_conversion_preserves_fields() {
        let chrono_now = Utc::now();
        let src = common::api::SessionView {
            session_id: "abc".into(),
            cwd: "/tmp".into(),
            status: common::session::Status::Working(common::session::WorkingStatus {
                tool: Some("Bash".into()),
            }),
            updated_at: chrono_now,
            hostname: Some("host".into()),
            git_branch: Some("main".into()),
            git_remote: Some("https://example/repo.git".into()),
        };
        let dst: SessionView = src.clone().into();
        assert_eq!(dst.session_id, "abc");
        assert_eq!(dst.cwd, "/tmp");
        assert_eq!(dst.hostname.as_deref(), Some("host"));
        assert!(matches!(dst.status, Status::Working { tool: Some(_) }));
        // SystemTime round-trip is lossy past nanosecond precision but
        // equivalent at millisecond resolution.
        let expected_epoch = chrono_now.timestamp() as u64;
        let actual_epoch = dst
            .updated_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(actual_epoch, expected_epoch);
    }

    #[test]
    fn menu_bar_summary_conversion() {
        let src = common::view_model::MenuBarSummary {
            waiting_input: 2,
            waiting_permission: 1,
            working: 3,
        };
        let dst: MenuBarSummary = src.into();
        assert_eq!(dst.waiting_input, 2);
        assert_eq!(dst.waiting_permission, 1);
        assert_eq!(dst.working, 3);
    }
}
