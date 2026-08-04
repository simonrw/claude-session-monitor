//! The render seam: turn an [`AppState`] snapshot into a ratatui frame.
//!
//! [`draw`] is a pure function of ([`AppState`], `now`, `home`): it reads no
//! clock and no environment, so a `TestBackend` test can feed it fixtures and
//! assert on the rendered buffer (section ordering, row content, truncation).
//! All presentation decisions - partitioning, staleness/fade, status label and
//! colour, cwd/remote shortening, relative time - come from
//! [`common::presentation`] rather than being re-derived here.

use chrono::{DateTime, Utc};
use common::api::{HostStatus, SessionView};
use common::presentation;
use common::session::Status;
use common::view_model::MenuBarSummary;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// The full state a frame is rendered from. The observer thread mutates this
/// (via the event channel); the render is a pure function of a snapshot of it.
#[derive(Default, Clone)]
pub struct AppState {
    pub sessions: Vec<SessionView>,
    pub connected: bool,
    pub summary: MenuBarSummary,
    /// Latest `GET /api/hosts` snapshot, for the watcher-silent empty state.
    pub hosts: Vec<HostStatus>,
    /// Whether at least one host-status poll has landed (see
    /// [`presentation::watcher_appears_silent`]).
    pub has_received_host_status: bool,
}

/// Map a UI-agnostic [`presentation::Rgb`] identity to ratatui's [`Color`].
fn to_color(rgb: presentation::Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// The waiting accent - the header count, the section border and its title all
/// use it. Taken from [`presentation::status_color`] rather than a literal so
/// the "needs you" red is defined in exactly one place across every frontend.
fn waiting_color() -> Color {
    to_color(presentation::status_color(&Status::Waiting { detail: None }))
}

/// The header line: title, connection indicator, then waiting/busy counts.
fn header_line(state: &AppState) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "csm",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if state.connected {
        spans.push(Span::styled("  \u{25cf} connected", Style::default().fg(Color::Green)));
    } else {
        spans.push(Span::styled(
            "  \u{25cf} disconnected",
            Style::default().fg(Color::Red),
        ));
    }
    if state.summary.waiting > 0 {
        spans.push(Span::styled(
            format!("  {} waiting", state.summary.waiting),
            Style::default().fg(waiting_color()),
        ));
    }
    spans.push(Span::raw(format!("  {} busy", state.summary.busy)));
    Line::from(spans)
}

/// One session row:
/// `[name] host:~/cwd branch@remote  status  rel_time`
///
/// Everything is a single [`Line`]; long rows truncate (the paragraph is drawn
/// without wrapping) rather than spilling onto a second line.
fn session_row(session: &SessionView, now: DateTime<Utc>, home: &str, connected: bool) -> Line<'static> {
    let stale = presentation::is_stale(session.updated_at, now);
    let has_tmux = session.tmux_target.is_some();
    // Faded (disconnected or stale) or un-jumpable sessions read as dimmed:
    // the whole row collapses to a single muted grey.
    let dimmed = presentation::should_fade(connected, stale) || !has_tmux;
    let dim = |c: Color| if dimmed { Color::DarkGray } else { c };

    let mut spans: Vec<Span> = Vec::new();

    // The `/rename` label, when set, sits ahead of the location as the
    // user-chosen name of intent (PRO-215).
    if let Some(name) = session.name.as_deref().filter(|n| !n.is_empty()) {
        spans.push(Span::styled(
            format!("[{name}] "),
            Style::default().fg(dim(Color::Magenta)).add_modifier(Modifier::BOLD),
        ));
    }

    if let Some(host) = &session.hostname {
        spans.push(Span::styled(format!("{host}:"), Style::default().fg(dim(Color::Green))));
    }

    let cwd_short = presentation::shorten_cwd(&session.cwd, home);
    spans.push(Span::styled(cwd_short, Style::default().fg(dim(Color::Reset))));

    if let Some(branch) = &session.git_branch {
        let remote = session.git_remote.as_deref().map(presentation::strip_git_remote);
        let vcs = match remote {
            Some(remote) => format!(" {branch}@{remote}"),
            None => format!(" {branch}"),
        };
        spans.push(Span::styled(vcs, Style::default().fg(dim(Color::Blue))));
    }

    let status = presentation::status_label(&session.status);
    let status_color = to_color(presentation::status_color(&session.status));
    spans.push(Span::styled(format!("  {status}"), Style::default().fg(dim(status_color))));

    let rel = presentation::relative_time(session.updated_at, now);
    spans.push(Span::styled(format!("  {rel}"), Style::default().fg(Color::DarkGray)));

    Line::from(spans)
}

/// Centered two-line empty-state message inside `area`.
fn draw_empty(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let lines = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(body.to_string(), Style::default().fg(Color::DarkGray))),
    ];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    frame.render_widget(Paragraph::new(lines).centered(), rows[1]);
}

/// The two-section list: waiting sessions (bordered, on top), then the rest.
fn draw_sessions(frame: &mut Frame, area: Rect, state: &AppState, now: DateTime<Utc>, home: &str) {
    let (waiting, rest) = presentation::partition_sessions(&state.sessions);

    let sections = if waiting.is_empty() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            // +2 for the section's top/bottom borders.
            .constraints([Constraint::Length(waiting.len() as u16 + 2), Constraint::Min(0)])
            .split(area)
    };

    if !waiting.is_empty() {
        let lines: Vec<Line> = waiting
            .iter()
            .map(|s| session_row(s, now, home, state.connected))
            .collect();
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(waiting_color()))
            .title(Span::styled(
                " waiting for you ",
                Style::default().fg(waiting_color()),
            ));
        frame.render_widget(Paragraph::new(lines).block(block), sections[0]);
    }

    let lines: Vec<Line> = rest
        .iter()
        .map(|s| session_row(s, now, home, state.connected))
        .collect();
    frame.render_widget(Paragraph::new(lines), sections[1]);
}

/// Render a full frame from `state` as of `now`, shortening paths against `home`.
pub fn draw(frame: &mut Frame, state: &AppState, now: DateTime<Utc>, home: &str) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header + bottom border
            Constraint::Min(0),    // body
            Constraint::Length(1), // help bar
        ])
        .split(frame.area());

    let header = Paragraph::new(header_line(state)).block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, outer[0]);

    if state.sessions.is_empty() {
        if presentation::watcher_appears_silent(&state.hosts, state.has_received_host_status, now) {
            draw_empty(
                frame,
                outer[1],
                "No watcher has reported in yet",
                "Start csm-watcher on a host to begin monitoring sessions.",
            );
        } else {
            draw_empty(
                frame,
                outer[1],
                "No active sessions",
                "The watcher is running but no sessions are active right now.",
            );
        }
    } else {
        draw_sessions(frame, outer[1], state, now, home);
    }

    let help = Paragraph::new(Line::from(Span::styled(
        " q quit",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(help, outer[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::api::AgentKind;
    use common::session::Status;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn session(id: &str, status: Status) -> SessionView {
        SessionView {
            session_id: id.into(),
            cwd: "/Users/me/dev/project".into(),
            status,
            agent_kind: AgentKind::Claude,
            model: None,
            updated_at: now(),
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: Some("sess:1.0".into()),
            name: None,
        }
    }

    /// Render `state` at the given size and return the buffer as one string per
    /// row (trailing whitespace trimmed).
    fn render(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, state, now(), "/Users/me"))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                let mut line = String::new();
                for x in 0..buffer.area.width {
                    line.push_str(buffer[(x, y)].symbol());
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn row_index(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("expected a row containing {needle:?} in {rows:#?}"))
    }

    #[test]
    fn waiting_section_renders_above_the_rest() {
        let state = AppState {
            sessions: vec![
                session("busy", Status::Busy { tool: None }),
                session(
                    "wait",
                    Status::Waiting {
                        detail: Some("Approve?".into()),
                    },
                ),
            ],
            connected: true,
            summary: MenuBarSummary { busy: 1, waiting: 1 },
            ..Default::default()
        };
        let rows = render(&state, 100, 20);
        assert!(rows.iter().any(|r| r.contains("waiting for you")));
        assert!(
            row_index(&rows, "waiting(Approve?)") < row_index(&rows, "project  busy"),
            "waiting row should sort above the busy row: {rows:#?}"
        );
    }

    #[test]
    fn row_shows_name_host_cwd_branch_remote_status_and_time() {
        let mut s = session("s", Status::Busy { tool: Some("Bash".into()) });
        s.name = Some("api-server".into());
        s.hostname = Some("buildbox".into());
        s.git_branch = Some("tui".into());
        s.git_remote = Some("https://github.com/me/proj.git".into());
        let state = AppState {
            sessions: vec![s],
            connected: true,
            summary: MenuBarSummary { busy: 1, waiting: 0 },
            ..Default::default()
        };
        let rows = render(&state, 120, 20);
        let row = &rows[row_index(&rows, "[api-server]")];
        assert!(row.contains("[api-server]"), "name label: {row}");
        assert!(row.contains("buildbox:"), "hostname: {row}");
        assert!(row.contains("~/dev/project"), "shortened cwd: {row}");
        assert!(row.contains("tui@me/proj"), "branch and stripped remote: {row}");
        assert!(row.contains("busy(Bash)"), "status label: {row}");
        assert!(row.contains("0s ago"), "relative time: {row}");
    }

    #[test]
    fn header_shows_connection_and_counts() {
        let state = AppState {
            connected: true,
            summary: MenuBarSummary { busy: 2, waiting: 3 },
            ..Default::default()
        };
        let rows = render(&state, 80, 10);
        assert!(rows[0].contains("connected"), "{rows:#?}");
        assert!(rows[0].contains("3 waiting"), "{rows:#?}");
        assert!(rows[0].contains("2 busy"), "{rows:#?}");
    }

    #[test]
    fn long_rows_truncate_to_the_terminal_width() {
        let mut s = session("s", Status::Busy { tool: None });
        s.cwd = "/Users/me/dev/a-really-long-directory-name-that-will-not-fit".into();
        s.git_branch = Some("a-very-long-feature-branch-name".into());
        s.git_remote = Some("https://github.com/org/enormous-repository-name".into());
        let state = AppState {
            sessions: vec![s],
            connected: true,
            summary: MenuBarSummary { busy: 1, waiting: 0 },
            ..Default::default()
        };
        let width = 40;
        let rows = render(&state, width, 20);
        // No row exceeds the terminal width, and the tail of the content is cut.
        assert!(rows.iter().all(|r| r.chars().count() <= width as usize));
        let row = &rows[row_index(&rows, "~/dev/a-really-long")];
        assert!(!row.contains("enormous-repository-name"), "tail truncated: {row}");
    }

    #[test]
    fn empty_state_distinguishes_silent_watcher_from_no_sessions() {
        let silent = AppState {
            connected: true,
            has_received_host_status: true,
            hosts: vec![],
            ..Default::default()
        };
        let rows = render(&silent, 80, 12);
        assert!(rows.iter().any(|r| r.contains("No watcher has reported")), "{rows:#?}");

        let no_sessions = AppState {
            connected: true,
            has_received_host_status: true,
            hosts: vec![HostStatus {
                hostname: "mbp".into(),
                agent_kind: AgentKind::Claude,
                last_seen_at: now(),
            }],
            ..Default::default()
        };
        let rows = render(&no_sessions, 80, 12);
        assert!(rows.iter().any(|r| r.contains("No active sessions")), "{rows:#?}");
    }
}
