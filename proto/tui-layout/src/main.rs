//! Throwaway ratatui prototype for the csm-tui layout (PRO-222).
//!
//! Renders hardcoded fixture sessions so the visual design can be iterated on.
//! Run interactively (`cargo run`) or dump captured frames (`cargo run -- --frames`).
//!
//! Keys: j/k move, d delete modal (y confirm, n/Esc cancel), 1/2/3 switch view
//! (normal / no-watcher / no-sessions), q quit.

use std::io;
use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Row-level stale threshold (2 min) - intentionally longer than the host-level
/// watcher threshold (30 s) so a slow watcher doesn't flicker rows in/out of dim.
const ROW_STALE_SECS: u64 = 120;

#[derive(Clone)]
enum SessionStatus {
    Waiting(Option<String>),
    Busy(Option<String>),
    Shell,
    Idle,
    Ended,
}

#[derive(Clone)]
struct SessionView {
    /// Present only if the session was /rename'd.
    name: Option<String>,
    /// Present only for remote (non-local) sessions.
    hostname: Option<String>,
    /// Already shortened: ~/ relative to home.
    cwd_short: String,
    branch: Option<String>,
    /// Shortened remote: org/repo.
    remote: Option<String>,
    status: SessionStatus,
    /// Seconds since the session last reported.
    updated_secs_ago: u64,
    has_tmux_target: bool,
}

impl SessionView {
    fn is_stale(&self) -> bool {
        self.updated_secs_ago > ROW_STALE_SECS
    }

    fn is_waiting(&self) -> bool {
        matches!(self.status, SessionStatus::Waiting(_))
    }

    fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.cwd_short.clone())
    }
}

fn fixtures() -> Vec<SessionView> {
    vec![
        SessionView {
            name: Some("api-server".into()),
            hostname: None,
            cwd_short: "~/dev/claude-session-monitor".into(),
            branch: Some("tui".into()),
            remote: Some("simonrw/claude-session-monitor".into()),
            status: SessionStatus::Waiting(Some(
                "Approve running `cargo test --workspace` in the sandbox".into(),
            )),
            updated_secs_ago: 12,
            has_tmux_target: true,
        },
        SessionView {
            name: None,
            hostname: None,
            cwd_short: "~/dev/dotfiles".into(),
            branch: Some("main".into()),
            remote: Some("simonrw/dotfiles".into()),
            status: SessionStatus::Waiting(None),
            updated_secs_ago: 45,
            has_tmux_target: true,
        },
        SessionView {
            name: None,
            hostname: None,
            cwd_short: "~/dev/claude-session-monitor".into(),
            branch: Some("main".into()),
            remote: Some("simonrw/claude-session-monitor".into()),
            status: SessionStatus::Busy(Some("Bash".into())),
            updated_secs_ago: 3,
            has_tmux_target: true,
        },
        SessionView {
            name: None,
            hostname: Some("buildbox".into()),
            cwd_short: "~/src/infra".into(),
            branch: Some("feat/deploy".into()),
            remote: Some("acme/infra".into()),
            status: SessionStatus::Busy(None),
            updated_secs_ago: 8,
            has_tmux_target: true,
        },
        SessionView {
            name: None,
            hostname: None,
            cwd_short: "~/dev/scratch".into(),
            branch: None,
            remote: None,
            status: SessionStatus::Shell,
            updated_secs_ago: 90,
            has_tmux_target: true,
        },
        SessionView {
            name: Some("experiments".into()),
            hostname: None,
            cwd_short: "~/dev/experiments".into(),
            branch: Some("main".into()),
            remote: None,
            status: SessionStatus::Idle,
            updated_secs_ago: 300,
            has_tmux_target: false,
        },
        SessionView {
            name: None,
            hostname: Some("buildbox".into()),
            cwd_short: "~/src/tools".into(),
            branch: Some("main".into()),
            remote: Some("acme/tools".into()),
            status: SessionStatus::Busy(Some("Edit".into())),
            updated_secs_ago: 400,
            has_tmux_target: true,
        },
        SessionView {
            name: None,
            hostname: None,
            cwd_short: "~/dev/old-project".into(),
            branch: Some("main".into()),
            remote: Some("simonrw/old-project".into()),
            status: SessionStatus::Ended,
            updated_secs_ago: 3700,
            has_tmux_target: false,
        },
    ]
}

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Normal,
    EmptyNoWatcher,
    EmptyNoSessions,
}

struct App {
    sessions: Vec<SessionView>,
    selected: usize,
    mode: ViewMode,
    modal: bool,
    connected: bool,
}

impl App {
    fn new() -> Self {
        Self {
            sessions: fixtures(),
            selected: 0,
            mode: ViewMode::Normal,
            modal: false,
            connected: true,
        }
    }

    /// Waiting sessions first, then the rest - the order rows render in.
    fn ordered(&self) -> Vec<&SessionView> {
        let (waiting, rest): (Vec<_>, Vec<_>) =
            self.sessions.iter().partition(|s| s.is_waiting());
        waiting.into_iter().chain(rest).collect()
    }
}

fn rel_time(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

fn truncate_detail(detail: &str) -> String {
    const MAX: usize = 35;
    let chars: Vec<char> = detail.chars().collect();
    if chars.len() <= MAX {
        detail.to_string()
    } else {
        let mut s: String = chars[..MAX - 1].iter().collect();
        s.push('…');
        s
    }
}

fn status_display(status: &SessionStatus) -> (String, Color) {
    match status {
        SessionStatus::Waiting(None) => ("waiting".into(), Color::Yellow),
        SessionStatus::Waiting(Some(detail)) => {
            (format!("waiting: {}", truncate_detail(detail)), Color::Yellow)
        }
        SessionStatus::Busy(None) => ("busy".into(), Color::Cyan),
        SessionStatus::Busy(Some(tool)) => (format!("busy:{tool}"), Color::Cyan),
        SessionStatus::Shell => ("shell".into(), Color::Blue),
        SessionStatus::Idle => ("idle".into(), Color::DarkGray),
        SessionStatus::Ended => ("ended".into(), Color::DarkGray),
    }
}

/// {name_chip}{hostname_prefix}{cwd_short}{vcs}  {status_display}{stale_tag}{tmux_tag}  {rel_time}
fn session_line(s: &SessionView, selected: bool, in_waiting_section: bool) -> Line<'static> {
    let dimmed = matches!(s.status, SessionStatus::Ended) || s.is_stale();
    let dim = |c: Color| if dimmed { Color::DarkGray } else { c };

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(if selected { "▶ " } else { "  " }));

    if let Some(name) = &s.name {
        spans.push(Span::styled(
            format!("[{name}] "),
            Style::default()
                .fg(dim(Color::Magenta))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(host) = &s.hostname {
        spans.push(Span::styled(
            format!("{host}:"),
            Style::default().fg(dim(Color::Green)),
        ));
    }
    spans.push(Span::styled(
        s.cwd_short.clone(),
        Style::default().fg(dim(Color::Reset)),
    ));
    if let Some(branch) = &s.branch {
        let vcs = match &s.remote {
            Some(remote) => format!(" {branch}@{remote}"),
            None => format!(" {branch}"),
        };
        spans.push(Span::styled(vcs, Style::default().fg(dim(Color::Blue))));
    }

    let (status_text, status_color) = status_display(&s.status);
    spans.push(Span::styled(
        format!("  {status_text}"),
        Style::default().fg(dim(status_color)),
    ));

    if s.is_stale() {
        // Red survives dimming so it still reads as a warning.
        spans.push(Span::styled(" [stale]", Style::default().fg(Color::Red)));
    }
    if !s.has_tmux_target {
        spans.push(Span::styled(" ⊗", Style::default().fg(Color::DarkGray)));
    }

    spans.push(Span::styled(
        format!("  {}", rel_time(s.updated_secs_ago)),
        Style::default().fg(Color::DarkGray),
    ));

    let mut line = Line::from(spans);
    if selected {
        let bg = if in_waiting_section {
            Color::Rgb(60, 50, 10)
        } else {
            Color::Rgb(40, 40, 60)
        };
        line = line.style(Style::default().bg(bg));
    }
    line
}

fn header_line(app: &App) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "csm",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if app.connected {
        spans.push(Span::styled("  ● ", Style::default().fg(Color::Green)));
        spans.push(Span::styled(
            "connected",
            Style::default().fg(Color::Green),
        ));
    } else {
        spans.push(Span::styled("  ● ", Style::default().fg(Color::Red)));
        spans.push(Span::styled(
            "disconnected",
            Style::default().fg(Color::Red),
        ));
    }
    if app.mode == ViewMode::Normal {
        let waiting = app.sessions.iter().filter(|s| s.is_waiting()).count();
        let busy = app
            .sessions
            .iter()
            .filter(|s| matches!(s.status, SessionStatus::Busy(_)))
            .count();
        if waiting > 0 {
            spans.push(Span::styled(
                format!("  {waiting} waiting"),
                Style::default().fg(Color::Yellow),
            ));
        }
        spans.push(Span::raw(format!("  {busy} busy")));
    }
    Line::from(spans)
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn draw(frame: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header (title line + bottom border)
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer/help bar
        ])
        .split(frame.area());

    let header = Paragraph::new(header_line(app))
        .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, outer[0]);

    match app.mode {
        ViewMode::EmptyNoWatcher => draw_empty(
            frame,
            outer[1],
            "No watcher has reported in yet",
            "Start csm-watcher on a host to begin monitoring sessions.",
        ),
        ViewMode::EmptyNoSessions => draw_empty(
            frame,
            outer[1],
            "No active sessions",
            "The watcher is running but there are no active Claude Code sessions on this host.",
        ),
        ViewMode::Normal => draw_sessions(frame, outer[1], app),
    }

    let help = Paragraph::new(Line::from(Span::styled(
        " j/k move  d delete  1/2/3 view  q quit",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(help, outer[2]);

    if app.modal {
        draw_modal(frame, app);
    }
}

fn draw_empty(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let lines = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            body.to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(area);
    let para = Paragraph::new(lines).centered();
    frame.render_widget(para, vertical[1]);
}

fn draw_sessions(frame: &mut Frame, area: Rect, app: &App) {
    let ordered = app.ordered();
    let waiting_count = ordered.iter().filter(|s| s.is_waiting()).count();

    let chunks = if waiting_count > 0 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(waiting_count as u16 + 2), // +2 for top/bottom borders
                Constraint::Min(0),
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    };

    if waiting_count > 0 {
        let lines: Vec<Line> = ordered[..waiting_count]
            .iter()
            .enumerate()
            .map(|(i, s)| session_line(s, i == app.selected, true))
            .collect();
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Span::styled(
                " ⚠ waiting for you ",
                Style::default().fg(Color::Yellow),
            ));
        frame.render_widget(Paragraph::new(lines).block(block), chunks[0]);
    }

    let lines: Vec<Line> = ordered[waiting_count..]
        .iter()
        .enumerate()
        .map(|(i, s)| session_line(s, waiting_count + i == app.selected, false))
        .collect();
    frame.render_widget(Paragraph::new(lines), chunks[1]);
}

fn draw_modal(frame: &mut Frame, app: &App) {
    let area = centered_rect(52, 7, frame.area());
    frame.render_widget(Clear, area);
    let target = app
        .ordered()
        .get(app.selected)
        .map(|s| s.display_name())
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(" Delete session ");
    let lines = vec![
        Line::raw(""),
        Line::from(format!("Delete \"{target}\"?")).centered(),
        Line::raw(""),
        Line::from(Span::styled(
            "y confirm   Esc/n cancel",
            Style::default().fg(Color::DarkGray),
        ))
        .centered(),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Render each scenario with a TestBackend and print the buffers - captured
/// frames for the ticket, and a way to eyeball the layout without a TTY.
fn dump_frames() {
    let scenarios: Vec<(&str, Box<dyn Fn(&mut App)>)> = vec![
        ("normal (wide, 100x20)", Box::new(|_: &mut App| {})),
        ("normal (narrow, 60x20)", Box::new(|_: &mut App| {})),
        (
            "delete modal",
            Box::new(|app: &mut App| app.modal = true),
        ),
        (
            "empty: no watcher (disconnected)",
            Box::new(|app: &mut App| {
                app.mode = ViewMode::EmptyNoWatcher;
                app.connected = false;
            }),
        ),
        (
            "empty: no sessions",
            Box::new(|app: &mut App| app.mode = ViewMode::EmptyNoSessions),
        ),
    ];

    for (name, setup) in scenarios {
        let width = if name.contains("narrow") { 60 } else { 100 };
        let mut app = App::new();
        setup(&mut app);
        let backend = TestBackend::new(width, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        println!("=== {name} ===");
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            println!("|{}|", line.trim_end());
        }
        println!();
    }
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--frames") {
        dump_frames();
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let mut app = App::new();
    loop {
        terminal.draw(|f| draw(f, &app))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let count = app.ordered().len();
            if app.modal {
                match key.code {
                    KeyCode::Char('y') => {
                        app.modal = false;
                        // Prototype: deletion is a no-op.
                    }
                    KeyCode::Char('n') | KeyCode::Esc => app.modal = false,
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('j') | KeyCode::Down => {
                    if count > 0 {
                        app.selected = (app.selected + 1) % count;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if count > 0 {
                        app.selected = (app.selected + count - 1) % count;
                    }
                }
                KeyCode::Char('d') => {
                    if app.mode == ViewMode::Normal && count > 0 {
                        app.modal = true;
                    }
                }
                KeyCode::Char('1') => app.mode = ViewMode::Normal,
                KeyCode::Char('2') => app.mode = ViewMode::EmptyNoWatcher,
                KeyCode::Char('3') => app.mode = ViewMode::EmptyNoSessions,
                _ => {}
            }
        }
    }
    ratatui::restore();
    Ok(())
}
