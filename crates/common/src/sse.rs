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
                match connect_and_stream(&url, &sessions) {
                    Ok(()) => {}
                    Err(_) => {
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

    let reader = BufReader::new(response);

    for line in reader.lines() {
        let line = line?;
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(views) = serde_json::from_str::<Vec<SessionView>>(data) {
                *sessions.lock().unwrap() = views;
            }
        }
    }

    Ok(())
}
