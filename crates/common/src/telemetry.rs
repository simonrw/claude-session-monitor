//! Tracing initialisation shared across binaries.
//!
//! Log files land in a platform-appropriate directory named after the supplied
//! `app_label` — "gui" for the egui app, "mac" for the native macOS app, etc.
//! The returned [`Guard`] must be kept alive for the duration of the process;
//! dropping it flushes the non-blocking writer.

use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;

/// RAII guard that must outlive all tracing calls.
pub struct Guard {
    _worker: WorkerGuard,
}

/// Initialise tracing for a binary. `app_label` determines the log file name.
///
/// `log_level` is a directive string (e.g. `"info"`, `"debug"`). The env var
/// `RUST_LOG` overrides it if set.
pub fn init(app_label: &str, log_level: &str) -> Guard {
    let log_dir = log_directory();
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, format!("{app_label}.log"));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();

    tracing::info!(
        log_dir = %log_dir.display(),
        app_label,
        "logging initialized"
    );
    Guard { _worker: guard }
}

fn log_directory() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));

    if cfg!(target_os = "macos") {
        home.join("Library/Logs/claude-session-monitor")
    } else {
        home.join(".local/share/claude-session-monitor")
    }
}
