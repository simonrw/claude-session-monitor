use chrono::{DateTime, Utc};
use clap::Parser;
use common::api::{SessionView, resolve_server_url};
use common::sse::SseClient;
use eframe::egui;
#[cfg(target_os = "macos")]
use muda::{CheckMenuItem, Menu, MenuEvent, PredefinedMenuItem, Submenu};
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
            updated_at,
            hostname: None,
            git_branch: None,
            git_remote: None,
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

struct App {
    sse: SseClient,
    server_url: String,
    pending_delete: Option<String>,
    always_on_top: bool,
    borderless: bool,
    vibrancy_enabled: bool,
    transparent: bool,
    #[cfg(target_os = "macos")]
    _menu: Menu,
    #[cfg(target_os = "macos")]
    always_on_top_item: CheckMenuItem,
    #[cfg(target_os = "macos")]
    borderless_item: CheckMenuItem,
    #[cfg(target_os = "macos")]
    transparent_item: CheckMenuItem,
}

impl App {
    fn new(server_url_arg: Option<&str>, file_url: &str, vibrancy_enabled: bool) -> Self {
        let server_url = resolve_server_url(server_url_arg, Some(file_url));
        let sse_url = format!("{}/api/events", server_url);
        tracing::info!(server_url, sse_url, "connecting to server");
        let sse = SseClient::new(&sse_url);
        sse.start();

        #[cfg(target_os = "macos")]
        let (_menu, always_on_top_item, borderless_item, transparent_item) = {
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
            let view_menu =
                Submenu::with_items("View", true, &[&always_on_top, &borderless, &transparent])
                    .expect("failed to create view menu");
            menu_bar
                .append(&view_menu)
                .expect("failed to append view menu");

            menu_bar.init_for_nsapp();
            (menu_bar, always_on_top, borderless, transparent)
        };

        Self {
            sse,
            server_url,
            pending_delete: None,
            always_on_top: false,
            borderless: false,
            vibrancy_enabled,
            transparent: false,
            #[cfg(target_os = "macos")]
            _menu,
            #[cfg(target_os = "macos")]
            always_on_top_item,
            #[cfg(target_os = "macos")]
            borderless_item,
            #[cfg(target_os = "macos")]
            transparent_item,
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

fn render_session(
    ui: &mut egui::Ui,
    session: &SessionView,
    now: DateTime<Utc>,
    connected: bool,
    pending_delete: &mut Option<String>,
) {
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

    let diff = now.signed_duration_since(session.updated_at);
    let relative_time = if diff.num_seconds() < 60 {
        format!("{}s ago", diff.num_seconds().max(0))
    } else {
        format!("{}m ago", diff.num_minutes())
    };

    let faded = should_fade(connected, is_stale(session.updated_at, now));
    let color = {
        let base = status_color(&session.status);
        if faded {
            egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 80)
        } else {
            base
        }
    };
    let text_color = if faded {
        egui::Color32::from_rgba_unmultiplied(180, 180, 180, 80)
    } else {
        ui.visuals().text_color()
    };

    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.colored_label(text_color, &line1);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("\u{2715}").clicked() {
                    *pending_delete = Some(session.session_id.clone());
                }
            });
        });
        ui.colored_label(color, format!("{:<20} {}", status_str, relative_time));
    });
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
                            let url = format!("{}/api/sessions/{}", self.server_url, session_id);
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
                });
            });
        });

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

                let connected = self.sse.is_connected();

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

                let sessions = self.sse.sessions();
                if sessions.is_empty() {
                    ui.label("No active sessions.");
                } else {
                    let now = Utc::now();
                    let (waiting, working) = partition_sessions(&sessions);

                    if !waiting.is_empty() {
                        for session in &waiting {
                            render_session(ui, session, now, connected, &mut self.pending_delete);
                        }
                        if !working.is_empty() {
                            ui.separator();
                        }
                    }

                    for session in &working {
                        render_session(ui, session, now, connected, &mut self.pending_delete);
                    }
                }
            });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn setup_tracing(log_level: &str) -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join(".local/share/claude-session-monitor");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "gui.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();

    tracing::info!(log_dir = %log_dir.display(), "logging initialized");
    guard
}

fn main() -> eframe::Result {
    let _sentry = common::sentry::init("gui");
    let args = Args::parse();
    let _guard = setup_tracing(&args.log_level);

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

    let config = match common::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    native_options.viewport = native_options.viewport.with_transparent(true);

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

            Ok(Box::new(App::new(
                args.server_url.as_deref(),
                &config.server.url,
                vibrancy,
            )))
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
    }
}
