//! Tracing initialisation shared across binaries.
//!
//! Callers supply both the `app_label` (used as the log-file stem) and the
//! `log_dir` to write rotated logs into. The `common` crate intentionally
//! does not guess a platform-appropriate directory — bins (`gui`, `reporter`,
//! `server`) and foreign hosts (mac/iOS via `core-ffi`) pick one appropriate
//! for their platform and pass it in.
//!
//! The returned [`Guard`] must be kept alive for the duration of the process;
//! dropping it flushes the non-blocking writer.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;

/// RAII guard that must outlive all tracing calls.
pub struct Guard {
    _worker: WorkerGuard,
}

/// Initialise tracing for a binary. `app_label` determines the log file name,
/// written into `log_dir` (created if missing).
///
/// `log_level` is a directive string (e.g. `"info"`, `"debug"`). The env var
/// `RUST_LOG` overrides it if set.
pub fn init(app_label: &str, log_level: &str, log_dir: &Path) -> Guard {
    std::fs::create_dir_all(log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(log_dir, format!("{app_label}.log"));
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
