use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::SessionView;

pub trait SseUpdateHandler: Send + Sync {
    fn on_update(&self, sessions: Vec<SessionView>, connected: bool);
}

struct SseState {
    sessions: Vec<SessionView>,
    connected: bool,
}

pub struct SseClient {
    url: String,
    state: Arc<Mutex<SseState>>,
    handler: Arc<Mutex<Option<Arc<dyn SseUpdateHandler>>>>,
}

impl SseClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            state: Arc::new(Mutex::new(SseState {
                sessions: Vec::new(),
                connected: false,
            })),
            handler: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_handler(&self, handler: Arc<dyn SseUpdateHandler>) {
        *self.handler.lock().unwrap() = Some(handler);
    }

    pub fn start(&self) {
        let url = self.url.clone();
        let state = Arc::clone(&self.state);
        let handler = Arc::clone(&self.handler);

        thread::spawn(move || {
            loop {
                tracing::debug!(url, "connecting to SSE stream");
                match connect_and_stream(&url, &state, &handler) {
                    Ok(()) => {
                        tracing::debug!("SSE stream ended, reconnecting");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SSE connection error, reconnecting in 1s");
                        #[cfg(feature = "sentry")]
                        {
                            ::sentry::capture_error(&*e);
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                }
                {
                    let mut s = state.lock().unwrap();
                    s.connected = false;
                }
                notify(&handler, &state);
            }
        });
    }

    pub fn sessions(&self) -> Vec<SessionView> {
        self.state.lock().unwrap().sessions.clone()
    }

    pub fn is_connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }
}

fn notify(handler: &Arc<Mutex<Option<Arc<dyn SseUpdateHandler>>>>, state: &Arc<Mutex<SseState>>) {
    let h = handler.lock().unwrap().clone();
    if let Some(h) = h {
        let (sessions, connected) = {
            let s = state.lock().unwrap();
            (s.sessions.clone(), s.connected)
        };
        h.on_update(sessions, connected);
    }
}

fn connect_and_stream(
    url: &str,
    state: &Arc<Mutex<SseState>>,
    handler: &Arc<Mutex<Option<Arc<dyn SseUpdateHandler>>>>,
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
                    {
                        let mut s = state.lock().unwrap();
                        s.sessions = views;
                        s.connected = true;
                    }
                    notify(handler, state);
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
