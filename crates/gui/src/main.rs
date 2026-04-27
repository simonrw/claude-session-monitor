use chrono::{DateTime, Utc};
use clap::Parser;
use common::activation;
use common::api::SessionView;
use common::view_model::{
    ConnectionState, CoreHandle, MenuBarSummary, SessionObserver, SubscriptionHandle,
};
use eframe::egui;
#[cfg(target_os = "macos")]
use muda::{CheckMenuItem, Menu, MenuEvent, PredefinedMenuItem, Submenu};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "claude-session-monitor-gui",
    about = "Claude session monitor GUI"
)]
struct Args {
    /// Server URL (e.g. http://localhost:7685)
    #[arg(long)]
    server_url: Option<String>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Enable macOS vibrancy/background blur effect
    #[arg(long)]
    vibrancy: bool,

    /// Hide the app from the macOS dock (runs as an accessory app).
    /// Accepted on all platforms but only takes effect on macOS.
    #[arg(long)]
    hide_from_dock: bool,
}

fn is_stale(updated_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(updated_at) >= chrono::Duration::minutes(30)
}

fn should_fade(connected: bool, stale: bool) -> bool {
    !connected || stale
}

fn partition_sessions(sessions: &[SessionView]) -> (Vec<&SessionView>, Vec<&SessionView>) {
    let mut waiting = Vec::new();
    let mut working = Vec::new();
    for session in sessions {
        match &session.status {
            common::session::Status::Waiting(_) => waiting.push(session),
            _ => working.push(session),
        }
    }
    working.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    (waiting, working)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};

    fn make_session(id: &str, status: Status, updated_at: DateTime<Utc>) -> SessionView {
        SessionView {
            session_id: id.into(),
            cwd: "/tmp/project".into(),
            status,
            agent_kind: common::api::AgentKind::Claude,
            model: None,
            updated_at,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        }
    }

    #[test]
    fn stale_at_thirty_minutes() {
        let now = Utc::now();
        let updated_at = now - chrono::Duration::minutes(30);
        assert!(is_stale(updated_at, now));
    }

    #[test]
    fn not_stale_at_twenty_nine_minutes() {
        let now = Utc::now();
        let updated_at = now - chrono::Duration::minutes(29);
        assert!(!is_stale(updated_at, now));
    }

    #[test]
    fn not_faded_when_connected_and_fresh() {
        assert!(!should_fade(true, false));
    }

    #[test]
    fn faded_when_connected_and_stale() {
        assert!(should_fade(true, true));
    }

    #[test]
    fn faded_when_disconnected_and_fresh() {
        assert!(should_fade(false, false));
    }

    #[test]
    fn faded_when_disconnected_and_stale() {
        assert!(should_fade(false, true));
    }

    #[test]
    fn partition_waiting_to_top() {
        let now = Utc::now();
        let sessions = vec![
            make_session(
                "s1",
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Input,
                    detail: None,
                }),
                now,
            ),
            make_session(
                "s2",
                Status::Waiting(WaitingStatus {
                    reason: WaitingReason::Permission,
                    detail: None,
                }),
                now,
            ),
        ];
        let (top, bottom) = partition_sessions(&sessions);
        assert_eq!(top.len(), 2);
        assert_eq!(bottom.len(), 0);
    }

    #[test]
    fn partition_working_to_bottom() {
        let now = Utc::now();
        let sessions = vec![make_session(
            "s1",
            Status::Working(WorkingStatus { tool: None }),
            now,
        )];
        let (top, bottom) = partition_sessions(&sessions);
        assert_eq!(top.len(), 0);
        assert_eq!(bottom.len(), 1);
    }

    #[test]
    fn partition_bottom_sorted_by_updated_at_desc() {
        let now = Utc::now();
        let older = now - chrono::Duration::minutes(5);
        let sessions = vec![
            make_session("s1", Status::Working(WorkingStatus { tool: None }), older),
            make_session("s2", Status::Working(WorkingStatus { tool: None }), now),
        ];
        let (_, bottom) = partition_sessions(&sessions);
        assert_eq!(bottom[0].session_id, "s2");
        assert_eq!(bottom[1].session_id, "s1");
    }
}

/// Mutable snapshot kept in sync by [`EguiObserver`]. Read on each egui frame.
#[derive(Default)]
struct Snapshot {
    sessions: Vec<SessionView>,
    connection: Option<ConnectionState>,
    _summary: MenuBarSummary,
    activation_errors: HashMap<String, String>,
}

struct EguiObserver {
    snapshot: Arc<Mutex<Snapshot>>,
}

impl SessionObserver for EguiObserver {
    fn on_sessions_changed(&self, sessions: Vec<SessionView>) {
        let mut snap = self.snapshot.lock().unwrap();
        snap.sessions = sessions;
        snap.activation_errors.clear();
    }
    fn on_connection_changed(&self, state: ConnectionState) {
        self.snapshot.lock().unwrap().connection = Some(state);
    }
    fn on_summary_changed(&self, summary: MenuBarSummary) {
        self.snapshot.lock().unwrap()._summary = summary;
    }
}

struct App {
    core: CoreHandle,
    _subscription: SubscriptionHandle,
    snapshot: Arc<Mutex<Snapshot>>,
    pending_delete: Option<String>,
    local_hostname: String,
    always_on_top: bool,
    borderless: bool,
    vibrancy_enabled: bool,
    transparent: bool,
    click_through: bool,
    #[cfg(target_os = "macos")]
    _menu: Menu,
    #[cfg(target_os = "macos")]
    always_on_top_item: CheckMenuItem,
    #[cfg(target_os = "macos")]
    borderless_item: CheckMenuItem,
    #[cfg(target_os = "macos")]
    transparent_item: CheckMenuItem,
    #[cfg(target_os = "macos")]
    click_through_item: CheckMenuItem,
}

impl App {
    fn new(server_url_arg: Option<String>, vibrancy_enabled: bool) -> Self {
        let core = CoreHandle::new(server_url_arg);
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let subscription = core.subscribe(Arc::new(EguiObserver {
            snapshot: Arc::clone(&snapshot),
        }));

        #[cfg(target_os = "macos")]
        let (_menu, always_on_top_item, borderless_item, transparent_item, click_through_item) = {
            let menu_bar = Menu::new();

            let app_menu = Submenu::with_items(
                "Claude Session Monitor",
                true,
                &[
                    &PredefinedMenuItem::about(None, None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::quit(None),
                ],
            )
            .expect("failed to create app menu");
            menu_bar
                .append(&app_menu)
                .expect("failed to append app menu");

            let always_on_top = CheckMenuItem::new("Always on Top", true, false, None);
            let borderless = CheckMenuItem::new("Borderless", true, false, None);
            let transparent = CheckMenuItem::new("Transparent", true, false, None);
            let click_through = CheckMenuItem::new("Click-through", true, false, None);
            let view_menu = Submenu::with_items(
                "View",
                true,
                &[&always_on_top, &borderless, &transparent, &click_through],
            )
            .expect("failed to create view menu");
            menu_bar
                .append(&view_menu)
                .expect("failed to append view menu");

            menu_bar.init_for_nsapp();
            (
                menu_bar,
                always_on_top,
                borderless,
                transparent,
                click_through,
            )
        };

        let local_hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_default();

        Self {
            core,
            _subscription: subscription,
            snapshot,
            pending_delete: None,
            local_hostname,
            always_on_top: false,
            borderless: false,
            vibrancy_enabled,
            transparent: false,
            click_through: false,
            #[cfg(target_os = "macos")]
            _menu,
            #[cfg(target_os = "macos")]
            always_on_top_item,
            #[cfg(target_os = "macos")]
            borderless_item,
            #[cfg(target_os = "macos")]
            transparent_item,
            #[cfg(target_os = "macos")]
            click_through_item,
        }
    }
}

fn status_color(status: &common::session::Status) -> egui::Color32 {
    match status {
        common::session::Status::Working(_) => egui::Color32::from_rgb(80, 200, 120),
        common::session::Status::Waiting(w) => match w.reason {
            common::session::WaitingReason::Permission => egui::Color32::from_rgb(220, 80, 80),
            common::session::WaitingReason::Input => egui::Color32::from_rgb(220, 160, 0),
        },
        common::session::Status::Ended => egui::Color32::GRAY,
    }
}

struct RenderContext<'a> {
    now: DateTime<Utc>,
    connected: bool,
    local_hostname: &'a str,
    pending_delete: &'a mut Option<String>,
    activation_errors: &'a mut HashMap<String, String>,
}

fn render_session(ui: &mut egui::Ui, session: &SessionView, ctx: &mut RenderContext<'_>) {
    let status_str = match &session.status {
        common::session::Status::Working(w) => match &w.tool {
            Some(tool) => format!("working({})", tool),
            None => "working".into(),
        },
        common::session::Status::Waiting(w) => {
            let reason = match w.reason {
                common::session::WaitingReason::Permission => "permission",
                common::session::WaitingReason::Input => "input",
            };
            let detail = w.detail.as_deref().unwrap_or("");
            if detail.is_empty() {
                format!("waiting({})", reason)
            } else {
                format!("waiting({}: {})", reason, detail)
            }
        }
        common::session::Status::Ended => "ended".into(),
    };

    let home = std::env::var("HOME").unwrap_or_default();
    let short_cwd = if !home.is_empty() && session.cwd.starts_with(&home) {
        format!("~{}", &session.cwd[home.len()..])
    } else {
        session.cwd.clone()
    };

    let repo_part = session.git_remote.as_deref().map(|remote| {
        let stripped = remote.strip_prefix("https://github.com/").unwrap_or(remote);
        let stripped = stripped.strip_suffix(".git").unwrap_or(stripped);
        stripped.to_owned()
    });
    let branch_repo = match (&session.git_branch, &repo_part) {
        (Some(b), Some(r)) => format!(" ({} \u{2192} {})", b, r),
        (Some(b), None) => format!(" ({})", b),
        _ => String::new(),
    };
    let line1 = match &session.hostname {
        Some(h) => format!("{}:{}{}", h, short_cwd, branch_repo),
        None => format!("{}{}", short_cwd, branch_repo),
    };

    let diff = ctx.now.signed_duration_since(session.updated_at);
    let relative_time = if diff.num_seconds() < 60 {
        format!("{}s ago", diff.num_seconds().max(0))
    } else {
        format!("{}m ago", diff.num_minutes())
    };

    let clickable = session.tmux_target.is_some();
    let faded = should_fade(ctx.connected, is_stale(session.updated_at, ctx.now));
    // Non-clickable sessions get extra dimming
    let dimmed = faded || !clickable;
    let color = {
        let base = status_color(&session.status);
        if dimmed {
            egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 80)
        } else {
            base
        }
    };
    let text_color = if dimmed {
        egui::Color32::from_rgba_unmultiplied(180, 180, 180, 80)
    } else {
        ui.visuals().text_color()
    };

    let group_response = ui
        .group(|ui| {
            ui.horizontal(|ui| {
                ui.colored_label(text_color, &line1);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{2715}").clicked() {
                        *ctx.pending_delete = Some(session.session_id.clone());
                    }
                });
            });
            ui.colored_label(color, format!("{:<20} {}", status_str, relative_time));

            // Show activation error inline if present
            if let Some(err) = ctx.activation_errors.get(&session.session_id) {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
            }
        })
        .response;

    // Clickable: pointer cursor + click handler
    if clickable {
        let response = group_response.interact(egui::Sense::click());
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            if let Err(e) = activation::activate(session, ctx.local_hostname) {
                ctx.activation_errors
                    .insert(session.session_id.clone(), e.to_string());
            }
        }
    } else {
        // Tooltip for non-clickable sessions
        group_response.on_hover_text("Not running in tmux");
    }

    ui.add_space(4.0);
}

impl eframe::App for App {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.transparent {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            let c = visuals.window_fill;
            [
                c.r() as f32 / 255.0,
                c.g() as f32 / 255.0,
                c.b() as f32 / 255.0,
                1.0,
            ]
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Confirmation dialog for deletion
        if let Some(session_id) = self.pending_delete.clone() {
            egui::Window::new("Confirm Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Delete session {}?", session_id));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            self.core.delete_session(session_id.clone());
                            self.pending_delete = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending_delete = None;
                        }
                    });
                });
        }

        // macOS: handle native menu events
        #[cfg(target_os = "macos")]
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *self.always_on_top_item.id() {
                self.always_on_top = self.always_on_top_item.is_checked();
                let level = if self.always_on_top {
                    egui::viewport::WindowLevel::AlwaysOnTop
                } else {
                    egui::viewport::WindowLevel::Normal
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            } else if event.id == *self.borderless_item.id() {
                self.borderless = self.borderless_item.is_checked();
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!self.borderless));
            } else if event.id == *self.transparent_item.id() {
                self.transparent = self.transparent_item.is_checked();
            } else if event.id == *self.click_through_item.id() {
                self.click_through = self.click_through_item.is_checked();
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(self.click_through));
            }
        }

        // Non-macOS: egui menu bar fallback
        #[cfg(not(target_os = "macos"))]
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("View", |ui| {
                    if ui
                        .checkbox(&mut self.always_on_top, "Always on top")
                        .changed()
                    {
                        let level = if self.always_on_top {
                            egui::viewport::WindowLevel::AlwaysOnTop
                        } else {
                            egui::viewport::WindowLevel::Normal
                        };
                        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                        ui.close_menu();
                    }
                    if ui.checkbox(&mut self.borderless, "Borderless").changed() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!self.borderless));
                        ui.close_menu();
                    }
                    if ui.checkbox(&mut self.transparent, "Transparent").changed() {
                        ui.close_menu();
                    }
                    if ui
                        .checkbox(
                            &mut self.click_through,
                            "Click-through (use Ctrl+Shift+C to disable)",
                        )
                        .changed()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(
                            self.click_through,
                        ));
                        ui.close_menu();
                    }
                });
            });
        });

        // Keyboard shortcut: Cmd+Shift+C (macOS) / Ctrl+Shift+C (others) toggles click-through
        if ctx.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.command && i.modifiers.shift) {
            self.click_through = !self.click_through;
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(self.click_through));
            #[cfg(target_os = "macos")]
            self.click_through_item.set_checked(self.click_through);
        }

        let central_frame = if self.vibrancy_enabled {
            egui::Frame::central_panel(&ctx.style()).fill(egui::Color32::TRANSPARENT)
        } else if self.transparent {
            egui::Frame::central_panel(&ctx.style())
                .fill(egui::Color32::from_rgba_unmultiplied(35, 35, 35, 200))
        } else {
            egui::Frame::central_panel(&ctx.style())
        };
        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ctx, |ui| {
                // When borderless, add a drag region so the window can still be moved
                if self.borderless {
                    let drag_rect = ui.allocate_space(egui::vec2(ui.available_width(), 20.0)).1;
                    let response = ui.interact(
                        drag_rect,
                        egui::Id::new("title_bar_drag"),
                        egui::Sense::drag(),
                    );
                    if response.dragged() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                }

                let (sessions, connected, mut activation_errors) = {
                    let s = self.snapshot.lock().unwrap();
                    (
                        s.sessions.clone(),
                        matches!(s.connection, Some(ConnectionState::Connected)),
                        s.activation_errors.clone(),
                    )
                };

                ui.horizontal(|ui| {
                    ui.heading("Claude Session Monitor");
                    let dot_color = if connected {
                        egui::Color32::from_rgb(80, 200, 120)
                    } else {
                        egui::Color32::from_rgb(220, 80, 80)
                    };
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, dot_color);
                });
                ui.separator();

                if sessions.is_empty() {
                    ui.label("No active sessions.");
                } else {
                    let now = Utc::now();
                    let (waiting, working) = partition_sessions(&sessions);

                    let mut render_ctx = RenderContext {
                        now,
                        connected,
                        local_hostname: &self.local_hostname,
                        pending_delete: &mut self.pending_delete,
                        activation_errors: &mut activation_errors,
                    };

                    if !waiting.is_empty() {
                        for session in &waiting {
                            render_session(ui, session, &mut render_ctx);
                        }
                        if !working.is_empty() {
                            ui.separator();
                        }
                    }

                    for session in &working {
                        render_session(ui, session, &mut render_ctx);
                    }
                }

                // Write back any new activation errors
                self.snapshot.lock().unwrap().activation_errors = activation_errors;
            });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

/// Platform-appropriate default log directory.
///
/// `common::telemetry::init` no longer guesses; each caller supplies the dir.
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

fn main() -> eframe::Result {
    let _sentry = common::sentry::init("gui");
    let args = Args::parse();
    let _guard = common::telemetry::init("gui", &args.log_level, &default_log_dir());

    let vibrancy = args.vibrancy;

    #[cfg(not(target_os = "macos"))]
    if vibrancy {
        tracing::warn!("--vibrancy is only supported on macOS; ignoring");
    }

    tracing::info!(vibrancy, "starting GUI");

    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut native_options = eframe::NativeOptions::default();

    #[cfg(target_os = "macos")]
    if vibrancy {
        native_options.viewport = native_options.viewport.with_transparent(true);
    }

    native_options.viewport = native_options.viewport.with_transparent(true);

    #[cfg(target_os = "macos")]
    if args.hide_from_dock {
        tracing::info!("hiding app from dock (activation policy: Accessory)");
        native_options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }));
    }

    #[cfg(not(target_os = "macos"))]
    if args.hide_from_dock {
        tracing::warn!("--hide-from-dock is only supported on macOS; ignoring");
    }

    let server_url_arg = args.server_url.clone();
    eframe::run_native(
        "Claude Session Monitor",
        native_options,
        Box::new(move |_cc| {
            #[cfg(target_os = "macos")]
            let cc = _cc;
            #[cfg(target_os = "macos")]
            if vibrancy {
                match window_vibrancy::apply_vibrancy(
                    cc,
                    window_vibrancy::NSVisualEffectMaterial::HudWindow,
                    Some(window_vibrancy::NSVisualEffectState::Active),
                    None,
                ) {
                    Ok(()) => tracing::info!("vibrancy applied"),
                    Err(e) => tracing::error!(?e, "failed to apply vibrancy"),
                }
            }

            Ok(Box::new(App::new(server_url_arg, vibrancy)))
        }),
    )
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn parse_all_args() {
        let args = Args::parse_from([
            "csm-gui",
            "--server-url",
            "http://custom:1234",
            "--log-level",
            "debug",
            "--vibrancy",
        ]);
        assert_eq!(args.server_url, Some("http://custom:1234".into()));
        assert_eq!(args.log_level, "debug");
        assert!(args.vibrancy);
    }

    #[test]
    fn defaults_when_no_args() {
        let args = Args::parse_from(["csm-gui"]);
        assert_eq!(args.server_url, None);
        assert_eq!(args.log_level, "info");
        assert!(!args.vibrancy);
        assert!(!args.hide_from_dock);
    }

    #[test]
    fn hide_from_dock_flag() {
        let args = Args::parse_from(["csm-gui", "--hide-from-dock"]);
        assert!(args.hide_from_dock);
    }
}
