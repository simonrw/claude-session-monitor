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

use std::path::PathBuf;
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
    pub agent_kind: AgentKind,
    pub model: Option<String>,
    pub updated_at: SystemTime,
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
    pub tmux_target: Option<String>,
}

impl From<common::api::SessionView> for SessionView {
    fn from(v: common::api::SessionView) -> Self {
        Self {
            session_id: v.session_id,
            cwd: v.cwd,
            status: v.status.into(),
            agent_kind: v.agent_kind.into(),
            model: v.model,
            updated_at: v.updated_at.into(),
            hostname: v.hostname,
            git_branch: v.git_branch,
            git_remote: v.git_remote,
            tmux_target: v.tmux_target,
        }
    }
}

// ---- Enums ---------------------------------------------------------------

#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl From<common::api::AgentKind> for AgentKind {
    fn from(a: common::api::AgentKind) -> Self {
        match a {
            common::api::AgentKind::Claude => Self::Claude,
            common::api::AgentKind::Codex => Self::Codex,
        }
    }
}

impl From<AgentKind> for common::api::AgentKind {
    fn from(a: AgentKind) -> Self {
        match a {
            AgentKind::Claude => Self::Claude,
            AgentKind::Codex => Self::Codex,
        }
    }
}

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

// ---- Errors -------------------------------------------------------------

/// Activation error variants exposed to Swift as a throwing enum.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ActivationError {
    #[error("Session has no tmux target")]
    NoTmuxTarget,
    #[error("Invalid tmux target format: {target}")]
    InvalidTarget { target: String },
    #[error("No tmux clients found")]
    NoTmuxClients,
    #[error("tmux command failed: {detail}")]
    TmuxFailed { detail: String },
    #[error("Failed to launch terminal: {detail}")]
    TerminalLaunchFailed { detail: String },
    #[error("activation is not supported on this platform")]
    UnsupportedPlatform,
}

impl From<common::activation::ActivationError> for ActivationError {
    fn from(e: common::activation::ActivationError) -> Self {
        match e {
            common::activation::ActivationError::NoTmuxTarget => Self::NoTmuxTarget,
            common::activation::ActivationError::InvalidTarget(t) => {
                Self::InvalidTarget { target: t }
            }
            common::activation::ActivationError::NoTmuxClients => Self::NoTmuxClients,
            common::activation::ActivationError::TmuxFailed(d) => Self::TmuxFailed { detail: d },
            common::activation::ActivationError::TerminalLaunchFailed(d) => {
                Self::TerminalLaunchFailed { detail: d }
            }
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

    /// Activate a session by switching to its tmux pane. For local sessions,
    /// switches the most recently active tmux client. For remote sessions,
    /// opens a new Ghostty terminal with SSH.
    ///
    /// On iOS the body is stubbed to return
    /// [`ActivationError::UnsupportedPlatform`] — there is no tmux /
    /// attachable terminal in the app sandbox. The FFI surface is identical
    /// across mac and iOS so Swift bindings are stable.
    ///
    /// `#[cfg]` is applied to the body (not the method) so `#[uniffi::export]`
    /// sees exactly one method regardless of target — otherwise UniFFI emits
    /// duplicate metadata constants and the build fails.
    pub fn activate_session(
        &self,
        #[cfg_attr(target_os = "ios", allow(unused_variables))] session: SessionView,
    ) -> Result<(), ActivationError> {
        #[cfg(target_os = "ios")]
        {
            Err(ActivationError::UnsupportedPlatform)
        }
        #[cfg(not(target_os = "ios"))]
        {
            let local_hostname = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_default();

            // Convert FFI SessionView back to common::api::SessionView for the
            // activation module. Only hostname and tmux_target matter for
            // activation, but we fill all fields for correctness.
            let common_status = match session.status {
                Status::Working { tool } => {
                    common::session::Status::Working(common::session::WorkingStatus { tool })
                }
                Status::Waiting { reason, detail } => {
                    let r = match reason {
                        WaitingReason::Permission => common::session::WaitingReason::Permission,
                        WaitingReason::Input => common::session::WaitingReason::Input,
                    };
                    common::session::Status::Waiting(common::session::WaitingStatus {
                        reason: r,
                        detail,
                    })
                }
                Status::Ended => common::session::Status::Ended,
            };
            let common_session = common::api::SessionView {
                session_id: session.session_id,
                cwd: session.cwd,
                status: common_status,
                agent_kind: session.agent_kind.into(),
                model: session.model,
                updated_at: chrono::DateTime::<chrono::Utc>::from(session.updated_at),
                hostname: session.hostname,
                git_branch: session.git_branch,
                git_remote: session.git_remote,
                tmux_target: session.tmux_target,
            };

            if let Err(e) = common::activation::activate(&common_session, &local_hostname) {
                tracing::error!(
                    session_id = %common_session.session_id,
                    hostname = ?common_session.hostname,
                    tmux_target = ?common_session.tmux_target,
                    error = %e,
                    "activate_session: activation failed"
                );
                return Err(e.into());
            }
            Ok(())
        }
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
/// `log_level` is a `tracing_subscriber::EnvFilter` directive (e.g. `"info"`);
/// `log_dir` is the filesystem directory to write rotated logs into (created
/// if missing). The foreign caller picks `log_dir` because the correct path
/// depends on the host platform (mac: `~/Library/Logs/...`, iOS: the app
/// sandbox's Caches dir, Linux: `~/.local/share/...`).
///
/// Must be called at most once per process; subsequent calls are no-ops on the
/// Rust side (the global subscriber can only be set once).
#[uniffi::export]
pub fn init_telemetry(
    app_label: String,
    log_level: String,
    log_dir: String,
) -> Arc<TelemetryGuard> {
    let dir = PathBuf::from(log_dir);
    Arc::new(TelemetryGuard {
        _guard: common::telemetry::init(&app_label, &log_level, &dir),
    })
}

// ---- Sentry --------------------------------------------------------------

/// RAII guard wrapping [`common::sentry::Guard`]. Holding it keeps the Sentry
/// client alive; dropping it flushes any pending events (the inner
/// `common::sentry::Guard` flushes on drop via the underlying
/// `sentry::ClientInitGuard`).
#[derive(uniffi::Object)]
pub struct SentryGuard {
    _guard: common::sentry::Guard,
}

/// Initialise Sentry error reporting. Returns a guard that must be held for
/// the lifetime of the process. When `SENTRY_DSN` was unset at build time the
/// guard is a no-op — still safe to drop.
#[uniffi::export]
pub fn init_sentry(app_label: String) -> Arc<SentryGuard> {
    Arc::new(SentryGuard {
        _guard: common::sentry::init(&app_label),
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
            agent_kind: common::api::AgentKind::Codex,
            model: Some("gpt-5.1-codex".into()),
            updated_at: chrono_now,
            hostname: Some("host".into()),
            git_branch: Some("main".into()),
            git_remote: Some("https://example/repo.git".into()),
            tmux_target: Some("main:0.1".into()),
        };
        let dst: SessionView = src.clone().into();
        assert_eq!(dst.session_id, "abc");
        assert_eq!(dst.cwd, "/tmp");
        assert_eq!(dst.hostname.as_deref(), Some("host"));
        assert!(matches!(dst.status, Status::Working { tool: Some(_) }));
        assert_eq!(dst.agent_kind, AgentKind::Codex);
        assert_eq!(dst.model.as_deref(), Some("gpt-5.1-codex"));
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

    #[test]
    fn activation_error_conversion_preserves_variants() {
        let cases: Vec<(common::activation::ActivationError, &str)> = vec![
            (
                common::activation::ActivationError::NoTmuxTarget,
                "NoTmuxTarget",
            ),
            (
                common::activation::ActivationError::InvalidTarget("bad".into()),
                "InvalidTarget",
            ),
            (
                common::activation::ActivationError::NoTmuxClients,
                "NoTmuxClients",
            ),
            (
                common::activation::ActivationError::TmuxFailed("err".into()),
                "TmuxFailed",
            ),
            (
                common::activation::ActivationError::TerminalLaunchFailed("err".into()),
                "TerminalLaunchFailed",
            ),
        ];
        for (src, label) in cases {
            let dst: ActivationError = src.into();
            // Verify the conversion produces the expected variant
            let matches = match (&dst, label) {
                (ActivationError::NoTmuxTarget, "NoTmuxTarget") => true,
                (ActivationError::InvalidTarget { .. }, "InvalidTarget") => true,
                (ActivationError::NoTmuxClients, "NoTmuxClients") => true,
                (ActivationError::TmuxFailed { .. }, "TmuxFailed") => true,
                (ActivationError::TerminalLaunchFailed { .. }, "TerminalLaunchFailed") => true,
                _ => false,
            };
            assert!(matches, "variant mismatch for {label}");
        }
    }

    #[test]
    fn activation_error_display_messages() {
        let err: ActivationError =
            common::activation::ActivationError::InvalidTarget("bad:format".into()).into();
        assert!(err.to_string().contains("bad:format"));

        let err: ActivationError =
            common::activation::ActivationError::TmuxFailed("session not found".into()).into();
        assert!(err.to_string().contains("session not found"));
    }

    /// The iOS stub for `activate_session` surfaces this variant. The variant
    /// must exist on every platform (the FFI surface is shared) and must
    /// render a clear human-readable message.
    #[test]
    fn unsupported_platform_display_message() {
        let err = ActivationError::UnsupportedPlatform;
        assert_eq!(
            err.to_string(),
            "activation is not supported on this platform"
        );
    }

    /// `init_sentry` is safe to call and its guard is safe to drop even when
    /// `SENTRY_DSN` was unset at build time (the inner guard is a no-op in
    /// that case). Exercising construct + drop catches any regressions in the
    /// FFI wrapping.
    #[test]
    fn init_sentry_constructs_and_drops() {
        let guard = init_sentry("csm-core-ffi-test".to_string());
        drop(guard);
    }
}
