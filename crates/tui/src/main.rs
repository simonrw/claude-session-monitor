//! `csm-tui`: a ratatui terminal frontend for the session monitor.
//!
//! A pure frontend over the shared core: it registers a [`SessionObserver`]
//! whose callbacks forward into an mpsc channel, and the blocking event loop
//! selects between that channel and crossterm input. No tokio, no new
//! networking - the same data seam every other frontend uses.

mod ui;

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use chrono::Utc;
use clap::Parser;
use common::activation;
use common::api::{HostStatus, SessionView};
use common::view_model::{
    ConnectionState, CoreHandle, MenuBarSummary, SessionObserver, SubscriptionHandle,
};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use ui::AppState;

#[derive(Parser, Debug)]
#[command(name = "csm-tui", about = "Claude session monitor TUI")]
struct Args {
    /// Server URL (e.g. http://localhost:7685)
    #[arg(long)]
    server_url: Option<String>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Jump to the selected session on Enter and then exit. Turns the TUI into a
    /// one-shot session switcher, so running it in a tmux split/popup closes the
    /// pane once you pick a session.
    #[arg(long)]
    exit_on_select: bool,
}

/// A push update from the core, forwarded off the SSE / host-poll threads onto
/// the event loop's channel so the loop owns all state mutation.
enum CoreEvent {
    Sessions(Vec<SessionView>),
    Connection(ConnectionState),
    Summary(MenuBarSummary),
    Hosts(Vec<HostStatus>),
}

/// Observer that does nothing but forward each callback down the channel. It
/// stays cheap and non-blocking, as the trait requires, because a channel send
/// is all it does.
struct ChannelObserver {
    tx: Sender<CoreEvent>,
}

impl SessionObserver for ChannelObserver {
    fn on_sessions_changed(&self, sessions: Vec<SessionView>) {
        let _ = self.tx.send(CoreEvent::Sessions(sessions));
    }
    fn on_connection_changed(&self, state: ConnectionState) {
        let _ = self.tx.send(CoreEvent::Connection(state));
    }
    fn on_summary_changed(&self, summary: MenuBarSummary) {
        let _ = self.tx.send(CoreEvent::Summary(summary));
    }
    fn on_host_status_changed(&self, hosts: Vec<HostStatus>) {
        let _ = self.tx.send(CoreEvent::Hosts(hosts));
    }
}

impl AppState {
    fn apply(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Sessions(sessions) => self.set_sessions(sessions),
            CoreEvent::Connection(state) => {
                self.connected = matches!(state, ConnectionState::Connected)
            }
            CoreEvent::Summary(summary) => self.summary = summary,
            CoreEvent::Hosts(hosts) => {
                self.hosts = hosts;
                self.has_received_host_status = true;
            }
        }
    }
}

/// Whether a key event should quit: `q`, or Ctrl-C.
fn is_quit(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('q'))
        || (matches!(code, KeyCode::Char('c')) && modifiers.contains(KeyModifiers::CONTROL))
}

/// The blocking event loop: redraw, then wait up to a tick for either a key
/// press or a pending core event. The tick doubles as the re-render cadence so
/// relative times and staleness re-evaluate without user input.
fn run(
    terminal: &mut ratatui::DefaultTerminal,
    rx: Receiver<CoreEvent>,
    core: &CoreHandle,
    mut state: AppState,
    exit_on_select: bool,
) -> io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    loop {
        while let Ok(event) = rx.try_recv() {
            state.apply(event);
        }

        terminal.draw(|frame| ui::draw(frame, &state, Utc::now(), &home))?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            // The quit chord is suppressed while the modal is open so a stray
            // `q` can't tear down the app mid-confirmation; Esc/n/y drive it.
            if !state.modal_open() && is_quit(key.code, key.modifiers) {
                return Ok(());
            }
            // The key seam ([`AppState::handle_key_with`]) with the real side
            // effects bound: tmux/ssh activation and core-backed deletion.
            let core = core.clone();
            let outcome = state.handle_key_with(key.code, &home, activation::activate, move |id| {
                core.delete_session(id)
            });
            // In switcher mode a successful jump is the whole job: tear down the
            // TUI so the tmux split/popup it runs in closes behind the switch. A
            // failed activation keeps the app up (the inline error is visible).
            if exit_on_select && outcome == ui::KeyOutcome::Activated {
                return Ok(());
            }
        }
    }
}

/// Platform-appropriate default log directory (mirrors the GUI). Logs go to a
/// file only - nothing may touch stdout/stderr while the TUI owns the screen.
fn default_log_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    if cfg!(target_os = "macos") {
        home.join("Library/Logs/claude-session-monitor")
    } else {
        home.join(".local/share/claude-session-monitor")
    }
}

fn main() -> io::Result<()> {
    let _sentry = common::sentry::init("tui");
    let args = Args::parse();
    let _guard = common::telemetry::init("tui", &args.log_level, &default_log_dir());

    tracing::info!("starting TUI");

    let core = CoreHandle::new(args.server_url);
    let (tx, rx) = channel();
    // Held for the process lifetime so the observer stays subscribed.
    let _subscription: SubscriptionHandle = core.subscribe(Arc::new(ChannelObserver { tx }));

    let state = AppState {
        local_hostname: common::hostname::resolve().unwrap_or_default(),
        ..AppState::default()
    };

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, rx, &core, state, args.exit_on_select);
    ratatui::restore();
    result
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn parse_all_args() {
        let args = Args::parse_from([
            "csm-tui",
            "--server-url",
            "http://custom:1234",
            "--log-level",
            "debug",
        ]);
        assert_eq!(args.server_url, Some("http://custom:1234".into()));
        assert_eq!(args.log_level, "debug");
    }

    #[test]
    fn defaults_when_no_args() {
        let args = Args::parse_from(["csm-tui"]);
        assert_eq!(args.server_url, None);
        assert_eq!(args.log_level, "info");
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        assert!(is_quit(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(is_quit(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!is_quit(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!is_quit(KeyCode::Char('j'), KeyModifiers::NONE));
    }
}
