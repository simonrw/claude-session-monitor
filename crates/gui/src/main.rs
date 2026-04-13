use common::api::resolve_server_url;
use common::sse::SseClient;
use eframe::egui;
use std::time::Duration;

struct App {
    sse: SseClient,
}

impl App {
    fn new() -> Self {
        let url = format!("{}/api/events", resolve_server_url(None));
        let sse = SseClient::new(&url);
        sse.start();
        Self { sse }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Claude Session Monitor");
            ui.separator();

            let sessions = self.sse.sessions();
            if sessions.is_empty() {
                ui.label("No active sessions.");
            } else {
                for session in &sessions {
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
                            format!("waiting({})", reason)
                        }
                        common::session::Status::Ended => "ended".into(),
                    };

                    // Shorten cwd: replace $HOME prefix with ~
                    let home = std::env::var("HOME").unwrap_or_default();
                    let short_cwd = if !home.is_empty() && session.cwd.starts_with(&home) {
                        format!("~{}", &session.cwd[home.len()..])
                    } else {
                        session.cwd.clone()
                    };

                    // Build line 1: hostname:~/path (branch → user/repo)
                    let repo_part = session.git_remote.as_deref().map(|remote| {
                        let stripped = remote
                            .strip_prefix("https://github.com/")
                            .unwrap_or(remote);
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

                    // Build line 2: status + relative time
                    let now = chrono::Utc::now();
                    let diff = now.signed_duration_since(session.updated_at);
                    let relative_time = if diff.num_seconds() < 60 {
                        format!("{}s ago", diff.num_seconds().max(0))
                    } else {
                        format!("{}m ago", diff.num_minutes())
                    };
                    let line2 = format!("{:<20} {}", status_str, relative_time);

                    ui.group(|ui| {
                        ui.label(&line1);
                        ui.label(&line2);
                    });
                    ui.add_space(4.0);
                }
            }
        });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Claude Session Monitor",
        native_options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}
