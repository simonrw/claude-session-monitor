use clap::Parser;
use common::api::resolve_server_url;
use watcher::{publish, sweep};

#[derive(Parser, Debug)]
#[command(
    name = "claude-session-monitor-watcher",
    about = "Claude session monitor watcher"
)]
struct Args {
    /// Server URL (e.g. http://localhost:7685)
    #[arg(long)]
    server_url: Option<String>,

    /// Perform a single sweep of the registry and exit
    // Continuous polling is added by PRO-210; until then this is the only
    // supported mode, so it must be passed explicitly. `required = true`
    // makes clap enforce that with its own usage output, rather than a
    // hand-rolled check after parsing. Note the non-doc comment: doc
    // comments on a clap field are rendered verbatim in `--help`, so
    // implementation notes and ticket references must not live in one.
    #[arg(long, required = true)]
    once: bool,
}

fn setup_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join(".local/share/claude-session-monitor");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "watcher.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Events from this binary's own `main.rs` are logged under
                // target `csm_watcher` (the binary/package name), while
                // events from the library code in `crates/watcher/src`
                // (sweep, registry, publish, status) are logged under
                // `watcher` (the `[lib] name` in Cargo.toml). Both must be
                // covered, or the sweep's own log lines - emitted from
                // `main.rs` - never appear.
                tracing_subscriber::EnvFilter::new("csm_watcher=debug,watcher=debug")
            }),
        )
        .init();

    guard
}

fn main() {
    // Install sentry's panic hook before tracing_subscriber so the chain is:
    // sentry hook -> previous (default) hook. tracing's init won't clobber it.
    let _sentry = common::sentry::init("watcher");

    let args = Args::parse();
    let _guard = setup_tracing();

    let config = match common::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let server_url = resolve_server_url(args.server_url.as_deref(), Some(&config.server.url));
    run_once(&server_url);
}

fn run_once(server_url: &str) {
    let dirs = sweep::registry_dirs_from_env();
    tracing::debug!(dir_count = dirs.len(), "starting sweep");

    if dirs.is_empty() {
        // An empty directory list is indistinguishable from "no sessions
        // exist" once it reaches `sweep`, which would publish an empty
        // snapshot and end every live session on this host. Automatic
        // discovery (PRO-208) hasn't landed yet, so an empty
        // `CSM_WATCHER_REGISTRY_DIRS` is the default, reachable state
        // today, not a misconfiguration - refuse to publish anything
        // rather than risk that wipe.
        tracing::error!(
            env_var = sweep::REGISTRY_DIRS_ENV,
            "no registry directories configured; refusing to publish an empty snapshot"
        );
        eprintln!(
            "no registry directories configured (set {}); refusing to publish an empty snapshot",
            sweep::REGISTRY_DIRS_ENV
        );
        std::process::exit(1);
    }

    let sessions = sweep::sweep(&dirs);
    tracing::info!(session_count = sessions.len(), "sweep complete");

    if let Err(e) = publish::publish(server_url, sessions) {
        tracing::error!(error = %e, "failed to publish snapshot");
        common::sentry::capture_error(&e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_server_url_and_once() {
        let args = Args::parse_from([
            "csm-watcher",
            "--server-url",
            "http://custom:1234",
            "--once",
        ]);
        assert_eq!(args.server_url, Some("http://custom:1234".into()));
        assert!(args.once);
    }

    #[test]
    fn defaults_server_url_to_none_when_once_is_passed() {
        let args = Args::parse_from(["csm-watcher", "--once"]);
        assert_eq!(args.server_url, None);
        assert!(args.once);
    }

    #[test]
    fn missing_once_flag_is_rejected_by_clap() {
        // `--once` is `required = true`: omitting it must be a clap parse
        // error (surfaced to the user as clap's own usage output), not a
        // successful parse followed by a hand-rolled runtime check.
        let result = Args::try_parse_from(["csm-watcher"]);
        assert!(result.is_err(), "parsing without --once must fail");
    }
}
