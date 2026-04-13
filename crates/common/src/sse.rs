use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::SessionView;

pub struct SseClient {
    url: String,
    sessions: Arc<Mutex<Vec<SessionView>>>,
}

impl SseClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn start(&self) {
        let url = self.url.clone();
        let sessions = Arc::clone(&self.sessions);

        thread::spawn(move || {
            loop {
                tracing::debug!(url, "connecting to SSE stream");
                match connect_and_stream(&url, &sessions) {
                    Ok(()) => {
                        tracing::debug!("SSE stream ended, reconnecting");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SSE connection error, reconnecting in 1s");
                        thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        });
    }

    pub fn sessions(&self) -> Vec<SessionView> {
        self.sessions.lock().unwrap().clone()
    }
}

fn connect_and_stream(
    url: &str,
    sessions: &Arc<Mutex<Vec<SessionView>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, BufReader};

    let response = reqwest::blocking::Client::new()
        .get(url)
        .header("Accept", "text/event-stream")
        .send()?;

    tracing::debug!(status = %response.status(), "SSE connection established");

    let reader = BufReader::new(response);

    for line in reader.lines() {
        let line = line?;
        if let Some(data) = line.strip_prefix("data: ") {
            match serde_json::from_str::<Vec<SessionView>>(data) {
                Ok(views) => {
                    tracing::debug!(session_count = views.len(), "received SSE update");
                    *sessions.lock().unwrap() = views;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        data_prefix = &data[..data.len().min(100)],
                        "failed to parse SSE data"
                    );
                }
            }
        }
    }

    Ok(())
}
