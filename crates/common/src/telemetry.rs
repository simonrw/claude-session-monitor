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
use tracing_appender::rolling::{Builder, Rotation};

/// RAII guard that must outlive all tracing calls.
pub struct Guard {
    _worker: WorkerGuard,
}

/// How many rotated daily log files to retain before the oldest is deleted.
///
/// Every current caller (`gui`, `core-ffi` on behalf of the mac/iOS apps, and
/// `watcher`) shares this one retention: `rolling::daily` (the previous
/// implementation) never pruned at all, so a caller logging at a couple of
/// lines a second - the watcher's own polling loop is the worst case here,
/// since it is the only caller that logs on a fixed short interval rather
/// than on user or hook-driven events - grew without bound: measured
/// directly at roughly 27MB/day for the watcher at its default two-second
/// poll interval and its pre-fix default `debug` level, which is on the
/// order of 10GB/year with nothing ever deleting it. 14 days is a deliberate
/// trade: generous enough to look back at "what happened last week" for a
/// hand-rolled bug report, while bounding worst-case disk use to roughly two
/// weeks of the watcher's own volume rather than an unbounded amount - and
/// small relative to typical free disk space even at the watcher's own
/// worst-case rate. `gui` and `core-ffi` log at far lower volume (UI/session
/// events, not a fixed poll), so the same cap costs them negligible space.
const MAX_LOG_FILES: usize = 14;

/// Initialise tracing for a binary. `app_label` determines the log file name,
/// written into `log_dir` (created if missing), rotated daily and pruned to
/// the most recent [`MAX_LOG_FILES`] files.
///
/// `log_level` is a directive string (e.g. `"info"`, `"debug"`). The env var
/// `RUST_LOG` overrides it if set.
pub fn init(app_label: &str, log_level: &str, log_dir: &Path) -> Guard {
    std::fs::create_dir_all(log_dir).ok();

    let file_appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(format!("{app_label}.log"))
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)
        .unwrap_or_else(|e| {
            panic!(
                "failed to build rolling file appender in {}: {e}",
                log_dir.display()
            )
        });
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
