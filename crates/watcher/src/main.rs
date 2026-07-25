use std::collections::HashMap;
use std::path::PathBuf;

use clap::Parser;
use common::api::resolve_server_url;
use watcher::discovery::{Discovery, DiscoveryError};
use watcher::git::GitCache;
use watcher::{discovery, publish, sweep};

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
    // Owned here, not inside `run_once`, so a future polling loop (PRO-210)
    // can hold one `GitCache` across every sweep - a fresh cache per sweep
    // would defeat its whole purpose, since every lookup would always miss.
    let git_cache = GitCache::new(
        watcher::git::DEFAULT_TTL,
        watcher::git::DEFAULT_COMMAND_TIMEOUT,
    );
    run_once(&server_url, &git_cache);
}

/// Decide which registry directories to sweep, and log accordingly.
///
/// This is where PRO-207's original protection - "never publish an empty
/// snapshot because a read failed" - now lives, adjusted for what an empty
/// result means once discovery exists:
///
/// * [`sweep::REGISTRY_DIRS_ENV`] non-empty: the explicit override is
///   present, `discover` is never called, and its directories are used
///   exactly as configured (the escape hatch and test seam PRO-208 promises
///   to preserve).
/// * The override is absent (unset or blank) and `discover` returns `Ok`:
///   process enumeration succeeded. Its result is used as-is. In practice
///   `discover`'s `registry_dirs` is never actually empty - it always
///   seeds at least the default config directory (see
///   `discovery::union_discovery`'s doc comment) as a floor against a total
///   `is_claude_exe` miss - but this function makes no assumption either
///   way: even a hypothetically empty `Ok` is used as-is rather than
///   treated as an error, since a successful discovery finding nothing
///   extra is a self-healing answer, not a failure to guard against.
/// * The override is absent and `discover` returns `Err`: enumeration
///   itself failed (e.g. the `ps` invocation errored, or `/proc` could not
///   be read), so the true set of live sessions is unknown. This is
///   indistinguishable from "no sessions exist" once it reaches `sweep`, so
///   it must not be allowed to produce a snapshot at all - the caller must
///   refuse to publish, exactly as PRO-207 did for an unconfigured
///   override.
///
/// `discover` is taken as a closure (rather than calling
/// `discovery::discover` directly) purely so tests can prove the override
/// truly bypasses directory discovery - by passing a closure that panics if
/// called - without touching this host's real processes.
///
/// Returns the full `Discovery`, not just `registry_dirs`: PRO-209 needs
/// `tmux_panes` too, to resolve each session's activation target. On the
/// explicit-override path, `registry_dirs` comes from `explicit` and
/// discovery's own directory search is never run - but `tmux_panes` still
/// comes from `discover_panes`, a *second*, independently injected closure
/// that performs the same process read purely for pane capture. This fixes
/// finding 3 from the PRO-209 review: `CSM_WATCHER_REGISTRY_DIRS` is
/// documented (PRO-204) as a permanent, supported escape hatch, not
/// scaffolding, so a session published while it is set must still resolve
/// a `tmux_target` like any other - losing it silently was a real,
/// user-visible downgrade, not a correct degrade. `discover_panes` must
/// never fail this function: pane capture is enrichment, not truth about
/// which sessions exist, so a failure there degrades to an empty map (see
/// `discovery::discover_tmux_panes`'s doc comment), exactly like `tmux`'s
/// own degrade when `tmux` itself is unavailable.
fn resolve_registry_dirs(
    explicit: Vec<PathBuf>,
    discover: impl FnOnce() -> Result<Discovery, DiscoveryError>,
    discover_panes: impl FnOnce() -> HashMap<i32, String>,
) -> Result<Discovery, DiscoveryError> {
    if !explicit.is_empty() {
        let tmux_panes = discover_panes();
        tracing::debug!(
            dir_count = explicit.len(),
            tmux_pane_count = tmux_panes.len(),
            env_var = sweep::REGISTRY_DIRS_ENV,
            "using explicit registry directories; directory discovery bypassed, pane capture \
             still run"
        );
        return Ok(Discovery {
            registry_dirs: explicit,
            tmux_panes,
        });
    }

    tracing::debug!(
        env_var = sweep::REGISTRY_DIRS_ENV,
        "no explicit registry directories configured; discovering from live Claude processes"
    );
    let found = discover()?;
    tracing::info!(
        dir_count = found.registry_dirs.len(),
        tmux_pane_count = found.tmux_panes.len(),
        "discovery complete"
    );
    Ok(found)
}

fn run_once(server_url: &str, git_cache: &GitCache) {
    let explicit = sweep::registry_dirs_from_env();
    let discovery = match resolve_registry_dirs(
        explicit,
        discovery::discover,
        discovery::discover_tmux_panes,
    ) {
        Ok(discovery) => discovery,
        Err(e) => {
            tracing::error!(
                error = %e,
                "failed to discover registry directories; refusing to publish an empty snapshot"
            );
            eprintln!(
                "failed to discover Claude Code registry directories ({e}); refusing to publish \
                 an empty snapshot. Set {} to override discovery explicitly.",
                sweep::REGISTRY_DIRS_ENV
            );
            std::process::exit(1);
        }
    };
    let dirs = discovery.registry_dirs;

    if dirs.is_empty() {
        // In the current implementation this is effectively unreachable:
        // `discovery::discover` always seeds at least the default config
        // directory into `registry_dirs` (see `union_discovery`'s doc
        // comment), and the explicit-override path is never empty either
        // (see `resolve_registry_dirs`). Kept as a defensive guard rather
        // than removed, since `dirs` still reaches here as a plain `Vec`
        // with no type-level guarantee it is non-empty.
        //
        // The wording matters: this branch is immediately followed by
        // `sweep::sweep` and `publish::publish`, which will POST an
        // empty snapshot - and an empty snapshot **ends every
        // previously-published Claude session on this host** (the server
        // ends every session absent from a published snapshot). The
        // previous wording here ("nothing to report") described this as a
        // no-op, which it is not; say plainly what is about to happen.
        tracing::warn!(
            "no registry directories to sweep; publishing an empty snapshot, which will end \
             every previously-published Claude session on this host"
        );
        eprintln!(
            "no registry directories to sweep; publishing an empty snapshot, which will end \
             every previously-published Claude session on this host"
        );
    }

    let sessions = sweep::sweep(&dirs, &discovery.tmux_panes, git_cache);
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
    use std::collections::HashMap;

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

    #[test]
    fn resolve_registry_dirs_uses_explicit_override_without_calling_directory_discovery() {
        let explicit = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        let tmux_panes = HashMap::from([(123, "%9".to_string())]);
        let result = resolve_registry_dirs(
            explicit.clone(),
            || panic!("directory discovery must not be called when an explicit override is set"),
            || tmux_panes.clone(),
        );
        let discovery = result.unwrap();
        assert_eq!(discovery.registry_dirs, explicit);
        assert_eq!(
            discovery.tmux_panes, tmux_panes,
            "the override bypasses directory discovery only - pane capture is a second, \
             independently injected seam that must still run, or tmux enrichment is silently \
             lost for every session published while the override is set (finding 3 from the \
             PRO-209 review)"
        );
    }

    #[test]
    fn resolve_registry_dirs_falls_back_to_discovery_when_explicit_is_empty() {
        let discovered = vec![PathBuf::from("/home/alice/.claude")];
        let tmux_panes = HashMap::from([(123, "%9".to_string())]);
        let result = resolve_registry_dirs(
            Vec::new(),
            || {
                Ok(Discovery {
                    registry_dirs: discovered.clone(),
                    tmux_panes: tmux_panes.clone(),
                })
            },
            || panic!("pane capture must not run separately when discovery already ran"),
        );
        let discovery = result.unwrap();
        assert_eq!(discovery.registry_dirs, discovered);
        assert_eq!(
            discovery.tmux_panes, tmux_panes,
            "tmux_panes from a successful discovery must be passed through, not dropped"
        );
    }

    #[test]
    fn resolve_registry_dirs_propagates_discovery_success_with_zero_dirs() {
        // Enumeration succeeding but finding no live Claude processes must
        // surface as an empty `Ok`, not an error - this is the self-healing
        // "nothing running" case, which `run_once` must treat as a valid
        // empty snapshot rather than refusing to publish.
        let result = resolve_registry_dirs(
            Vec::new(),
            || Ok(Discovery::default()),
            || panic!("pane capture must not run separately when discovery already ran"),
        );
        assert_eq!(result.unwrap().registry_dirs, Vec::<PathBuf>::new());
    }

    #[test]
    fn resolve_registry_dirs_propagates_discovery_error() {
        // Enumeration itself failing (e.g. `ps` could not be run) must
        // surface as an `Err`, never silently degrade into an empty `Ok` -
        // that distinction is what keeps a failed read from being
        // published as "no sessions".
        let result = resolve_registry_dirs(
            Vec::new(),
            || Err(DiscoveryError::Enumerate(std::io::Error::other("boom"))),
            || panic!("pane capture must not run separately when discovery already ran"),
        );
        assert!(result.is_err());
    }
}
