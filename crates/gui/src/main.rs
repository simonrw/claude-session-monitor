use chrono::{DateTime, Utc};
use common::api::{SessionView, resolve_server_url};
use common::sse::SseClient;
use eframe::egui;
use std::time::Duration;

fn is_stale(updated_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(updated_at) >= chrono::Duration::minutes(30)
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
    working.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
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
}

impl App {
    fn new() -> Self {
        let server_url = resolve_server_url(None);
        let sse_url = format!("{}/api/events", server_url);
        let sse = SseClient::new(&sse_url);
        sse.start();
        Self {
            sse,
            server_url,
            pending_delete: None,
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

    let stale = is_stale(session.updated_at, now);
    let color = {
        let base = status_color(&session.status);
        if stale {
            egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 80)
        } else {
            base
        }
    };
    let text_color = if stale {
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
                            std::thread::spawn(move || {
                                let client = reqwest::blocking::Client::new();
                                match client.delete(&url).send() {
                                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                                        eprintln!("Session {} not found for deletion", session_id);
                                    }
                                    Ok(resp) if !resp.status().is_success() => {
                                        eprintln!(
                                            "Delete session {} failed: {}",
                                            session_id,
                                            resp.status()
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!("Delete request error: {}", e);
                                    }
                                    _ => {}
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

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Claude Session Monitor");
            ui.separator();

            let sessions = self.sse.sessions();
            if sessions.is_empty() {
                ui.label("No active sessions.");
            } else {
                let now = Utc::now();
                let (waiting, working) = partition_sessions(&sessions);

                if !waiting.is_empty() {
                    for session in &waiting {
                        render_session(ui, session, now, &mut self.pending_delete);
                    }
                    if !working.is_empty() {
                        ui.separator();
                    }
                }

                for session in &working {
                    render_session(ui, session, now, &mut self.pending_delete);
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
