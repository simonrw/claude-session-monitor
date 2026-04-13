mod hook;
mod enrichment;

use common::api::{ReportPayload, resolve_server_url};

fn setup_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join(".local/share/claude-session-monitor");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "reporter.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("reporter=debug")),
        )
        .init();

    guard
}

fn main() {
    let _guard = setup_tracing();

    let input = match read_stdin() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to read stdin");
            return;
        }
    };

    let event: hook::HookEvent = match serde_json::from_str(&input) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "failed to parse hook event JSON");
            return;
        }
    };

    let span = tracing::info_span!(
        "report",
        session_id = %event.session_id,
        cwd = %event.cwd,
        hook_event_name = %event.hook_event_name,
    );
    let _enter = span.enter();

    tracing::debug!("processing hook event");

    let status = hook::derive_status(&event);
    tracing::debug!(status = ?status, "derived status");

    let enrichment = enrichment::gather(&event.cwd);

    let payload = ReportPayload {
        session_id: event.session_id,
        cwd: enrichment.cwd,
        status,
        hook_event_name: event.hook_event_name,
        tool_name: event.tool_name,
        tool_input: event.tool_input,
        notification_type: event.notification_type,
        hostname: enrichment.hostname,
        git_branch: enrichment.git_branch,
        git_remote: enrichment.git_remote,
    };

    let url = format!("{}/api/sessions", resolve_server_url(None));
    tracing::debug!(url = %url, "posting to server");
    let result = reqwest::blocking::Client::new()
        .post(&url)
        .json(&payload)
        .send();
    match result {
        Ok(resp) => tracing::debug!(status = %resp.status(), "server responded"),
        Err(e) => tracing::error!(error = %e, "failed to post to server"),
    }
}

fn read_stdin() -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
