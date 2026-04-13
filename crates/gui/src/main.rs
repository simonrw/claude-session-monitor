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
                            Some(tool) => format!("working ({})", tool),
                            None => "working".into(),
                        },
                        common::session::Status::Waiting(w) => {
                            let reason = match w.reason {
                                common::session::WaitingReason::Permission => "permission",
                                common::session::WaitingReason::Input => "input",
                            };
                            format!("waiting ({})", reason)
                        }
                        common::session::Status::Ended => "ended".into(),
                    };
                    ui.label(format!(
                        "{}: {} ({})",
                        session.session_id, session.cwd, status_str
                    ));
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
