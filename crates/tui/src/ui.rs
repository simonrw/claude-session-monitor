//! The render and key seam: turn an [`AppState`] snapshot into a ratatui
//! frame, and route key presses into state changes.
//!
//! [`draw`] is a pure function of ([`AppState`], `now`, `home`): it reads no
//! clock and no environment, so a `TestBackend` test can feed it fixtures and
//! assert on the rendered buffer (section ordering, row content, truncation).
//! All presentation decisions - partitioning, staleness/fade, status label and
//! colour, cwd/remote shortening, relative time - come from
//! [`common::presentation`] rather than being re-derived here. Key routing
//! ([`AppState::handle_key_with`]) is parameterised over its two side effects
//! (activation, deletion), so the same tests can inject key events and assert
//! the resulting frame.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use common::activation::ActivationError;
use common::api::{HostStatus, SessionView};
use common::presentation;
use common::session::Status;
use common::view_model::MenuBarSummary;
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

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
    /// The keyboard cursor, tracked by session identity (not row index) so it
    /// survives live reorders. `None` when nothing is selected (empty list).
    pub selected: Option<String>,
    /// This host's name, used to decide local vs remote activation. Filled at
    /// startup from [`common::hostname::resolve`]; empty in tests (which never
    /// reach the local/remote branch).
    pub local_hostname: String,
    /// Per-session activation failures, keyed by `session_id`, rendered inline
    /// against the affected row. Mirrors the GUI's map: an entry is cleared on
    /// that session's next successful activation, and the whole map is dropped
    /// when a fresh session list arrives (see [`set_sessions`]).
    pub activation_errors: HashMap<String, String>,
    /// The in-flight delete confirmation, if any. `Some` while the modal is
    /// open; the modal captures the target's id and a human label at the moment
    /// it opens (so the prompt stays stable even if the list churns underneath).
    /// While this is `Some`, list navigation and activation are suspended -
    /// only confirm/cancel act (see [`AppState::handle_key_with`]).
    pub pending_delete: Option<PendingDelete>,
}

/// The target of an open delete-confirmation modal: the session to delete and
/// the label shown in the prompt. Mirrors the GUI's confirm-before-delete flow.
#[derive(Clone)]
pub struct PendingDelete {
    /// The `session_id` passed to [`CoreHandle::delete_session`] on confirm.
    pub session_id: String,
    /// The name (or shortened cwd) shown in the modal so the user knows what
    /// they are deleting.
    pub label: String,
}

impl AppState {
    /// The session ids in the order they render: the waiting section first,
    /// then the rest. Selection navigation walks this flattened order so the
    /// cursor crosses seamlessly between the two sections.
    fn ordered_ids(&self) -> Vec<String> {
        let (waiting, rest) = presentation::partition_sessions(&self.sessions);
        waiting
            .iter()
            .chain(rest.iter())
            .map(|s| s.session_id.clone())
            .collect()
    }

    /// Move the cursor by `delta` rows through the rendered order, wrapping at
    /// the ends. A no-op on an empty list.
    fn move_selection(&mut self, delta: isize) {
        let order = self.ordered_ids();
        if order.is_empty() {
            self.selected = None;
            return;
        }
        let len = order.len() as isize;
        let current = self
            .selected
            .as_deref()
            .and_then(|id| order.iter().position(|o| o == id))
            .map(|i| i as isize);
        let next = match current {
            Some(i) => (i + delta).rem_euclid(len),
            // No live selection: enter at the top (or bottom, moving up).
            None if delta >= 0 => 0,
            None => len - 1,
        };
        self.selected = Some(order[next as usize].clone());
    }

    /// Move the cursor to the next row (down).
    pub fn select_next(&mut self) {
        self.move_selection(1);
    }

    /// Move the cursor to the previous row (up).
    pub fn select_prev(&mut self) {
        self.move_selection(-1);
    }

    /// Replace the session list, keeping the cursor on the same session across
    /// reorders. When the selected session disappears the cursor clamps to the
    /// row that took its place (or the last row); an empty list clears it.
    pub fn set_sessions(&mut self, sessions: Vec<SessionView>) {
        // A fresh list can invalidate any stored error (targets may have moved),
        // so drop them all - the same reset the GUI does on `on_sessions_changed`.
        self.activation_errors.clear();
        let previous_order = self.ordered_ids();
        self.sessions = sessions;
        let new_order = self.ordered_ids();

        let still_present = self
            .selected
            .as_deref()
            .is_some_and(|id| new_order.iter().any(|o| o == id));
        if still_present {
            return;
        }

        // The selected session is gone. Clamp to its old position within the
        // new order so the cursor stays near where it was, rather than jumping.
        self.selected = self
            .selected
            .as_deref()
            .and_then(|id| previous_order.iter().position(|o| o == id))
            .map(|old_idx| old_idx.min(new_order.len().saturating_sub(1)))
            .and_then(|idx| new_order.get(idx).cloned())
            .or_else(|| new_order.first().cloned());
    }

    /// Activate a session: on success the session's stored error (if any)
    /// clears; on failure the error is recorded against its id. Parameterised
    /// over the activator so tests can inject a failure without touching real
    /// tmux/ssh. A no-op when nothing is selected.
    fn activate_selected_with<F>(&mut self, activate: F)
    where
        F: Fn(&SessionView, &str) -> Result<(), ActivationError>,
    {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(session) = self.sessions.iter().find(|s| s.session_id == id) else {
            return;
        };
        match activate(session, &self.local_hostname) {
            Ok(()) => {
                self.activation_errors.remove(&id);
            }
            Err(e) => {
                self.activation_errors.insert(id, e.to_string());
            }
        }
    }

    /// Whether the delete-confirmation modal is currently open. When it is,
    /// navigation and activation keys are suspended.
    pub fn modal_open(&self) -> bool {
        self.pending_delete.is_some()
    }

    /// Open the delete-confirmation modal for the selected session, capturing
    /// its id and a display label. A no-op when nothing is selected (or the
    /// selection no longer resolves to a session). Never deletes on its own -
    /// deletion only happens on an explicit [`confirm_delete_with`].
    ///
    /// [`confirm_delete_with`]: AppState::confirm_delete_with
    pub fn open_delete_modal(&mut self, home: &str) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(session) = self.sessions.iter().find(|s| s.session_id == id) else {
            return;
        };
        let label = session
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| presentation::shorten_cwd(&session.cwd, home));
        self.pending_delete = Some(PendingDelete {
            session_id: id,
            label,
        });
    }

    /// Dismiss the modal with no effect.
    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    /// Confirm the pending delete: hand the target's id to `delete` and close
    /// the modal. Parameterised over the deleter so tests can assert the call
    /// without a real [`CoreHandle`]. A no-op when no modal is open. The row
    /// disappears on the next server broadcast - nothing is removed locally.
    ///
    /// [`CoreHandle`]: common::view_model::CoreHandle
    pub fn confirm_delete_with<F>(&mut self, delete: F)
    where
        F: FnOnce(String),
    {
        if let Some(pending) = self.pending_delete.take() {
            delete(pending.session_id);
        }
    }

    /// Apply a (non-quit) key press. Arrow keys and j/k move the selection
    /// cursor; Enter activates; `d` opens the delete-confirmation modal.
    ///
    /// While the modal is open every other key is suspended: only `y` (confirm)
    /// and `n`/Esc (cancel) act. This is the confirm-before-delete guarantee -
    /// no key deletes without first opening the modal and then confirming it.
    ///
    /// Parameterised over the two side effects a key can trigger: `run` binds
    /// the real ones ([`common::activation::activate`],
    /// [`CoreHandle::delete_session`]), while tests inject key events and
    /// assert the resulting frame.
    ///
    /// [`CoreHandle::delete_session`]: common::view_model::CoreHandle::delete_session
    pub fn handle_key_with<A, D>(&mut self, code: KeyCode, home: &str, activate: A, delete: D)
    where
        A: Fn(&SessionView, &str) -> Result<(), ActivationError>,
        D: FnOnce(String),
    {
        if self.modal_open() {
            match code {
                KeyCode::Char('y') => self.confirm_delete_with(delete),
                KeyCode::Char('n') | KeyCode::Esc => self.cancel_delete(),
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Enter => self.activate_selected_with(activate),
            KeyCode::Char('d') => self.open_delete_modal(home),
            _ => {}
        }
    }
}

/// Map a UI-agnostic [`presentation::Rgb`] identity to ratatui's [`Color`].
fn to_color(rgb: presentation::Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// The waiting accent - the header count, the section border and its title all
/// use it. Taken from [`presentation::status_color`] rather than a literal so
/// the "needs you" red is defined in exactly one place across every frontend.
fn waiting_color() -> Color {
    to_color(presentation::status_color(&Status::Waiting {
        detail: None,
    }))
}

/// The header line: title, connection indicator, then waiting/busy counts.
fn header_line(state: &AppState) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "csm",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if state.connected {
        spans.push(Span::styled(
            "  \u{25cf} connected",
            Style::default().fg(Color::Green),
        ));
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
/// Which list section a row belongs to, so the selection cursor can tint the
/// two sections differently (per the PRO-222 prototype).
#[derive(Clone, Copy)]
enum Section {
    Waiting,
    Rest,
}

/// The background tint of the selected row. Warm for the waiting section, cool
/// for the rest, so the cursor stands out against both coloured and dimmed rows
/// without colliding with any status colour (the PRO-222 prototype's design).
fn selection_bg(section: Section) -> Color {
    match section {
        Section::Waiting => Color::Rgb(60, 50, 10),
        Section::Rest => Color::Rgb(40, 40, 60),
    }
}

fn session_row(
    session: &SessionView,
    now: DateTime<Utc>,
    home: &str,
    connected: bool,
    selected: bool,
    section: Section,
    error: Option<&str>,
) -> Line<'static> {
    let stale = presentation::is_stale(session.updated_at, now);
    let has_tmux = session.tmux_target.is_some();
    // Faded (disconnected or stale) sessions read as dimmed: the whole row
    // collapses to a single muted grey. This is deliberately distinct from the
    // no-tmux de-emphasis below - a fresh, jumpable-but-elsewhere session keeps
    // its status colour, while a stale one loses it, so the two conditions stay
    // legible apart (the dimming treatment chosen by the PRO-222 prototype).
    let dimmed = presentation::should_fade(connected, stale);
    let dim = |c: Color| if dimmed { Color::DarkGray } else { c };

    let mut spans: Vec<Span> = Vec::new();

    // The cursor glyph leads every row; a blank keeps unselected rows aligned.
    spans.push(Span::styled(
        if selected { "\u{25b6} " } else { "  " },
        Style::default().add_modifier(Modifier::BOLD),
    ));

    // The `/rename` label, when set, sits ahead of the location as the
    // user-chosen name of intent (PRO-215).
    if let Some(name) = session.name.as_deref().filter(|n| !n.is_empty()) {
        spans.push(Span::styled(
            format!("[{name}] "),
            Style::default()
                .fg(dim(Color::Magenta))
                .add_modifier(Modifier::BOLD),
        ));
    }

    if let Some(host) = &session.hostname {
        spans.push(Span::styled(
            format!("{host}:"),
            Style::default().fg(dim(Color::Green)),
        ));
    }

    let cwd_short = presentation::shorten_cwd(&session.cwd, home);
    spans.push(Span::styled(
        cwd_short,
        Style::default().fg(dim(Color::Reset)),
    ));

    if let Some(branch) = &session.git_branch {
        let remote = session
            .git_remote
            .as_deref()
            .map(presentation::strip_git_remote);
        let vcs = match remote {
            Some(remote) => format!(" {branch}@{remote}"),
            None => format!(" {branch}"),
        };
        spans.push(Span::styled(vcs, Style::default().fg(dim(Color::Blue))));
    }

    let status = presentation::status_label(&session.status);
    let status_color = to_color(presentation::status_color(&session.status));
    spans.push(Span::styled(
        format!("  {status}"),
        Style::default().fg(dim(status_color)),
    ));

    // Sessions with no tmux target can't be jumped to. Rather than dimming the
    // whole row (which would collide with the stale/disconnected treatment), we
    // tag them with a muted "no target" glyph so the reason they're inert stays
    // distinguishable from mere staleness.
    if !has_tmux {
        spans.push(Span::styled(
            " \u{2297}",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let rel = presentation::relative_time(session.updated_at, now);
    spans.push(Span::styled(
        format!("  {rel}"),
        Style::default().fg(Color::DarkGray),
    ));

    // A failed activation surfaces inline on its own row, in the same red the
    // GUI uses, so the error stays attached to the session it belongs to rather
    // than floating as a global banner.
    if let Some(err) = error {
        spans.push(Span::styled(
            format!("  \u{26a0} {err}"),
            Style::default().fg(Color::Rgb(220, 80, 80)),
        ));
    }

    let line = Line::from(spans);
    if selected {
        line.style(Style::default().bg(selection_bg(section)))
    } else {
        line
    }
}

/// Centered two-line empty-state message inside `area`.
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
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
            .constraints([
                Constraint::Length(waiting.len() as u16 + 2),
                Constraint::Min(0),
            ])
            .split(area)
    };

    let is_selected = |s: &SessionView| state.selected.as_deref() == Some(s.session_id.as_str());
    let error_of = |s: &SessionView| {
        state
            .activation_errors
            .get(&s.session_id)
            .map(String::as_str)
    };

    if !waiting.is_empty() {
        let lines: Vec<Line> = waiting
            .iter()
            .map(|s| {
                session_row(
                    s,
                    now,
                    home,
                    state.connected,
                    is_selected(s),
                    Section::Waiting,
                    error_of(s),
                )
            })
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
        .map(|s| {
            session_row(
                s,
                now,
                home,
                state.connected,
                is_selected(s),
                Section::Rest,
                error_of(s),
            )
        })
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

    let header =
        Paragraph::new(header_line(state)).block(Block::default().borders(Borders::BOTTOM));
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
        " \u{2191}/\u{2193} j/k move   \u{21b5} activate   d delete   q quit",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(help, outer[2]);

    // The confirmation modal draws last so it overlays everything else.
    if let Some(pending) = &state.pending_delete {
        draw_delete_modal(frame, pending);
    }
}

/// A [`Rect`] of the given size centered within `area`, clamped to fit.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

/// The delete-confirmation overlay: a centered, red-bordered box naming the
/// target session and its two actions. Mirrors the PRO-222 prototype's modal.
fn draw_delete_modal(frame: &mut Frame, pending: &PendingDelete) {
    let area = centered_rect(52, 7, frame.area());
    // Clear the cells underneath so the list doesn't bleed through the box.
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(" Delete session ");
    let lines = vec![
        Line::raw(""),
        Line::from(format!("Delete \"{}\"?", pending.label)).centered(),
        Line::raw(""),
        Line::from(Span::styled(
            "y confirm   Esc/n cancel",
            Style::default().fg(Color::DarkGray),
        ))
        .centered(),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
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

    /// Draw `state` at the given size and hand back the raw cell buffer - the
    /// single source of truth for "how we rasterise state", shared by the
    /// string view ([`render`]) and the cell-style view ([`fg_of`]).
    fn render_buffer(state: &AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, state, now(), "/Users/me"))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Render `state` at the given size and return the buffer as one string per
    /// row (trailing whitespace trimmed).
    fn render(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let buffer = render_buffer(state, width, height);
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

    /// The foreground colour of the first cell of `needle` in the rendered
    /// buffer - how the colour/dim/de-emphasis decisions are asserted, since
    /// those decisions live in cell styles the string view of `render` drops.
    fn fg_of(state: &AppState, width: u16, height: u16, needle: &str) -> Color {
        let buffer = render_buffer(state, width, height);
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            if let Some(byte_idx) = row.find(needle) {
                let x = row[..byte_idx].chars().count() as u16;
                return buffer[(x, y)].fg;
            }
        }
        panic!("expected {needle:?} in the rendered buffer");
    }

    fn one_session_state(s: SessionView, connected: bool) -> AppState {
        AppState {
            sessions: vec![s],
            connected,
            summary: MenuBarSummary {
                busy: 1,
                waiting: 0,
            },
            ..Default::default()
        }
    }

    #[test]
    fn status_uses_the_shared_colour_identity() {
        let state = one_session_state(
            session(
                "s",
                Status::Busy {
                    tool: Some("Bash".into()),
                },
            ),
            true,
        );
        // Busy green from `presentation::status_color`, mapped to a ratatui Rgb.
        // "busy(Bash)" is unique to the row (the header only ever says "N busy").
        assert_eq!(
            fg_of(&state, 100, 10, "busy(Bash)"),
            Color::Rgb(80, 200, 120)
        );
    }

    #[test]
    fn shell_and_idle_get_their_own_identities() {
        let shell = one_session_state(session("s", Status::Shell), true);
        assert_eq!(fg_of(&shell, 100, 10, "shell"), Color::Rgb(70, 170, 190));
        let idle = one_session_state(session("i", Status::Idle), true);
        assert_eq!(fg_of(&idle, 100, 10, "idle"), Color::Rgb(140, 150, 190));
    }

    #[test]
    fn stale_session_is_dimmed() {
        let mut s = session(
            "s",
            Status::Busy {
                tool: Some("Bash".into()),
            },
        );
        s.updated_at = now() - chrono::Duration::minutes(31);
        let state = one_session_state(s, true);
        assert_eq!(fg_of(&state, 100, 10, "busy(Bash)"), Color::DarkGray);
    }

    #[test]
    fn disconnected_dims_every_session() {
        // Fresh session, but the client is disconnected so its freshness is
        // suspect: it dims even though it is not itself stale.
        let state = one_session_state(
            session(
                "s",
                Status::Busy {
                    tool: Some("Bash".into()),
                },
            ),
            false,
        );
        assert_eq!(fg_of(&state, 100, 10, "busy(Bash)"), Color::DarkGray);
    }

    #[test]
    fn no_tmux_target_is_de_emphasised_not_dimmed() {
        let mut s = session(
            "s",
            Status::Busy {
                tool: Some("Bash".into()),
            },
        );
        s.tmux_target = None;
        let state = one_session_state(s, true);
        // The status keeps its live colour - de-emphasis is a distinct signal...
        assert_eq!(
            fg_of(&state, 100, 10, "busy(Bash)"),
            Color::Rgb(80, 200, 120)
        );
        // ...carried by a muted "no target" glyph that the dimming lacks.
        let rows = render(&state, 100, 10);
        assert!(
            rows.iter().any(|r| r.contains('\u{2297}')),
            "expected the no-tmux glyph: {rows:#?}"
        );
        assert_eq!(fg_of(&state, 100, 10, "\u{2297}"), Color::DarkGray);
    }

    #[test]
    fn waiting_and_ended_get_their_own_identities() {
        // Waiting red and Ended grey - the two identities the other colour
        // tests don't reach, both enumerated by the spec. A `waiting(detail)`
        // needle avoids the header's "N waiting" count and the section title.
        let waiting = one_session_state(
            session(
                "w",
                Status::Waiting {
                    detail: Some("Approve?".into()),
                },
            ),
            true,
        );
        assert_eq!(
            fg_of(&waiting, 100, 10, "waiting(Approve?)"),
            Color::Rgb(220, 80, 80)
        );
        let ended = one_session_state(session("e", Status::Ended), true);
        assert_eq!(fg_of(&ended, 100, 10, "ended"), Color::Rgb(160, 160, 160));
    }

    #[test]
    fn stale_and_no_tmux_together_still_shows_the_glyph() {
        // When a session is both dimmed and un-jumpable the row collapses to
        // grey, so the de-emphasis rides on the glyph's *presence* rather than a
        // contrasting colour (the treatment the PRO-222 prototype chose).
        let mut s = session(
            "s",
            Status::Busy {
                tool: Some("Bash".into()),
            },
        );
        s.updated_at = now() - chrono::Duration::minutes(31);
        s.tmux_target = None;
        let state = one_session_state(s, true);
        assert_eq!(fg_of(&state, 100, 10, "busy(Bash)"), Color::DarkGray);
        let rows = render(&state, 100, 10);
        assert!(
            rows.iter().any(|r| r.contains('\u{2297}')),
            "the no-tmux glyph survives dimming: {rows:#?}"
        );
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
            summary: MenuBarSummary {
                busy: 1,
                waiting: 1,
            },
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
        let mut s = session(
            "s",
            Status::Busy {
                tool: Some("Bash".into()),
            },
        );
        s.name = Some("api-server".into());
        s.hostname = Some("buildbox".into());
        s.git_branch = Some("tui".into());
        s.git_remote = Some("https://github.com/me/proj.git".into());
        let state = AppState {
            sessions: vec![s],
            connected: true,
            summary: MenuBarSummary {
                busy: 1,
                waiting: 0,
            },
            ..Default::default()
        };
        let rows = render(&state, 120, 20);
        let row = &rows[row_index(&rows, "[api-server]")];
        assert!(row.contains("[api-server]"), "name label: {row}");
        assert!(row.contains("buildbox:"), "hostname: {row}");
        assert!(row.contains("~/dev/project"), "shortened cwd: {row}");
        assert!(
            row.contains("tui@me/proj"),
            "branch and stripped remote: {row}"
        );
        assert!(row.contains("busy(Bash)"), "status label: {row}");
        assert!(row.contains("0s ago"), "relative time: {row}");
    }

    #[test]
    fn header_shows_connection_and_counts() {
        let state = AppState {
            connected: true,
            summary: MenuBarSummary {
                busy: 2,
                waiting: 3,
            },
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
            summary: MenuBarSummary {
                busy: 1,
                waiting: 0,
            },
            ..Default::default()
        };
        let width = 40;
        let rows = render(&state, width, 20);
        // No row exceeds the terminal width, and the tail of the content is cut.
        assert!(rows.iter().all(|r| r.chars().count() <= width as usize));
        let row = &rows[row_index(&rows, "~/dev/a-really-long")];
        assert!(
            !row.contains("enormous-repository-name"),
            "tail truncated: {row}"
        );
    }

    /// The background colour of the first cell of `needle` in the rendered
    /// buffer - how the selection tint (a background, not a foreground) is
    /// asserted.
    fn bg_of(state: &AppState, width: u16, height: u16, needle: &str) -> Color {
        let buffer = render_buffer(state, width, height);
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            if let Some(byte_idx) = row.find(needle) {
                let x = row[..byte_idx].chars().count() as u16;
                return buffer[(x, y)].bg;
            }
        }
        panic!("expected {needle:?} in the rendered buffer");
    }

    /// The cursor glyph the selected row carries.
    const CURSOR: &str = "\u{25b6}";

    fn two_section_state() -> AppState {
        AppState {
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
            summary: MenuBarSummary {
                busy: 1,
                waiting: 1,
            },
            ..Default::default()
        }
    }

    /// Inject a key press through the key seam with inert activation/delete
    /// effects - how the key-handling tests drive the app, mirroring `run`'s
    /// wiring without a CoreHandle or real tmux/ssh.
    fn press(state: &mut AppState, code: KeyCode) {
        state.handle_key_with(code, "/Users/me", |_, _| Ok(()), |_| {});
    }

    #[test]
    fn arrows_and_jk_move_the_cursor_across_both_sections() {
        let mut state = two_section_state();
        // The first key down enters at the top of the render order: the
        // waiting section (which sorts above the rest).
        press(&mut state, KeyCode::Down);
        let rows = render(&state, 100, 20);
        let cursor_row = row_index(&rows, CURSOR);
        assert!(
            rows[cursor_row].contains("waiting(Approve?)"),
            "cursor starts in the waiting section: {rows:#?}"
        );
        // Down again (via j) crosses seamlessly into the rest section.
        press(&mut state, KeyCode::Char('j'));
        let rows = render(&state, 100, 20);
        assert!(
            rows[row_index(&rows, CURSOR)].contains("busy"),
            "cursor crossed into the rest section: {rows:#?}"
        );
        // Up (via k) crosses back into the waiting section.
        press(&mut state, KeyCode::Char('k'));
        let rows = render(&state, 100, 20);
        assert!(
            rows[row_index(&rows, CURSOR)].contains("waiting(Approve?)"),
            "cursor crossed back into the waiting section: {rows:#?}"
        );
        // Up from the top row wraps to the bottom.
        press(&mut state, KeyCode::Up);
        let rows = render(&state, 100, 20);
        assert!(
            rows[row_index(&rows, CURSOR)].contains("busy"),
            "cursor wrapped to the last row: {rows:#?}"
        );
    }

    #[test]
    fn selected_row_stands_out_with_a_section_tint() {
        let mut state = two_section_state();
        // Waiting row selected: warm tint, and it survives the row's colours.
        state.selected = Some("wait".into());
        assert_eq!(
            bg_of(&state, 100, 20, "waiting(Approve?)"),
            Color::Rgb(60, 50, 10)
        );
        // Rest row selected: a distinct cool tint.
        state.selected = Some("busy".into());
        assert_eq!(
            bg_of(&state, 100, 20, "project  busy"),
            Color::Rgb(40, 40, 60)
        );
    }

    #[test]
    fn selection_follows_its_session_across_a_reorder() {
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        // The busy session is now waiting and a new busy session appears; the
        // list order changes underneath the cursor, but it tracks "busy".
        state.set_sessions(vec![
            session("busy", Status::Waiting { detail: None }),
            session("fresh", Status::Busy { tool: None }),
        ]);
        assert_eq!(state.selected.as_deref(), Some("busy"));
        let rows = render(&state, 100, 20);
        assert!(
            rows[row_index(&rows, CURSOR)].contains("waiting"),
            "cursor followed 'busy' into the waiting section: {rows:#?}"
        );
    }

    #[test]
    fn selection_clamps_when_its_session_disappears() {
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        // 'busy' is removed; the cursor clamps to a surviving row.
        state.set_sessions(vec![session(
            "wait",
            Status::Waiting {
                detail: Some("Approve?".into()),
            },
        )]);
        assert_eq!(state.selected.as_deref(), Some("wait"));

        // Emptying the list clears the cursor without panicking.
        state.set_sessions(vec![]);
        assert_eq!(state.selected, None);

        // Navigating an empty list is a harmless no-op.
        state.select_next();
        state.select_prev();
        assert_eq!(state.selected, None);
    }

    /// Press Enter with an activator that fails - no real tmux/ssh.
    fn press_enter_failing(state: &mut AppState) {
        state.handle_key_with(
            KeyCode::Enter,
            "/Users/me",
            |_, _| Err(ActivationError::NoTmuxTarget),
            |_| {},
        );
    }

    #[test]
    fn activation_failure_renders_inline_against_its_session() {
        // Inject a failure through the Enter key and the activator seam, and
        // assert the message lands on the affected session's row.
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        press_enter_failing(&mut state);

        assert_eq!(
            state.activation_errors.get("busy").map(String::as_str),
            Some("session has no tmux target"),
        );

        let rows = render(&state, 100, 20);
        let row = &rows[row_index(&rows, "session has no tmux target")];
        assert!(
            row.contains("project  busy"),
            "the error sits on the busy session's own row: {rows:#?}"
        );
        // Rendered in the shared error red.
        assert_eq!(
            fg_of(&state, 100, 20, "session has no tmux target"),
            Color::Rgb(220, 80, 80),
        );
    }

    #[test]
    fn successful_activation_clears_a_prior_error() {
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        press_enter_failing(&mut state);
        assert!(state.activation_errors.contains_key("busy"));

        // A later success on the same session drops its stored error (the
        // `press` helper's activator succeeds).
        press(&mut state, KeyCode::Enter);
        assert!(state.activation_errors.is_empty());
    }

    #[test]
    fn a_fresh_session_list_clears_activation_errors() {
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        press_enter_failing(&mut state);
        assert!(state.activation_errors.contains_key("busy"));

        state.set_sessions(vec![session("busy", Status::Busy { tool: None })]);
        assert!(state.activation_errors.is_empty());
    }

    #[test]
    fn enter_with_no_selection_is_a_noop() {
        let mut state = two_section_state();
        state.selected = None;
        press_enter_failing(&mut state);
        assert!(state.activation_errors.is_empty());
    }

    #[test]
    fn d_opens_the_modal_naming_the_selected_session() {
        let mut s = session("busy", Status::Busy { tool: None });
        s.name = Some("api-server".into());
        let mut state = one_session_state(s, true);
        state.selected = Some("busy".into());
        assert!(!state.modal_open());

        press(&mut state, KeyCode::Char('d'));
        assert!(state.modal_open());
        let rows = render(&state, 100, 20);
        assert!(
            rows.iter().any(|r| r.contains("Delete \"api-server\"?")),
            "the modal names the target session: {rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("y confirm")),
            "the modal shows its actions: {rows:#?}"
        );
    }

    #[test]
    fn delete_modal_falls_back_to_the_cwd_when_unnamed() {
        let mut state = one_session_state(session("busy", Status::Busy { tool: None }), true);
        state.selected = Some("busy".into());
        press(&mut state, KeyCode::Char('d'));
        let rows = render(&state, 100, 20);
        assert!(
            rows.iter().any(|r| r.contains("Delete \"~/dev/project\"?")),
            "unnamed sessions are named by their shortened cwd: {rows:#?}"
        );
    }

    #[test]
    fn d_with_no_selection_is_a_noop() {
        let mut state = two_section_state();
        state.selected = None;
        press(&mut state, KeyCode::Char('d'));
        assert!(!state.modal_open());
    }

    #[test]
    fn esc_and_n_both_cancel_the_modal_without_deleting() {
        let mut state = one_session_state(session("busy", Status::Busy { tool: None }), true);
        state.selected = Some("busy".into());

        press(&mut state, KeyCode::Char('d'));
        assert!(state.modal_open());
        let deleted = std::cell::Cell::new(false);
        state.handle_key_with(
            KeyCode::Esc,
            "/Users/me",
            |_, _| Ok(()),
            |_| deleted.set(true),
        );
        assert!(!state.modal_open());

        press(&mut state, KeyCode::Char('d'));
        assert!(state.modal_open());
        state.handle_key_with(
            KeyCode::Char('n'),
            "/Users/me",
            |_, _| Ok(()),
            |_| deleted.set(true),
        );
        assert!(!state.modal_open());

        // Cancel never reaches the delete seam, and the session is untouched.
        assert!(!deleted.get());
        assert_eq!(state.sessions.len(), 1);
    }

    #[test]
    fn y_confirms_the_delete_with_the_target_id() {
        let mut state = one_session_state(session("busy", Status::Busy { tool: None }), true);
        state.selected = Some("busy".into());
        press(&mut state, KeyCode::Char('d'));

        // Inject a recorder through the deleter seam - no real CoreHandle.
        let mut deleted: Option<String> = None;
        state.handle_key_with(
            KeyCode::Char('y'),
            "/Users/me",
            |_, _| Ok(()),
            |id| deleted = Some(id),
        );
        assert_eq!(deleted.as_deref(), Some("busy"));
        // The modal closes on confirm; the row lingers until the next broadcast.
        assert!(!state.modal_open());
        assert_eq!(state.sessions.len(), 1);
    }

    #[test]
    fn y_with_no_modal_open_never_deletes() {
        let mut state = one_session_state(session("busy", Status::Busy { tool: None }), true);
        state.selected = Some("busy".into());
        let mut called = false;
        state.handle_key_with(
            KeyCode::Char('y'),
            "/Users/me",
            |_, _| Ok(()),
            |_| called = true,
        );
        assert!(!called, "no delete happens without an open modal");
    }

    #[test]
    fn the_open_modal_suspends_every_other_key() {
        let mut state = two_section_state();
        state.selected = Some("wait".into());
        press(&mut state, KeyCode::Char('d'));
        assert!(state.modal_open());

        // Navigation keys no longer move the cursor.
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(state.selected.as_deref(), Some("wait"));

        // Enter no longer reaches the activator.
        let activated = std::cell::Cell::new(false);
        state.handle_key_with(
            KeyCode::Enter,
            "/Users/me",
            |_, _| {
                activated.set(true);
                Ok(())
            },
            |_| {},
        );
        assert!(
            !activated.get(),
            "activation is suspended while the modal is open"
        );

        // Unhandled keys leave the modal in place; Esc still closes it.
        press(&mut state, KeyCode::Char('x'));
        assert!(state.modal_open());
        press(&mut state, KeyCode::Esc);
        assert!(!state.modal_open());
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
        assert!(
            rows.iter().any(|r| r.contains("No watcher has reported")),
            "{rows:#?}"
        );

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
        assert!(
            rows.iter().any(|r| r.contains("No active sessions")),
            "{rows:#?}"
        );
    }
}
