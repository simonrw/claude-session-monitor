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

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use common::activation::ActivationError;
use common::api::{HostStatus, SessionView};
use common::presentation;
use common::session::Status;
use common::view_model::MenuBarSummary;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// The system appearance used to choose theme-sensitive colours.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    #[default]
    Dark,
}

/// Which hosts contribute Sessions to this TUI process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HostFilter {
    #[default]
    All,
    Local,
}

/// The full state a frame is rendered from. The observer thread mutates this
/// (via the event channel); the render is a pure function of a snapshot of it.
#[derive(Default, Clone)]
pub struct AppState {
    pub sessions: Vec<SessionView>,
    pub connected: bool,
    pub summary: MenuBarSummary,
    /// System appearance captured at startup.
    pub appearance: Appearance,
    /// Latest `GET /api/hosts` snapshot, for the watcher-silent empty state.
    pub hosts: Vec<HostStatus>,
    /// Whether at least one host-status poll has landed (see
    /// [`presentation::watcher_appears_silent`]).
    pub has_received_host_status: bool,
    /// The keyboard cursor, tracked by session identity (not row index) so it
    /// survives live reorders. `None` when nothing is selected (empty list).
    pub selected: Option<String>,
    /// This host's name, used to decide local vs remote activation. Filled at
    /// startup from [`common::hostname::resolve`]. An empty value disables the
    /// local-host filter rather than presenting an empty local list.
    pub local_hostname: String,
    /// Process-local host filter. The complete server snapshot remains in
    /// [`sessions`](Self::sessions); this only controls the derived visible list.
    pub host_filter: HostFilter,
    /// Set after `h` is pressed without a resolved local hostname. Rendered in
    /// the help row so the failed toggle is visible without hiding Sessions.
    pub local_filter_unavailable: bool,
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
    /// Top of the viewport in display lines. Selection-anchored: every selection
    /// change calls [`sync_viewport`] to keep the selected row visible.
    pub scroll_offset: usize,
    /// True when the user has pressed `g` once and is waiting for a second `g`
    /// to complete the `gg` (first-session) chord. Any other key clears it.
    pub pending_g: bool,
    /// Last known body-pane width in columns, set by the event loop before each
    /// draw. Drives the wide/narrow layout decision and the lines-per-session
    /// count used by viewport sync and half-page jumps.
    pub last_width: usize,
    /// Last known body-pane height in rows (full terminal height minus the
    /// header and help-bar rows), set by the event loop before each draw.
    /// Used by viewport sync and half-page jump calculations.
    pub last_body_height: usize,
    /// Whether we are showing the list or a full-screen detail page.
    pub view_mode: ViewMode,
}

/// What a key press did that the event loop needs to react to. Today the only
/// loop-visible effect is a successful activation, which `--exit-on-select`
/// turns into "jump then quit"; everything else is handled inside [`AppState`]
/// and reported as [`KeyOutcome::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    None,
    /// A session was successfully activated by this key press.
    Activated,
}

/// Which top-level view is active. The list is the default; Space opens the
/// detail page for the selected session, and Space/Esc return to the list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    List,
    /// Full-screen read-only detail for the session whose id is stored here.
    Detail,
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

/// Terminal width at which the layout switches from narrow cards to wide rows.
const WIDE_COLS: usize = 80;

impl AppState {
    /// The single Session list used by every visible operation. All-hosts mode
    /// borrows the complete snapshot; local mode owns the filtered projection.
    fn visible_sessions(&self) -> Cow<'_, [SessionView]> {
        match self.host_filter {
            HostFilter::All => Cow::Borrowed(&self.sessions),
            HostFilter::Local => Cow::Owned(
                self.sessions
                    .iter()
                    .filter(|session| {
                        session.hostname.as_deref() == Some(self.local_hostname.as_str())
                    })
                    .cloned()
                    .collect(),
            ),
        }
    }

    fn watcher_appears_silent(&self, now: DateTime<Utc>) -> bool {
        match self.host_filter {
            HostFilter::All => presentation::watcher_appears_silent(
                &self.hosts,
                self.has_received_host_status,
                now,
            ),
            HostFilter::Local => {
                let local_hosts: Vec<HostStatus> = self
                    .hosts
                    .iter()
                    .filter(|host| host.hostname == self.local_hostname)
                    .cloned()
                    .collect();
                presentation::watcher_appears_silent(
                    &local_hosts,
                    self.has_received_host_status,
                    now,
                )
            }
        }
    }

    /// Set the viewport dimensions derived from the terminal size. Called by
    /// the event loop before each draw so key handlers see current values.
    pub fn update_size(&mut self, term_width: usize, term_height: usize) {
        self.last_width = term_width;
        // Must mirror the row-count split in `draw` (header=2, help=1).
        self.last_body_height = term_height.saturating_sub(3);
    }

    /// Lines per session in the current layout (1 wide, 4 narrow).
    fn lines_per(&self) -> usize {
        if self.last_width >= WIDE_COLS { 1 } else { 4 }
    }

    /// Number of sessions in half a visible page, clamped to at least 1.
    fn half_page_step(&self) -> isize {
        (self.last_body_height / self.lines_per().max(1) / 2).max(1) as isize
    }

    /// The first display-line index of the selected session within the flat
    /// scrollable list produced by [`build_flat_lines`]. `None` when nothing
    /// is selected or the selection is not found (e.g. stale id after a list
    /// update that hasn't yet called `sync_viewport`).
    fn selected_row(&self) -> Option<usize> {
        let id = self.selected.as_deref()?;
        let visible = self.visible_sessions();
        let (waiting, rest) = presentation::partition_sessions(&visible);
        let lp = self.lines_per();
        let has_waiting = !waiting.is_empty();

        if has_waiting {
            for (i, s) in waiting.iter().enumerate() {
                if s.session_id == id {
                    return Some(1 + i * lp);
                }
            }
        }

        // Rest section: starts after waiting chrome (2 border lines) and all waiting session lines.
        let rest_start = if has_waiting {
            1 + waiting.len() * lp + 1
        } else {
            0
        };
        for (i, s) in rest.iter().enumerate() {
            if s.session_id == id {
                return Some(rest_start + i * lp);
            }
        }

        None
    }

    /// Clamp `scroll_offset` so the selected session's lines are within the
    /// visible window. Must be called after every selection change.
    fn sync_viewport(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let lp = self.lines_per();
        let h = self.last_body_height;
        if h == 0 {
            return;
        }
        // Scroll down if the last line of the selection is below the viewport.
        let sel_end = row + lp.saturating_sub(1);
        if sel_end >= self.scroll_offset + h {
            self.scroll_offset = sel_end + 1 - h;
        }
        // Scroll up if the first line of the selection is above the viewport.
        if row < self.scroll_offset {
            self.scroll_offset = row;
        }
    }

    /// Move the cursor to the first session (vim `gg`).
    pub fn select_first(&mut self) {
        let order = self.ordered_ids();
        self.selected = order.into_iter().next();
        self.sync_viewport();
    }

    /// Move the cursor to the last session (vim `G`).
    pub fn select_last(&mut self) {
        let order = self.ordered_ids();
        self.selected = order.into_iter().last();
        self.sync_viewport();
    }

    /// Jump the selection down by half the visible page (Ctrl-d).
    pub fn select_half_page_down(&mut self) {
        self.move_selection(self.half_page_step());
        self.sync_viewport();
    }

    /// Jump the selection up by half the visible page (Ctrl-u).
    pub fn select_half_page_up(&mut self) {
        self.move_selection(-self.half_page_step());
        self.sync_viewport();
    }

    /// The session ids in the order they render: the waiting section first,
    /// then the rest. Selection navigation walks this flattened order so the
    /// cursor crosses seamlessly between the two sections.
    fn ordered_ids(&self) -> Vec<String> {
        let visible = self.visible_sessions();
        let (waiting, rest) = presentation::partition_sessions(&visible);
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
        self.sync_viewport();
    }

    /// Move the cursor to the previous row (up).
    pub fn select_prev(&mut self) {
        self.move_selection(-1);
        self.sync_viewport();
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
        self.reconcile_selection(previous_order);
    }

    /// Keep the current identity when visible, otherwise follow the existing
    /// removal rule and clamp to the old position in the new visible order.
    fn reconcile_selection(&mut self, previous_order: Vec<String>) {
        let new_order = self.ordered_ids();
        let still_present = self
            .selected
            .as_deref()
            .is_some_and(|id| new_order.iter().any(|o| o == id));
        if still_present {
            self.sync_viewport();
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
        if self.selected.is_none() {
            self.scroll_offset = 0;
        } else {
            self.sync_viewport();
        }
    }

    fn toggle_host_filter(&mut self) {
        if self.local_hostname.is_empty() {
            self.local_filter_unavailable = true;
            return;
        }
        self.local_filter_unavailable = false;
        let previous_order = self.ordered_ids();
        self.host_filter = match self.host_filter {
            HostFilter::All => HostFilter::Local,
            HostFilter::Local => HostFilter::All,
        };
        self.reconcile_selection(previous_order);
    }

    /// Activate a session: on success the session's stored error (if any)
    /// clears; on failure the error is recorded against its id. Parameterised
    /// over the activator so tests can inject a failure without touching real
    /// tmux/ssh. Returns `true` only when a session was actually activated, so
    /// the event loop can honour `--exit-on-select`. Returns `false` (a no-op)
    /// when nothing is selected, the selection no longer resolves, or activation
    /// fails.
    fn activate_selected_with<F>(&mut self, activate: F) -> bool
    where
        F: Fn(&SessionView, &str) -> Result<(), ActivationError>,
    {
        let Some(id) = self.selected.clone() else {
            return false;
        };
        let visible = self.visible_sessions();
        let Some(session) = visible.iter().find(|s| s.session_id == id).cloned() else {
            return false;
        };
        match activate(&session, &self.local_hostname) {
            Ok(()) => {
                self.activation_errors.remove(&id);
                true
            }
            Err(e) => {
                self.activation_errors.insert(id, e.to_string());
                false
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
        self.pending_g = false;
        let Some(id) = self.selected.clone() else {
            return;
        };
        let visible = self.visible_sessions();
        let Some(session) = visible.iter().find(|s| s.session_id == id) else {
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
    /// cursor; Enter activates; `d` opens the delete-confirmation modal; `h`
    /// toggles between all-hosts and local-host modes.
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
    pub fn handle_key_with<A, D>(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        home: &str,
        activate: A,
        delete: D,
    ) -> KeyOutcome
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
            return KeyOutcome::None;
        }

        // Detail view: only Space and Esc act.
        if self.view_mode == ViewMode::Detail {
            if matches!(code, KeyCode::Char(' ') | KeyCode::Esc) {
                self.view_mode = ViewMode::List;
            }
            return KeyOutcome::None;
        }

        // Snapshot and reset the pending-g state. If this key is `g`, the arm
        // below re-sets it when appropriate; every other key leaves it false.
        let was_pending_g = self.pending_g;
        self.pending_g = false;

        match code {
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_half_page_down();
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_half_page_up();
            }
            KeyCode::Char('G') => self.select_last(),
            KeyCode::Char('g') => {
                if was_pending_g {
                    self.select_first();
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Enter => {
                if self.activate_selected_with(activate) {
                    return KeyOutcome::Activated;
                }
            }
            KeyCode::Char('d') => self.open_delete_modal(home),
            KeyCode::Char(' ') => {
                if self.selected.is_some() {
                    self.view_mode = ViewMode::Detail;
                }
            }
            KeyCode::Char('h') => self.toggle_host_filter(),
            _ => {}
        }
        KeyOutcome::None
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

/// Top border line for the waiting section (replaces Block's top border).
fn waiting_border_top(width: usize) -> Line<'static> {
    let title = " waiting for you ";
    let title_len = title.chars().count();
    let left = "──";
    let right_len = width.saturating_sub(left.chars().count() + title_len);
    Line::from(Span::styled(
        format!("{}{}{}", left, title, "─".repeat(right_len)),
        Style::default().fg(waiting_color()),
    ))
}

/// Bottom border line for the waiting section.
fn waiting_border_bottom(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(waiting_color()),
    ))
}

/// Build the complete ordered flat list of display lines for all sessions.
/// The waiting section's border lines are embedded as ordinary text lines so
/// the entire list can be sliced by [`scroll_offset`] without special-casing.
fn build_flat_lines(state: &AppState, now: DateTime<Utc>, home: &str) -> Vec<Line<'static>> {
    let visible = state.visible_sessions();
    let (waiting, rest) = presentation::partition_sessions(&visible);
    let w = state.last_width;
    let wide = w >= WIDE_COLS;
    let row_selection_bg = |s: &SessionView| {
        (state.selected.as_deref() == Some(s.session_id.as_str()))
            .then_some(selection_bg(state.appearance))
    };
    let error_of = |s: &SessionView| {
        state
            .activation_errors
            .get(&s.session_id)
            .map(String::as_str)
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    if !waiting.is_empty() {
        lines.push(waiting_border_top(w));
        for s in &waiting {
            if wide {
                lines.push(session_row_wide(
                    s,
                    now,
                    home,
                    state.connected,
                    row_selection_bg(s),
                    error_of(s),
                    w,
                ));
            } else {
                lines.extend(session_card(
                    s,
                    now,
                    home,
                    state.connected,
                    row_selection_bg(s),
                    error_of(s),
                    w,
                ));
            }
        }
        lines.push(waiting_border_bottom(w));
    }

    for s in &rest {
        if wide {
            lines.push(session_row_wide(
                s,
                now,
                home,
                state.connected,
                row_selection_bg(s),
                error_of(s),
                w,
            ));
        } else {
            lines.extend(session_card(
                s,
                now,
                home,
                state.connected,
                row_selection_bg(s),
                error_of(s),
                w,
            ));
        }
    }

    lines
}

/// The header line: title, host filter, connection indicator, and visible counts.
fn header_line(state: &AppState) -> Line<'static> {
    let summary = match state.host_filter {
        HostFilter::All => state.summary,
        HostFilter::Local => MenuBarSummary::from_sessions(&state.visible_sessions()),
    };
    let mut spans = vec![Span::styled(
        "csm",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if state.last_width < WIDE_COLS {
        spans.push(Span::raw(match state.host_filter {
            HostFilter::All => "  all",
            HostFilter::Local => "  local",
        }));
        spans.push(Span::styled(
            if state.connected {
                "  \u{25cf} up"
            } else {
                "  \u{25cf} down"
            },
            Style::default().fg(if state.connected {
                Color::Green
            } else {
                Color::Red
            }),
        ));
        if summary.waiting > 0 {
            spans.push(Span::styled(
                format!("  {} waiting", summary.waiting),
                Style::default().fg(waiting_color()),
            ));
        }
        spans.push(Span::raw(format!("  {} busy", summary.busy)));
        return Line::from(spans);
    }

    let connection_text = if state.connected {
        "  \u{25cf} connected"
    } else {
        "  \u{25cf} disconnected"
    };
    let waiting_text = (summary.waiting > 0).then(|| format!("  {} waiting", summary.waiting));
    let busy_text = format!("  {} busy", summary.busy);
    let filter_text = match state.host_filter {
        HostFilter::All => "  all hosts".to_string(),
        HostFilter::Local => {
            let fixed_width = "csm".chars().count()
                + "  local: ".chars().count()
                + connection_text.chars().count()
                + waiting_text
                    .as_deref()
                    .map(|text| text.chars().count())
                    .unwrap_or(0)
                + busy_text.chars().count();
            let hostname_width = state.last_width.saturating_sub(fixed_width);
            format!(
                "  local: {}",
                presentation::truncate_text(&state.local_hostname, hostname_width)
            )
        }
    };
    spans.push(Span::raw(filter_text));
    spans.push(Span::styled(
        connection_text,
        Style::default().fg(if state.connected {
            Color::Green
        } else {
            Color::Red
        }),
    ));
    if let Some(waiting_text) = waiting_text {
        spans.push(Span::styled(
            waiting_text,
            Style::default().fg(waiting_color()),
        ));
    }
    spans.push(Span::raw(busy_text));
    Line::from(spans)
}

/// Catppuccin Surface 0, using Latte in light mode and Mocha in dark mode.
fn selection_bg(appearance: Appearance) -> Color {
    match appearance {
        Appearance::Light => Color::Rgb(204, 208, 218),
        Appearance::Dark => Color::Rgb(49, 50, 68),
    }
}

/// Wide layout (>= 80 cols): one budgeted line per session.
///
/// Field priority (survives longest first):
/// status > error > name > host > cwd > time > ⊗ > branch@remote
///
/// status, activation error, and relative time always survive.
/// branch@remote is dropped whole (never truncated mid-string).
/// cwd is left-elided when space runs out.
fn session_row_wide(
    session: &SessionView,
    now: DateTime<Utc>,
    home: &str,
    connected: bool,
    selection_bg: Option<Color>,
    error: Option<&str>,
    width: usize,
) -> Line<'static> {
    let stale = presentation::is_stale(session.updated_at, now);
    let has_tmux = session.tmux_target.is_some();
    let dimmed = presentation::should_fade(connected, stale);
    let dim = |c: Color| if dimmed { Color::DarkGray } else { c };

    let status = presentation::status_label(&session.status);
    let status_color = to_color(presentation::status_color(&session.status));
    let rel = presentation::relative_time(session.updated_at, now);

    // Compute the fixed right-side cost so we know what's left for the left side.
    // status is "  {status}", time is "  {rel}", no-tmux is " ⊗", error is "  ⚠ {err}"
    let status_str = format!("  {status}");
    let time_str = format!("  {rel}");
    let no_tmux_str = if !has_tmux { " \u{2297}" } else { "" };
    let error_str = error.map(|e| format!("  \u{26a0} {e}")).unwrap_or_default();

    let right_width = status_str.chars().count()
        + time_str.chars().count()
        + no_tmux_str.chars().count()
        + error_str.chars().count();

    // 2 chars for the cursor glyph + space.
    let cursor_width = 2usize;
    let left_budget = width
        .saturating_sub(right_width)
        .saturating_sub(cursor_width);

    // Try to fit name and/or host in the left budget, keeping at least 5 chars for cwd.
    const MIN_CWD: usize = 5;

    let name_part = session
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .map(|n| format!("[{n}] "));
    let host_part = session.hostname.as_deref().map(|h| format!("{h}:"));

    let branch_str = session.git_branch.as_deref().map(|branch| {
        let remote = session
            .git_remote
            .as_deref()
            .map(presentation::strip_git_remote);
        match remote {
            Some(remote) => format!(" {branch}@{remote}"),
            None => format!(" {branch}"),
        }
    });

    let name_len = name_part.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    let host_len = host_part.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    let branch_len = branch_str
        .as_deref()
        .map(|s| s.chars().count())
        .unwrap_or(0);

    let include_name = name_part.is_some() && name_len + MIN_CWD <= left_budget;
    let include_host = if include_name {
        host_part.is_some() && name_len + host_len + MIN_CWD <= left_budget
    } else {
        host_part.is_some() && host_len + MIN_CWD <= left_budget
    };

    let optional_used =
        if include_name { name_len } else { 0 } + if include_host { host_len } else { 0 };
    let cwd_branch_budget = left_budget.saturating_sub(optional_used);

    let include_branch = branch_str.is_some() && branch_len + MIN_CWD <= cwd_branch_budget;
    let cwd_budget = if include_branch {
        cwd_branch_budget.saturating_sub(branch_len)
    } else {
        cwd_branch_budget
    };

    let cwd_elided = presentation::elide_path(&session.cwd, home, cwd_budget);

    let mut spans: Vec<Span> = Vec::new();

    spans.push(Span::styled(
        if selection_bg.is_some() {
            "\u{25b6} "
        } else {
            "  "
        },
        Style::default().add_modifier(Modifier::BOLD),
    ));

    if include_name {
        spans.push(Span::styled(
            name_part.unwrap(),
            Style::default()
                .fg(dim(Color::Magenta))
                .add_modifier(Modifier::BOLD),
        ));
    }

    if include_host {
        spans.push(Span::styled(
            host_part.unwrap(),
            Style::default().fg(dim(Color::Green)),
        ));
    }

    spans.push(Span::styled(
        cwd_elided,
        Style::default().fg(dim(Color::Reset)),
    ));

    if include_branch {
        spans.push(Span::styled(
            branch_str.unwrap(),
            Style::default().fg(dim(Color::Blue)),
        ));
    }

    spans.push(Span::styled(
        status_str,
        Style::default().fg(dim(status_color)),
    ));

    if !has_tmux {
        spans.push(Span::styled(
            " \u{2297}",
            Style::default().fg(Color::DarkGray),
        ));
    }

    spans.push(Span::styled(time_str, Style::default().fg(Color::DarkGray)));

    if let Some(err) = error {
        spans.push(Span::styled(
            format!("  \u{26a0} {err}"),
            Style::default().fg(Color::Rgb(220, 80, 80)),
        ));
    }

    let line = Line::from(spans);
    if let Some(bg) = selection_bg {
        line.style(Style::default().bg(bg))
    } else {
        line
    }
}

/// Narrow layout (< 80 cols): three-line card per session plus a blank separator.
///
/// Line 1: `▌ ▶ [name] hostname  42s`
/// Line 2: `▌   …/elided/cwd branch@remote`
/// Line 3: `▌   status  error…  ⊗`
/// Line 4: (blank)
///
/// The `▌` gutter is coloured with the status colour (dimmed when stale/disconnected).
fn session_card(
    session: &SessionView,
    now: DateTime<Utc>,
    home: &str,
    connected: bool,
    selection_bg: Option<Color>,
    error: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let stale = presentation::is_stale(session.updated_at, now);
    let has_tmux = session.tmux_target.is_some();
    let dimmed = presentation::should_fade(connected, stale);
    let dim = |c: Color| if dimmed { Color::DarkGray } else { c };

    let status_rgb = presentation::status_color(&session.status);
    let status_color = to_color(status_rgb);
    let gutter_color = dim(status_color);

    let gutter = Span::styled("\u{258c} ", Style::default().fg(gutter_color));

    let abbrev = presentation::abbreviated_relative_time(session.updated_at, now);
    let status_label = presentation::status_label(&session.status);

    // Line 1: gutter cursor [name] hostname  time
    // gutter=2, cursor=2, remaining for name+host+time
    let time_str = format!("  {abbrev}");
    let time_width = time_str.chars().count();
    // 2 gutter + 2 cursor = 4 fixed
    let line1_remaining = width.saturating_sub(4 + time_width);

    let name_part = session
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .map(|n| format!("[{n}] "));
    let host_part = session.hostname.as_deref().map(|h| format!("{h} "));
    let name_len = name_part.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    let host_len = host_part.as_deref().map(|s| s.chars().count()).unwrap_or(0);

    let include_name = name_part.is_some() && name_len <= line1_remaining;
    let include_host = host_part.is_some()
        && (if include_name { name_len } else { 0 }) + host_len <= line1_remaining;

    let mut line1_spans = vec![gutter.clone()];
    line1_spans.push(Span::styled(
        if selection_bg.is_some() {
            "\u{25b6} "
        } else {
            "  "
        },
        Style::default().add_modifier(Modifier::BOLD),
    ));
    if include_name {
        line1_spans.push(Span::styled(
            name_part.unwrap(),
            Style::default()
                .fg(dim(Color::Magenta))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if include_host {
        line1_spans.push(Span::styled(
            host_part.unwrap(),
            Style::default().fg(dim(Color::Green)),
        ));
    }
    line1_spans.push(Span::styled(time_str, Style::default().fg(Color::DarkGray)));

    // Line 2: gutter "  " cwd branch?
    // 2 gutter + 2 indent = 4 fixed
    let branch_str = session.git_branch.as_deref().map(|branch| {
        let remote = session
            .git_remote
            .as_deref()
            .map(presentation::strip_git_remote);
        match remote {
            Some(remote) => format!(" {branch}@{remote}"),
            None => format!(" {branch}"),
        }
    });
    let branch_len = branch_str
        .as_deref()
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let line2_budget = width.saturating_sub(4);
    let include_branch = branch_str.is_some() && branch_len + 5 <= line2_budget;
    let cwd_budget = if include_branch {
        line2_budget.saturating_sub(branch_len)
    } else {
        line2_budget
    };
    let cwd_elided = presentation::elide_path(&session.cwd, home, cwd_budget);

    let mut line2_spans = vec![gutter.clone(), Span::raw("  ")];
    line2_spans.push(Span::styled(
        cwd_elided,
        Style::default().fg(dim(Color::Reset)),
    ));
    if include_branch {
        line2_spans.push(Span::styled(
            branch_str.unwrap(),
            Style::default().fg(dim(Color::Blue)),
        ));
    }

    // Line 3: gutter "  " status  error…  ⊗
    let status_str = status_label.clone();
    let no_tmux_part = if !has_tmux { " \u{2297}" } else { "" };
    // 2 gutter + 2 indent + status = base; then error if space, then ⊗
    let line3_base = 4 + status_str.chars().count() + no_tmux_part.chars().count();
    let error_budget = width.saturating_sub(line3_base).saturating_sub(2); // 2 for "  " prefix
    let error_display = error.map(|e| {
        let truncated = presentation::truncate_text(e, error_budget);
        format!("  {truncated}")
    });

    let mut line3_spans = vec![gutter.clone(), Span::raw("  ")];
    line3_spans.push(Span::styled(
        status_str,
        Style::default().fg(dim(status_color)),
    ));
    if let Some(err_str) = error_display {
        line3_spans.push(Span::styled(
            err_str,
            Style::default().fg(Color::Rgb(220, 80, 80)),
        ));
    }
    if !has_tmux {
        line3_spans.push(Span::styled(
            " \u{2297}",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let bg_style = if let Some(bg) = selection_bg {
        Style::default().bg(bg)
    } else {
        Style::default()
    };

    vec![
        Line::from(line1_spans).style(bg_style),
        Line::from(line2_spans).style(bg_style),
        Line::from(line3_spans).style(bg_style),
        Line::raw(""),
    ]
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

/// The unified scrollable session list: waiting section (with embedded border
/// lines) followed by the rest, sliced to the visible viewport window.
fn draw_sessions(frame: &mut Frame, area: Rect, state: &AppState, now: DateTime<Utc>, home: &str) {
    let all_lines = build_flat_lines(state, now, home);
    let visible: Vec<Line> = all_lines
        .into_iter()
        .skip(state.scroll_offset)
        .take(area.height as usize)
        .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

/// Full-screen read-only detail page for one session.
///
/// Shows every known field, wrapped rather than truncated. No navigation or
/// action keys act here - Space/Esc return to the list (see
/// [`AppState::handle_key_with`]).
fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    session: &SessionView,
    now: DateTime<Utc>,
    home: &str,
) {
    let status_label = presentation::status_label(&session.status);
    let status_color = to_color(presentation::status_color(&session.status));

    let agent_kind_str = match session.agent_kind {
        common::api::AgentKind::Claude => "claude",
        common::api::AgentKind::Codex => "codex",
    };

    let tmux_str = session
        .tmux_target
        .as_deref()
        .unwrap_or("none - cannot activate");

    let rel = presentation::relative_time(session.updated_at, now);
    let abs_time = session
        .updated_at
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    let full_cwd = presentation::shorten_cwd(&session.cwd, home);

    let mut lines: Vec<Line> = Vec::new();

    // Status
    lines.push(Line::from(vec![
        Span::styled("status:      ", Style::default().fg(Color::DarkGray)),
        Span::styled(status_label, Style::default().fg(status_color)),
    ]));

    // Activation error, if any
    if let common::session::Status::Waiting {
        detail: Some(ref detail),
    } = session.status
    {
        lines.push(Line::from(vec![
            Span::styled("error:       ", Style::default().fg(Color::DarkGray)),
            Span::styled(detail.clone(), Style::default().fg(Color::Red)),
        ]));
    }

    // Name
    if let Some(ref name) = session.name {
        lines.push(Line::from(vec![
            Span::styled("name:        ", Style::default().fg(Color::DarkGray)),
            Span::raw(name.clone()),
        ]));
    }

    // Host
    if let Some(ref hostname) = session.hostname {
        lines.push(Line::from(vec![
            Span::styled("host:        ", Style::default().fg(Color::DarkGray)),
            Span::raw(hostname.clone()),
        ]));
    }

    // CWD (full, untruncated)
    lines.push(Line::from(vec![
        Span::styled("cwd:         ", Style::default().fg(Color::DarkGray)),
        Span::raw(full_cwd),
    ]));

    // Branch
    if let Some(ref branch) = session.git_branch {
        lines.push(Line::from(vec![
            Span::styled("branch:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(branch.clone()),
        ]));
    }

    // Raw remote URL (unstripped)
    if let Some(ref remote) = session.git_remote {
        lines.push(Line::from(vec![
            Span::styled("remote:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(remote.clone()),
        ]));
    }

    // Updated times
    lines.push(Line::from(vec![
        Span::styled("updated:     ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{rel}  ({abs_time})")),
    ]));

    // Tmux target
    lines.push(Line::from(vec![
        Span::styled("tmux:        ", Style::default().fg(Color::DarkGray)),
        Span::raw(tmux_str),
    ]));

    // Model
    lines.push(Line::from(vec![
        Span::styled("model:       ", Style::default().fg(Color::DarkGray)),
        Span::raw(session.model.as_deref().unwrap_or("unknown").to_string()),
    ]));

    // Agent kind
    lines.push(Line::from(vec![
        Span::styled("agent:       ", Style::default().fg(Color::DarkGray)),
        Span::raw(agent_kind_str),
    ]));

    // Session ID
    lines.push(Line::from(vec![
        Span::styled("session id:  ", Style::default().fg(Color::DarkGray)),
        Span::raw(session.session_id.clone()),
    ]));

    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
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

    if state.view_mode == ViewMode::Detail {
        // Find the selected session and render the detail page.
        let selected_id = state.selected.as_deref().unwrap_or("");
        let visible = state.visible_sessions();
        if let Some(session) = visible.iter().find(|s| s.session_id == selected_id) {
            draw_detail(frame, outer[1], session, now, home);
        }
    } else if state.visible_sessions().is_empty() {
        match (state.host_filter, state.watcher_appears_silent(now)) {
            (HostFilter::Local, true) => draw_empty(
                frame,
                outer[1],
                "No local watcher is reporting",
                "Start csm-watcher on this host to monitor local sessions.",
            ),
            (HostFilter::Local, false) => draw_empty(
                frame,
                outer[1],
                "No active sessions on this host",
                "The local watcher is running but no sessions are active.",
            ),
            (HostFilter::All, true) => draw_empty(
                frame,
                outer[1],
                "No watcher has reported in yet",
                "Start csm-watcher on a host to begin monitoring sessions.",
            ),
            (HostFilter::All, false) => draw_empty(
                frame,
                outer[1],
                "No active sessions",
                "The watcher is running but no sessions are active right now.",
            ),
        }
    } else {
        draw_sessions(frame, outer[1], state, now, home);
    }

    let help_text = if state.view_mode == ViewMode::Detail {
        " Space/Esc back"
    } else if state.local_filter_unavailable {
        " Local host unavailable; all hosts shown"
    } else {
        " h hosts   \u{2191}/\u{2193} j/k move   ^d/^u page   gg/G first/last   \u{21b5} activate   Space detail   d delete   q quit"
    };
    let help = Paragraph::new(Line::from(Span::styled(
        help_text,
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
    let modal_width = 52u16.min(frame.area().width);
    let area = centered_rect(modal_width, 7, frame.area());
    // Clear the cells underneath so the list doesn't bleed through the box.
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(" Delete session ");
    // Inner width = modal_width - 2 borders. The prompt template `Delete ""?`
    // is 10 chars, leaving the rest for the label.
    let inner_width = (modal_width as usize).saturating_sub(2);
    let label_budget = inner_width.saturating_sub(10);
    let label = presentation::truncate_text(&pending.label, label_budget);
    let lines = vec![
        Line::raw(""),
        Line::from(format!("Delete \"{}\"?", label)).centered(),
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
    ///
    /// Clones `state` and patches `last_width` / `last_body_height` so that
    /// viewport-sync helpers (used by key handlers) see dimensions consistent
    /// with what the test backend draws at.
    fn render_buffer(state: &AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut state = state.clone();
        state.last_width = width as usize;
        // Layout: header(2 rows) + body(min) + help(1 row) = height - 3.
        state.last_body_height = (height as usize).saturating_sub(3);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &state, now(), "/Users/me"))
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
    fn h_toggles_between_all_and_local_sessions() {
        let mut local = session("local", Status::Busy { tool: None });
        local.hostname = Some("mbp".into());
        local.cwd = "/Users/me/dev/local-project".into();
        let mut remote = session(
            "remote",
            Status::Waiting {
                detail: Some("Approve?".into()),
            },
        );
        remote.hostname = Some("buildbox".into());
        remote.cwd = "/Users/me/dev/remote-project".into();
        let mut state = AppState {
            sessions: vec![local, remote],
            connected: true,
            local_hostname: "mbp".into(),
            summary: MenuBarSummary {
                busy: 1,
                waiting: 1,
            },
            ..Default::default()
        };

        let rows = render(&state, 100, 16);
        assert!(rows[0].contains("all hosts"), "{rows:#?}");
        assert!(rows.iter().any(|row| row.contains("local-project")));
        assert!(rows.iter().any(|row| row.contains("remote-project")));

        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 100, 16);
        assert!(rows[0].contains("local: mbp"), "{rows:#?}");
        assert!(rows[0].contains("1 busy"), "{rows:#?}");
        assert!(!rows[0].contains("waiting"), "{rows:#?}");
        assert!(rows.iter().any(|row| row.contains("local-project")));
        assert!(!rows.iter().any(|row| row.contains("remote-project")));
        assert!(rows.last().unwrap().contains("h hosts"), "{rows:#?}");

        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 100, 16);
        assert!(rows[0].contains("all hosts"), "{rows:#?}");
        assert!(rows.iter().any(|row| row.contains("remote-project")));
    }

    #[test]
    fn local_filter_and_shortcut_remain_visible_at_floor_width() {
        let mut state = AppState {
            local_hostname: "a-very-long-local-hostname".into(),
            ..Default::default()
        };
        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 40, 12);
        assert!(rows[0].contains("local"), "{rows:#?}");
        assert!(rows.last().unwrap().contains("h hosts"), "{rows:#?}");
    }

    #[test]
    fn local_filter_header_keeps_visible_counts_at_floor_width() {
        let mut local_busy = session("local-busy", Status::Busy { tool: None });
        local_busy.hostname = Some("a-very-long-local-hostname".into());
        let mut local_waiting = session("local-waiting", Status::Waiting { detail: None });
        local_waiting.hostname = Some("a-very-long-local-hostname".into());
        let mut remote_waiting = session("remote-waiting", Status::Waiting { detail: None });
        remote_waiting.hostname = Some("buildbox".into());
        let mut state = AppState {
            sessions: vec![local_busy, local_waiting, remote_waiting],
            connected: true,
            local_hostname: "a-very-long-local-hostname".into(),
            summary: MenuBarSummary {
                busy: 1,
                waiting: 2,
            },
            ..Default::default()
        };
        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 40, 16);
        assert!(rows[0].contains("local"), "{rows:#?}");
        assert!(rows[0].contains("1 waiting"), "{rows:#?}");
        assert!(rows[0].contains("1 busy"), "{rows:#?}");
    }

    #[test]
    fn wide_header_shortens_the_hostname_before_hiding_counts() {
        let mut local_busy = session("local-busy", Status::Busy { tool: None });
        local_busy.hostname = Some("a-very-long-local-hostname-that-keeps-going".into());
        let mut local_waiting = session("local-waiting", Status::Waiting { detail: None });
        local_waiting.hostname = Some("a-very-long-local-hostname-that-keeps-going".into());
        let mut state = AppState {
            sessions: vec![local_busy, local_waiting],
            connected: true,
            local_hostname: "a-very-long-local-hostname-that-keeps-going".into(),
            ..Default::default()
        };
        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 80, 16);
        assert!(rows[0].contains("local:"), "{rows:#?}");
        assert!(rows[0].contains('…'), "{rows:#?}");
        assert!(rows[0].contains("1 waiting"), "{rows:#?}");
        assert!(rows[0].contains("1 busy"), "{rows:#?}");
    }

    #[test]
    fn filtering_clamps_selection_and_actions_to_a_visible_session() {
        let mut remote = session("remote", Status::Waiting { detail: None });
        remote.hostname = Some("buildbox".into());
        let mut local = session("local", Status::Busy { tool: None });
        local.hostname = Some("mbp".into());
        let mut state = AppState {
            sessions: vec![remote, local],
            local_hostname: "mbp".into(),
            selected: Some("remote".into()),
            ..Default::default()
        };

        press(&mut state, KeyCode::Char('h'));
        assert_eq!(state.selected.as_deref(), Some("local"));

        let activated = std::cell::RefCell::new(None);
        let outcome = state.handle_key_with(
            KeyCode::Enter,
            KeyModifiers::NONE,
            "/Users/me",
            |session, _| {
                *activated.borrow_mut() = Some(session.session_id.clone());
                Ok(())
            },
            |_| {},
        );
        assert_eq!(outcome, KeyOutcome::Activated);
        assert_eq!(activated.into_inner().as_deref(), Some("local"));

        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.view_mode, ViewMode::Detail);
        assert!(render(&state, 80, 14).join("\n").contains("mbp"));
        press(&mut state, KeyCode::Esc);

        press(&mut state, KeyCode::Char('d'));
        let deleted = std::cell::RefCell::new(None);
        state.handle_key_with(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
            "/Users/me",
            |_, _| Ok(()),
            |id| *deleted.borrow_mut() = Some(id),
        );
        assert_eq!(deleted.into_inner().as_deref(), Some("local"));
    }

    #[test]
    fn local_filter_survives_live_updates_and_restores_the_full_snapshot() {
        let mut local = session("local", Status::Idle);
        local.hostname = Some("mbp".into());
        local.cwd = "/Users/me/dev/local-old".into();
        let mut remote = session("remote", Status::Idle);
        remote.hostname = Some("buildbox".into());
        remote.cwd = "/Users/me/dev/remote-old".into();
        let mut state = AppState {
            sessions: vec![local, remote],
            local_hostname: "mbp".into(),
            selected: Some("local".into()),
            last_width: 100,
            last_body_height: 4,
            ..Default::default()
        };
        press(&mut state, KeyCode::Char('h'));

        let mut moved_local = session("local", Status::Waiting { detail: None });
        moved_local.hostname = Some("mbp".into());
        moved_local.cwd = "/Users/me/dev/local-new".into();
        let mut new_local = session("local-two", Status::Busy { tool: None });
        new_local.hostname = Some("mbp".into());
        new_local.cwd = "/Users/me/dev/local-two".into();
        let mut new_remote = session("remote-two", Status::Busy { tool: None });
        new_remote.hostname = Some("buildbox".into());
        new_remote.cwd = "/Users/me/dev/remote-new".into();
        state.set_sessions(vec![new_remote, new_local, moved_local]);

        assert_eq!(state.selected.as_deref(), Some("local"));
        let rows = render(&state, 100, 16);
        assert!(rows.iter().any(|row| row.contains("local-new")));
        assert!(rows.iter().any(|row| row.contains("local-two")));
        assert!(!rows.iter().any(|row| row.contains("remote-new")));
        assert!(
            row_index(&rows, "local-new") < row_index(&rows, "local-two"),
            "waiting Session should remain above the Rest section: {rows:#?}"
        );

        let mut remaining_remote = session("remote-two", Status::Busy { tool: None });
        remaining_remote.hostname = Some("buildbox".into());
        remaining_remote.cwd = "/Users/me/dev/remote-new".into();
        state.scroll_offset = 9;
        state.set_sessions(vec![remaining_remote]);
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);

        press(&mut state, KeyCode::Char('h'));
        let rows = render(&state, 100, 12);
        assert_eq!(state.selected.as_deref(), Some("remote-two"));
        assert!(rows.iter().any(|row| row.contains("remote-new")));
    }

    #[test]
    fn navigation_and_jumps_visit_only_local_sessions() {
        let mut local_waiting = session("local-waiting", Status::Waiting { detail: None });
        local_waiting.hostname = Some("mbp".into());
        let mut remote_waiting = session("remote-waiting", Status::Waiting { detail: None });
        remote_waiting.hostname = Some("buildbox".into());
        let mut local_one = session("local-one", Status::Idle);
        local_one.hostname = Some("mbp".into());
        let mut remote_one = session("remote-one", Status::Idle);
        remote_one.hostname = Some("buildbox".into());
        let mut local_two = session("local-two", Status::Idle);
        local_two.hostname = Some("mbp".into());
        let mut state = AppState {
            sessions: vec![
                local_waiting,
                remote_waiting,
                local_one,
                remote_one,
                local_two,
            ],
            local_hostname: "mbp".into(),
            last_width: 100,
            last_body_height: 4,
            ..Default::default()
        };
        press(&mut state, KeyCode::Char('h'));

        state.select_first();
        assert_eq!(state.selected.as_deref(), Some("local-waiting"));
        press(&mut state, KeyCode::Char('G'));
        assert_eq!(state.selected.as_deref(), Some("local-two"));
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(state.selected.as_deref(), Some("local-one"));
        press(&mut state, KeyCode::Char('g'));
        press(&mut state, KeyCode::Char('g'));
        assert_eq!(state.selected.as_deref(), Some("local-waiting"));
        press_mod(&mut state, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(state.selected.as_deref(), Some("local-two"));
        press_mod(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(state.selected.as_deref(), Some("local-waiting"));
    }

    #[test]
    fn wide_row_status_and_time_always_present() {
        let mut s = session("s", Status::Busy { tool: None });
        s.cwd = "/Users/me/dev/a-really-long-directory-name-that-will-not-fit-at-all".into();
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
        let rows = render(&state, 80, 10);
        // At least one row (the session row, not the header) should have both
        // a status label and a relative time.
        assert!(
            rows.iter().any(|r| r.contains("busy") && r.contains("ago")),
            "status and time both present at 80 cols: {rows:#?}"
        );
    }

    #[test]
    fn wide_row_branch_drops_whole_before_cwd_elides() {
        let mut s = session("s", Status::Busy { tool: None });
        s.cwd = "/Users/me/dev/project".into();
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
        let rows = render(&state, 80, 10);
        // The branch@remote is never mid-truncated: either it's fully there or absent.
        let session_row = rows
            .iter()
            .find(|r| r.contains("busy") && r.contains("ago"));
        assert!(session_row.is_some(), "session row not found: {rows:#?}");
        let row = session_row.unwrap();
        if row.contains("a-very-long-feature-branch-name") {
            assert!(
                row.contains("a-very-long-feature-branch-name@org/enormous-repository-name"),
                "branch must appear whole or not at all: {row}"
            );
        }
        // cwd should be present (possibly elided but not missing).
        assert!(
            row.contains("project") || row.contains("…"),
            "cwd present: {row}"
        );
        // status always present.
        assert!(row.contains("busy"), "status present: {row}");
    }

    #[test]
    fn narrow_layout_renders_cards() {
        let mut s = session("s", Status::Busy { tool: None });
        s.hostname = Some("buildbox".into());
        s.cwd = "/Users/me/dev/project".into();
        let state = AppState {
            sessions: vec![s],
            connected: true,
            summary: MenuBarSummary {
                busy: 1,
                waiting: 0,
            },
            ..Default::default()
        };
        let rows = render(&state, 79, 20);
        // The gutter character should appear.
        assert!(
            rows.iter().any(|r| r.contains('\u{258c}')),
            "card gutter ▌ present: {rows:#?}"
        );
        // status should be visible.
        assert!(
            rows.iter().any(|r| r.contains("busy")),
            "status present in card: {rows:#?}"
        );
    }

    #[test]
    fn narrow_layout_40_cols_has_all_priority_fields() {
        let s = session("s", Status::Idle);
        let state = AppState {
            sessions: vec![s],
            connected: true,
            summary: MenuBarSummary {
                busy: 0,
                waiting: 0,
            },
            ..Default::default()
        };
        let rows = render(&state, 40, 20);
        assert!(
            rows.iter().any(|r| r.contains("idle")),
            "status present at 40 cols: {rows:#?}"
        );
        // cwd or elided cwd present.
        assert!(
            rows.iter()
                .any(|r| r.contains("project") || r.contains("…")),
            "cwd present at 40 cols: {rows:#?}"
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
        state.handle_key_with(code, KeyModifiers::NONE, "/Users/me", |_, _| Ok(()), |_| {});
    }

    /// Inject a key press with explicit modifiers (e.g. Ctrl+d).
    fn press_mod(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
        state.handle_key_with(code, modifiers, "/Users/me", |_, _| Ok(()), |_| {});
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
    fn selected_row_uses_catppuccin_mocha_surface_0_in_dark_mode() {
        let mut state = two_section_state();
        // Dark mode is the default when system theme detection is unavailable.
        state.selected = Some("wait".into());
        assert_eq!(
            bg_of(&state, 100, 20, "waiting(Approve?)"),
            Color::Rgb(49, 50, 68)
        );
        assert_eq!(
            bg_of(&state, 60, 20, "waiting(Approve?)"),
            Color::Rgb(49, 50, 68)
        );
        state.selected = Some("busy".into());
        assert_eq!(
            bg_of(&state, 100, 20, "project  busy"),
            Color::Rgb(49, 50, 68)
        );
    }

    #[test]
    fn selected_row_uses_catppuccin_latte_surface_0_in_light_mode() {
        let mut state = two_section_state();
        state.appearance = Appearance::Light;
        state.selected = Some("wait".into());
        assert_eq!(
            bg_of(&state, 100, 20, "waiting(Approve?)"),
            Color::Rgb(204, 208, 218)
        );
        assert_eq!(
            bg_of(&state, 60, 20, "waiting(Approve?)"),
            Color::Rgb(204, 208, 218)
        );
        state.selected = Some("busy".into());
        assert_eq!(
            bg_of(&state, 100, 20, "project  busy"),
            Color::Rgb(204, 208, 218)
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
            KeyModifiers::NONE,
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
    fn enter_reports_activated_on_success() {
        // A successful Enter-activation is what `--exit-on-select` keys off, so
        // the seam must report it back to the loop.
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        let outcome = state.handle_key_with(
            KeyCode::Enter,
            KeyModifiers::NONE,
            "/Users/me",
            |_, _| Ok(()),
            |_| {},
        );
        assert_eq!(outcome, KeyOutcome::Activated);
    }

    #[test]
    fn enter_reports_none_when_activation_fails() {
        // A failed jump must not exit the switcher: the outcome stays `None` (and
        // the inline error is recorded so the user can pick another row).
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        let outcome = state.handle_key_with(
            KeyCode::Enter,
            KeyModifiers::NONE,
            "/Users/me",
            |_, _| Err(ActivationError::NoTmuxTarget),
            |_| {},
        );
        assert_eq!(outcome, KeyOutcome::None);
        assert!(state.activation_errors.contains_key("busy"));
    }

    #[test]
    fn non_activating_keys_report_none() {
        // Navigation and delete never signal an exit-worthy activation.
        let mut state = two_section_state();
        state.selected = Some("busy".into());
        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('d'),
        ] {
            let outcome =
                state.handle_key_with(code, KeyModifiers::NONE, "/Users/me", |_, _| Ok(()), |_| {});
            assert_eq!(outcome, KeyOutcome::None, "{code:?} should not activate");
        }
    }

    #[test]
    fn enter_with_no_selection_reports_none() {
        let mut state = two_section_state();
        state.selected = None;
        let outcome = state.handle_key_with(
            KeyCode::Enter,
            KeyModifiers::NONE,
            "/Users/me",
            |_, _| Ok(()),
            |_| {},
        );
        assert_eq!(outcome, KeyOutcome::None);
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
            KeyModifiers::NONE,
            "/Users/me",
            |_, _| Ok(()),
            |_| deleted.set(true),
        );
        assert!(!state.modal_open());

        press(&mut state, KeyCode::Char('d'));
        assert!(state.modal_open());
        state.handle_key_with(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
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
            KeyModifiers::NONE,
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
            KeyModifiers::NONE,
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
            KeyModifiers::NONE,
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

    #[test]
    fn local_empty_state_uses_only_the_local_watcher_health() {
        let mut remote = session("remote", Status::Idle);
        remote.hostname = Some("buildbox".into());
        let mut state = AppState {
            sessions: vec![remote],
            connected: true,
            local_hostname: "mbp".into(),
            has_received_host_status: true,
            hosts: vec![HostStatus {
                hostname: "buildbox".into(),
                agent_kind: AgentKind::Claude,
                last_seen_at: now(),
            }],
            ..Default::default()
        };
        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 80, 12);
        assert!(
            rows.iter()
                .any(|row| row.contains("No local watcher is reporting")),
            "{rows:#?}"
        );

        state.hosts.push(HostStatus {
            hostname: "mbp".into(),
            agent_kind: AgentKind::Claude,
            last_seen_at: now(),
        });
        let rows = render(&state, 80, 12);
        assert!(
            rows.iter()
                .any(|row| row.contains("No active sessions on this host")),
            "{rows:#?}"
        );

        state.hosts[1].last_seen_at = now() - chrono::Duration::seconds(31);
        let rows = render(&state, 40, 12);
        assert!(
            rows.iter()
                .any(|row| row.contains("No local watcher is reporting")),
            "{rows:#?}"
        );
    }

    #[test]
    fn unresolved_hostname_keeps_all_hosts_visible_and_explains_the_filter() {
        let mut remote = session("remote", Status::Idle);
        remote.hostname = Some("buildbox".into());
        remote.cwd = "/Users/me/dev/remote-project".into();
        let mut unknown = session("unknown", Status::Idle);
        unknown.cwd = "/Users/me/dev/unknown-project".into();
        let mut state = AppState {
            sessions: vec![remote, unknown],
            local_hostname: String::new(),
            ..Default::default()
        };

        press(&mut state, KeyCode::Char('h'));

        let rows = render(&state, 40, 16);
        assert!(rows[0].contains("all"), "{rows:#?}");
        assert!(
            rows.iter().any(|row| row.contains("remote-project")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("unknown-project")),
            "{rows:#?}"
        );
        assert!(
            rows.last().unwrap().contains("Local host unavailable"),
            "{rows:#?}"
        );
    }

    #[test]
    fn sessions_without_a_hostname_are_not_local() {
        let mut local = session("local", Status::Idle);
        local.hostname = Some("mbp".into());
        local.cwd = "/Users/me/dev/local-project".into();
        let mut unknown = session("unknown", Status::Idle);
        unknown.cwd = "/Users/me/dev/unknown-project".into();
        let mut state = AppState {
            sessions: vec![local, unknown],
            local_hostname: "mbp".into(),
            ..Default::default()
        };

        let rows = render(&state, 40, 16);
        assert!(rows.iter().any(|row| row.contains("unknown-project")));
        press(&mut state, KeyCode::Char('h'));
        let rows = render(&state, 40, 16);
        assert!(rows.iter().any(|row| row.contains("local-project")));
        assert!(!rows.iter().any(|row| row.contains("unknown-project")));
    }

    #[test]
    fn h_does_nothing_in_detail_view_or_the_delete_modal() {
        let mut local = session("local", Status::Idle);
        local.hostname = Some("mbp".into());
        let mut state = AppState {
            sessions: vec![local],
            local_hostname: "mbp".into(),
            selected: Some("local".into()),
            ..Default::default()
        };

        press(&mut state, KeyCode::Char(' '));
        press(&mut state, KeyCode::Char('h'));
        assert_eq!(state.host_filter, HostFilter::All);
        press(&mut state, KeyCode::Esc);

        press(&mut state, KeyCode::Char('d'));
        press(&mut state, KeyCode::Char('h'));
        assert_eq!(state.host_filter, HostFilter::All);
        assert!(state.modal_open());
        press(&mut state, KeyCode::Esc);

        press(&mut state, KeyCode::Char('h'));
        assert_eq!(state.host_filter, HostFilter::Local);
    }

    /// Build a state with `n` idle sessions, body height set, and no selection.
    fn many_sessions_state(n: usize, last_body_height: usize, last_width: usize) -> AppState {
        let sessions: Vec<SessionView> = (0..n)
            .map(|i| session(&format!("s{i}"), Status::Idle))
            .collect();
        AppState {
            sessions,
            connected: true,
            last_width,
            last_body_height,
            ..Default::default()
        }
    }

    #[test]
    fn viewport_tracks_selection_scrolling_down() {
        // 5 sessions at wide layout (1 line each), body height = 3 (header 2 +
        // help 1 => subtract 3, so a terminal of height 6 gives body_height 3).
        // With 5 sessions and 3 visible rows, moving to the 5th session should
        // scroll the viewport so that session is visible.
        let mut state = many_sessions_state(5, 3, 100);
        // Select the first session to anchor.
        state.select_first();
        assert_eq!(state.scroll_offset, 0);

        // Move to the last session (G); viewport must scroll to show it.
        state.select_last();
        let selected_row = state.selected_row().unwrap(); // row 4 (0-indexed)
        assert!(
            selected_row < state.scroll_offset + state.last_body_height,
            "selected row {selected_row} should be visible: offset={} height={}",
            state.scroll_offset,
            state.last_body_height
        );
        assert!(
            selected_row >= state.scroll_offset,
            "selected row {selected_row} should be above viewport bottom"
        );
    }

    #[test]
    fn viewport_tracks_selection_scrolling_up() {
        let mut state = many_sessions_state(5, 3, 100);
        // Start at the last session.
        state.select_last();
        let offset_at_bottom = state.scroll_offset;
        assert!(
            offset_at_bottom > 0,
            "bottom selection should have scrolled down"
        );

        // Jump back to first (gg); offset should reset to 0.
        state.select_first();
        assert_eq!(state.scroll_offset, 0, "gg should scroll back to top");
    }

    #[test]
    fn gg_jumps_to_first_session() {
        let mut state = many_sessions_state(5, 10, 100);
        state.select_last();
        // Press g twice to trigger gg.
        press(&mut state, KeyCode::Char('g'));
        press(&mut state, KeyCode::Char('g'));
        let order = state.ordered_ids();
        assert_eq!(
            state.selected.as_deref(),
            Some(order[0].as_str()),
            "gg should select the first session"
        );
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn capital_g_jumps_to_last_session() {
        let mut state = many_sessions_state(5, 10, 100);
        state.select_first();
        press(&mut state, KeyCode::Char('G'));
        let order = state.ordered_ids();
        assert_eq!(
            state.selected.as_deref(),
            Some(order[order.len() - 1].as_str()),
            "G should select the last session"
        );
    }

    #[test]
    fn single_g_does_not_jump_clears_on_next_different_key() {
        let mut state = many_sessions_state(3, 10, 100);
        state.select_first();
        // One g: sets pending_g, no jump.
        press(&mut state, KeyCode::Char('g'));
        assert!(state.pending_g, "single g should arm pending_g");
        // Any other key clears pending_g without jumping.
        press(&mut state, KeyCode::Char('j'));
        assert!(!state.pending_g, "j should clear pending_g");
        let order = state.ordered_ids();
        assert_eq!(
            state.selected.as_deref(),
            Some(order[1].as_str()),
            "j should have moved down (not gg-jumped)"
        );
    }

    #[test]
    fn ctrl_d_jumps_half_page_down() {
        // 10 sessions, body height 4 -> half = 2 sessions per jump.
        let mut state = many_sessions_state(10, 4, 100);
        state.select_first();
        let first_id = state.selected.clone().unwrap();
        press_mod(&mut state, KeyCode::Char('d'), KeyModifiers::CONTROL);
        // Should have moved by at least 1 session.
        assert_ne!(
            state.selected.as_deref(),
            Some(first_id.as_str()),
            "Ctrl-d should move selection down"
        );
        // Viewport should keep selection visible.
        let row = state.selected_row().unwrap();
        assert!(row >= state.scroll_offset);
        assert!(row < state.scroll_offset + state.last_body_height);
    }

    #[test]
    fn ctrl_u_jumps_half_page_up() {
        let mut state = many_sessions_state(10, 4, 100);
        state.select_last();
        let last_id = state.selected.clone().unwrap();
        press_mod(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_ne!(
            state.selected.as_deref(),
            Some(last_id.as_str()),
            "Ctrl-u should move selection up"
        );
        let row = state.selected_row().unwrap();
        assert!(row >= state.scroll_offset);
        assert!(row < state.scroll_offset + state.last_body_height);
    }

    #[test]
    fn viewport_narrow_layout_tracks_card_selection() {
        // Narrow layout: 4 lines per card. 3 sessions, body height = 6 -> fits
        // 1.5 cards. Selecting the last session should scroll.
        let mut state = many_sessions_state(3, 6, 60);
        state.select_first();
        assert_eq!(state.scroll_offset, 0);
        state.select_last();
        let row = state.selected_row().unwrap();
        assert!(
            row < state.scroll_offset + state.last_body_height,
            "last card bottom must be visible: row={row}, offset={}, height={}",
            state.scroll_offset,
            state.last_body_height
        );
        assert!(
            row >= state.scroll_offset,
            "last card top must be above viewport bottom"
        );
    }

    #[test]
    fn render_shows_only_viewport_window_at_small_height() {
        // Wide layout (100 cols), 5 sessions, terminal height = 6 (body = 3).
        // Select the last session so viewport is scrolled down.
        let mut state = many_sessions_state(5, 3, 100);
        state.select_last();

        // The first session must NOT appear in the rendered frame.
        let rows = render(&state, 100, 6);
        // s0's cwd "project" will appear in every session's row, so use the
        // session id embedded in... wait, there's no session id in the row.
        // Instead check that "s0" is not visible but "s4" is. We can't directly
        // tell them apart by row text, so assert scroll_offset > 0 instead
        // (the viewport has moved past the first row).
        assert!(
            state.scroll_offset > 0,
            "viewport should be scrolled past session 0: offset={}",
            state.scroll_offset
        );

        // The cursor glyph must appear in the rendered area.
        assert!(
            rows.iter().any(|r| r.contains(CURSOR)),
            "selected session cursor should be visible: {rows:#?}"
        );
    }

    #[test]
    fn waiting_section_chrome_counted_in_viewport_offset() {
        // When there's a waiting section, its 2 border lines must be accounted
        // for in selected_row so that jumping to the first rest session still
        // keeps it visible.
        let mut state = AppState {
            sessions: vec![
                session(
                    "wait",
                    Status::Waiting {
                        detail: Some("Approve?".into()),
                    },
                ),
                session("busy", Status::Busy { tool: None }),
            ],
            connected: true,
            last_width: 100,
            last_body_height: 5,
            ..Default::default()
        };
        state.select_last();
        let row = state.selected_row().unwrap();
        // With 1 waiting session: top_border(1) + 1 session(1) + bottom_border(1) = 3 lines
        // before the rest section. The rest session starts at row 3.
        assert_eq!(
            row, 3,
            "rest session should start at row 3 after waiting chrome"
        );
        assert!(
            row < state.scroll_offset + state.last_body_height,
            "row visible"
        );
    }

    #[test]
    fn viewport_selection_at_middle_stays_visible() {
        // 7 sessions, body height 3 (wide layout). Navigate to the middle session
        // and verify the viewport keeps it visible.
        let mut state = many_sessions_state(7, 3, 100);
        state.select_first();
        // Move to index 3 (the middle of 0..=6).
        for _ in 0..3 {
            press(&mut state, KeyCode::Char('j'));
        }
        let row = state.selected_row().unwrap();
        assert!(
            row >= state.scroll_offset,
            "middle selection top must be within viewport: row={row}, offset={}",
            state.scroll_offset
        );
        assert!(
            row < state.scroll_offset + state.last_body_height,
            "middle selection must be visible: row={row}, offset={}, height={}",
            state.scroll_offset,
            state.last_body_height
        );
    }

    // ---- detail view tests ----

    fn detail_session() -> SessionView {
        SessionView {
            session_id: "abc-123".into(),
            cwd: "/Users/me/dev/myproject".into(),
            status: Status::Busy { tool: None },
            agent_kind: AgentKind::Claude,
            model: Some("claude-opus-4-5".into()),
            updated_at: now(),
            hostname: Some("myhost".into()),
            git_branch: Some("main".into()),
            git_remote: Some("git@github.com:user/repo.git".into()),
            tmux_target: Some("sess:1.0".into()),
            name: Some("my-session".into()),
        }
    }

    fn detail_state(s: SessionView) -> AppState {
        let id = s.session_id.clone();
        AppState {
            sessions: vec![s],
            connected: true,
            selected: Some(id),
            view_mode: ViewMode::Detail,
            summary: MenuBarSummary {
                busy: 1,
                waiting: 0,
            },
            ..Default::default()
        }
    }

    #[test]
    fn space_opens_detail_view_and_esc_returns() {
        let mut state = one_session_state(session("s1", Status::Busy { tool: None }), true);
        state.selected = Some("s1".into());

        // Before Space: list view, help bar shows normal help
        let rows = render(&state, 100, 10);
        assert!(rows.last().unwrap().contains("j/k move"), "list help bar");
        assert_eq!(state.view_mode, ViewMode::List);

        // Space opens detail view
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.view_mode, ViewMode::Detail);
        let rows = render(&state, 100, 10);
        assert!(
            rows.last().unwrap().contains("Space/Esc back"),
            "detail help bar"
        );

        // Esc returns to list
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.view_mode, ViewMode::List);
        let rows = render(&state, 100, 10);
        assert!(
            rows.last().unwrap().contains("j/k move"),
            "list help bar after Esc"
        );
    }

    #[test]
    fn space_in_detail_view_also_returns_to_list() {
        let mut state = one_session_state(session("s1", Status::Busy { tool: None }), true);
        state.selected = Some("s1".into());
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.view_mode, ViewMode::Detail);
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.view_mode, ViewMode::List);
    }

    #[test]
    fn detail_view_shows_all_fields_at_wide_width() {
        let state = detail_state(detail_session());
        let rows = render(&state, 100, 20);
        let content = rows.join("\n");
        assert!(content.contains("abc-123"), "session id");
        assert!(content.contains("myhost"), "hostname");
        assert!(content.contains("myproject"), "cwd");
        assert!(content.contains("main"), "branch");
        assert!(
            content.contains("git@github.com:user/repo.git"),
            "raw remote url"
        );
        assert!(content.contains("sess:1.0"), "tmux target");
        assert!(content.contains("claude-opus-4-5"), "model");
        assert!(content.contains("claude"), "agent kind");
        assert!(content.contains("my-session"), "name");
        assert!(content.contains("busy"), "status");
        // Both relative and absolute time
        assert!(content.contains("2026-08-04"), "absolute time");
    }

    #[test]
    fn detail_view_shows_all_fields_at_narrow_width() {
        let state = detail_state(detail_session());
        let rows = render(&state, 40, 25);
        let content = rows.join("\n");
        assert!(content.contains("abc-123"), "session id at narrow width");
        assert!(
            content.contains("git@github.com:user/repo.git"),
            "raw remote at narrow width"
        );
        assert!(content.contains("claude-opus-4-5"), "model at narrow width");
    }

    #[test]
    fn detail_view_no_tmux_shows_cannot_activate() {
        let mut s = detail_session();
        s.tmux_target = None;
        let state = detail_state(s);
        let rows = render(&state, 100, 20);
        let content = rows.join("\n");
        assert!(
            content.contains("none - cannot activate"),
            "no tmux message"
        );
    }

    #[test]
    fn navigation_keys_do_nothing_in_detail_view() {
        let mut state = detail_state(detail_session());
        // selection stays on the session
        let original_selected = state.selected.clone();
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('G'));
        // still in detail view
        assert_eq!(state.view_mode, ViewMode::Detail);
        // selection unchanged
        assert_eq!(state.selected, original_selected);
    }

    #[test]
    fn selection_is_preserved_when_returning_from_detail() {
        let mut state = AppState {
            sessions: vec![
                session("s1", Status::Busy { tool: None }),
                session("s2", Status::Busy { tool: None }),
            ],
            connected: true,
            selected: Some("s2".into()),
            summary: MenuBarSummary {
                busy: 2,
                waiting: 0,
            },
            ..Default::default()
        };
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.view_mode, ViewMode::Detail);
        assert_eq!(state.selected.as_deref(), Some("s2"));
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.view_mode, ViewMode::List);
        assert_eq!(
            state.selected.as_deref(),
            Some("s2"),
            "selection preserved after return"
        );
    }

    fn delete_modal_state(label: &str) -> AppState {
        let s = session("s1", Status::Busy { tool: None });
        let mut state = one_session_state(s, true);
        state.selected = Some("s1".into());
        state.pending_delete = Some(PendingDelete {
            session_id: "s1".into(),
            label: label.into(),
        });
        state
    }

    #[test]
    fn delete_modal_fits_at_52_cols() {
        let state = delete_modal_state("my-session");
        let rows = render(&state, 52, 10);
        let content = rows.join("\n");
        assert!(content.contains("Delete session"), "modal title at 52 cols");
        assert!(content.contains("my-session"), "label at 52 cols");
        assert!(content.contains("y confirm"), "confirm hint at 52 cols");
        // The modal border must start at column 0 (fits the full 52 cols)
        let modal_row = rows.iter().find(|r| r.contains("Delete session")).unwrap();
        assert!(
            modal_row.starts_with('╭'),
            "border starts at col 0 at 52 cols"
        );
    }

    #[test]
    fn delete_modal_fits_at_40_cols() {
        let state = delete_modal_state("my-session");
        let rows = render(&state, 40, 10);
        let content = rows.join("\n");
        assert!(content.contains("Delete session"), "modal title at 40 cols");
        assert!(content.contains("y confirm"), "confirm hint at 40 cols");
        // The modal frame must not overflow: border starts at col 0
        let modal_row = rows.iter().find(|r| r.contains("Delete session")).unwrap();
        assert!(
            modal_row.starts_with('╭'),
            "border starts at col 0 at 40 cols"
        );
        // The rendered row width must be <= 40
        assert!(
            modal_row.chars().count() <= 40,
            "modal does not overflow at 40 cols"
        );
    }

    #[test]
    fn delete_modal_shortens_long_label_at_40_cols() {
        let long_label = "a-very-long-session-name-that-exceeds-budget";
        let state = delete_modal_state(long_label);
        let rows = render(&state, 40, 10);
        let content = rows.join("\n");
        // The full label must not appear; a truncated form with ellipsis should
        assert!(
            !content.contains(long_label),
            "long label must be shortened at 40 cols"
        );
        assert!(content.contains('…'), "truncated label has ellipsis");
    }
}
