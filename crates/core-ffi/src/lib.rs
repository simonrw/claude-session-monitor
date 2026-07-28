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
/// [`SessionObserver::on_summary_changed`]. See
/// `common::view_model::MenuBarSummary`'s doc comment for what counts as
/// `busy` (notably: `Shell` does) and why the old `waiting_input`/
/// `waiting_permission` split is gone.
#[derive(uniffi::Record, Clone, Copy, PartialEq, Eq)]
pub struct MenuBarSummary {
    pub busy: u32,
    pub waiting: u32,
}

impl From<common::view_model::MenuBarSummary> for MenuBarSummary {
    fn from(s: common::view_model::MenuBarSummary) -> Self {
        Self {
            busy: s.busy,
            waiting: s.waiting,
        }
    }
}

/// Mirrors `common::api::HostStatus`. Lets a client distinguish "this host
/// has zero live sessions" from "this host's watcher has never reported" -
/// see `on_host_status_changed` below.
#[derive(uniffi::Record, Clone, PartialEq)]
pub struct HostStatus {
    pub hostname: String,
    pub agent_kind: AgentKind,
    pub last_seen_at: SystemTime,
}

impl From<common::api::HostStatus> for HostStatus {
    fn from(h: common::api::HostStatus) -> Self {
        Self {
            hostname: h.hostname,
            agent_kind: h.agent_kind.into(),
            last_seen_at: h.last_seen_at.into(),
        }
    }
}

/// Whether a host last reported at `last_seen_at` should be treated as
/// having gone silent as of `now`, i.e. its watcher has stopped reporting
/// rather than genuinely having zero sessions right now.
///
/// A free function taking the two timestamps directly, rather than a method
/// on [`HostStatus`], because UniFFI records carry no behaviour across the
/// FFI boundary - see `common::api::host_is_stale`, which this wraps and
/// which is the single place the staleness threshold and comparison are
/// defined. Every client - the Rust GUI directly, and mac/iOS through this
/// export - shares that one definition rather than each re-implementing it.
#[uniffi::export]
pub fn host_status_is_stale(last_seen_at: SystemTime, now: SystemTime) -> bool {
    common::api::host_is_stale(
        chrono::DateTime::<chrono::Utc>::from(last_seen_at),
        chrono::DateTime::<chrono::Utc>::from(now),
    )
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
    /// `/rename` display label. `None` for Codex sessions and for any
    /// Claude session never renamed. See `common::api::SessionView::name`.
    pub name: Option<String>,
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
            name: v.name,
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

/// Session status - mirrors `common::session::Status`'s five-state
/// vocabulary (see its doc comment). `WaitingReason` (Permission/Input) is
/// gone: the registry carries no such distinction, so `Waiting` only carries
/// `detail` now.
#[derive(uniffi::Enum, Clone, PartialEq, Eq)]
pub enum Status {
    Busy { tool: Option<String> },
    Shell,
    Idle,
    Waiting { detail: Option<String> },
    Ended,
}

impl From<common::session::Status> for Status {
    fn from(s: common::session::Status) -> Self {
        match s {
            common::session::Status::Busy { tool } => Self::Busy { tool },
            common::session::Status::Shell => Self::Shell,
            common::session::Status::Idle => Self::Idle,
            common::session::Status::Waiting { detail } => Self::Waiting { detail },
            common::session::Status::Ended => Self::Ended,
        }
    }
}

impl From<Status> for common::session::Status {
    fn from(s: Status) -> Self {
        match s {
            Status::Busy { tool } => Self::Busy { tool },
            Status::Shell => Self::Shell,
            Status::Idle => Self::Idle,
            Status::Waiting { detail } => Self::Waiting { detail },
            Status::Ended => Self::Ended,
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

    /// See `common::view_model::SessionObserver::on_host_status_changed`'s
    /// doc comment: lets a client distinguish "zero live sessions" from "no
    /// watcher has ever reported for this host". Required (not defaulted):
    /// UniFFI callback interfaces cannot fall back to a Rust-side default
    /// body for a foreign (Swift) implementor, so every implementation of
    /// this trait - Rust or Swift - must define it explicitly.
    fn on_host_status_changed(&self, hosts: Vec<HostStatus>);
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
    fn on_host_status_changed(&self, hosts: Vec<common::api::HostStatus>) {
        let converted = hosts.into_iter().map(HostStatus::from).collect();
        self.foreign.on_host_status_changed(converted);
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
            let local_hostname = common::hostname::resolve().unwrap_or_default();

            // Convert FFI SessionView back to common::api::SessionView for the
            // activation module. Only hostname and tmux_target matter for
            // activation, but we fill all fields for correctness.
            let common_session = common::api::SessionView {
                session_id: session.session_id,
                cwd: session.cwd,
                status: session.status.into(),
                agent_kind: session.agent_kind.into(),
                model: session.model,
                updated_at: chrono::DateTime::<chrono::Utc>::from(session.updated_at),
                hostname: session.hostname,
                git_branch: session.git_branch,
                git_remote: session.git_remote,
                tmux_target: session.tmux_target,
                name: session.name,
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
        hosts: Mutex<Vec<Vec<HostStatus>>>,
    }

    impl Recorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sessions: Mutex::new(Vec::new()),
                connections: Mutex::new(Vec::new()),
                summaries: Mutex::new(Vec::new()),
                hosts: Mutex::new(Vec::new()),
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
        fn on_host_status_changed(&self, hosts: Vec<HostStatus>) {
            self.hosts.lock().unwrap().push(hosts);
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
        assert_eq!(recorder.hosts.lock().unwrap().len(), 1);
    }

    #[test]
    fn session_view_conversion_preserves_fields() {
        let chrono_now = Utc::now();
        let src = common::api::SessionView {
            session_id: "abc".into(),
            cwd: "/tmp".into(),
            status: common::session::Status::Busy {
                tool: Some("Bash".into()),
            },
            agent_kind: common::api::AgentKind::Codex,
            model: Some("gpt-5.1-codex".into()),
            updated_at: chrono_now,
            hostname: Some("host".into()),
            git_branch: Some("main".into()),
            git_remote: Some("https://example/repo.git".into()),
            tmux_target: Some("main:0.1".into()),
            name: Some("captain-marvel".into()),
        };
        let dst: SessionView = src.clone().into();
        assert_eq!(dst.session_id, "abc");
        assert_eq!(dst.cwd, "/tmp");
        assert_eq!(dst.hostname.as_deref(), Some("host"));
        assert!(matches!(dst.status, Status::Busy { tool: Some(_) }));
        assert_eq!(dst.agent_kind, AgentKind::Codex);
        assert_eq!(dst.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(dst.name.as_deref(), Some("captain-marvel"));
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
    fn status_conversion_round_trips_all_variants() {
        let cases = [
            common::session::Status::Busy { tool: None },
            common::session::Status::Busy {
                tool: Some("Bash".into()),
            },
            common::session::Status::Shell,
            common::session::Status::Idle,
            common::session::Status::Waiting { detail: None },
            common::session::Status::Waiting {
                detail: Some("Allow Bash to run cargo test?".into()),
            },
            common::session::Status::Ended,
        ];
        for case in cases {
            let ffi: Status = case.clone().into();
            let back: common::session::Status = ffi.into();
            assert_eq!(back, case);
        }
    }

    #[test]
    fn menu_bar_summary_conversion() {
        let src = common::view_model::MenuBarSummary {
            busy: 3,
            waiting: 2,
        };
        let dst: MenuBarSummary = src.into();
        assert_eq!(dst.busy, 3);
        assert_eq!(dst.waiting, 2);
    }

    #[test]
    fn host_status_is_stale_wraps_common_threshold() {
        let now = SystemTime::now();
        let fresh = now - std::time::Duration::from_secs(1);
        let stale = now - std::time::Duration::from_secs(60);
        assert!(!host_status_is_stale(fresh, now));
        assert!(host_status_is_stale(stale, now));
    }

    #[test]
    fn host_status_conversion_preserves_fields() {
        let src = common::api::HostStatus {
            hostname: "mbp".into(),
            agent_kind: common::api::AgentKind::Claude,
            last_seen_at: Utc::now(),
        };
        let dst: HostStatus = src.clone().into();
        assert_eq!(dst.hostname, "mbp");
        assert_eq!(dst.agent_kind, AgentKind::Claude);
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
