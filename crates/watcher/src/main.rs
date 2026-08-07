use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use common::api::{AgentKind, SnapshotSession, resolve_server_url};
use watcher::debounce::Debounce;
use watcher::discovery::{
    Discovery, DiscoveryError, ForeignUidWarnings, ProcessCache, ProcessSnapshot,
};
use watcher::git::GitCache;
use watcher::sweep::OrphanWarnings;
use watcher::{discovery, publish, sweep};

/// Default poll period between sweeps, as the humantime string clap's
/// `default_value` takes: frequent enough that a state change (a session
/// ending, a status flipping) appears within a couple of seconds, matching
/// PRO-204's target and the two-second figure every enrichment module in
/// this crate (`git`, `tmux`, `command`) already documents its own timeouts
/// against.
///
/// Kept as the humantime string clap needs, rather than a `Duration`
/// constant, since `default_value` (unlike `default_value_t`) takes a
/// string that is fed through the same `parse_interval` every explicit
/// `--interval` is - so the default is validated exactly like any operator-
/// supplied value, with no separate "trust me, this Duration is fine" path.
const DEFAULT_INTERVAL: &str = "2s";

/// Lower bound on `--interval`, enforced at parse time, and the floor a
/// [`Backoff`]'s base is clamped to.
///
/// A zero (or merely tiny) interval degenerates in two independent ways
/// without this floor, both reproduced directly against the pre-fix code:
///
/// * On the success path, `run_daemon` sleeps for exactly `interval`
///   between cycles with no floor of its own - `--interval-ms 0` is a true
///   busy loop, discovering and publishing as fast as the CPU and the
///   network allow.
/// * On the failure path, `Backoff::fail` computes its next wait as
///   `self.current.saturating_mul(2)`; starting from a `base` (and
///   therefore an initial `current`) of zero, doubling zero forever leaves
///   the wait at zero forever, so a down server is retried as fast as
///   possible rather than backed off from at all. Measured directly against
///   a dead server: 48 failed publish attempts in 10 seconds at 22-27% CPU
///   with `--interval-ms 0` before this floor existed.
///
/// 100ms is small enough to stay well under any interval this crate's own
/// tests need (the daemon backoff integration test in
/// `crates/server/tests/reconciliation.rs` uses exactly this value as its
/// base), while being nowhere near zero.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Explicit request timeout for the watcher's shared publish HTTP client.
/// See [`build_http_client`] for the full rationale.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);

/// Parse and validate `--interval`: a humantime duration string (`2s`,
/// `500ms`, ...) no smaller than [`MIN_INTERVAL`].
fn parse_interval(s: &str) -> Result<Duration, String> {
    let duration: Duration = s
        .parse::<humantime::Duration>()
        .map_err(|e| format!("invalid duration {s:?}: {e}"))?
        .into();
    if duration < MIN_INTERVAL {
        return Err(format!(
            "--interval must be at least {MIN_INTERVAL:?}, got {duration:?} (from {s:?}); an \
             interval this small turns a down server into a busy loop, since the retry backoff \
             can never wait longer than the configured interval between successes"
        ));
    }
    Ok(duration)
}

#[derive(Parser, Debug)]
#[command(
    name = "claude-session-monitor-watcher",
    about = "Claude session monitor watcher"
)]
struct Args {
    /// Server URL (e.g. http://localhost:7685)
    #[arg(long)]
    server_url: Option<String>,

    /// Perform a single sweep of the registry and exit, instead of running
    /// continuously
    #[arg(long)]
    once: bool,

    /// Poll period between sweeps, as a duration such as `2s` or `500ms`,
    /// when running continuously (ignored with --once). Minimum 100ms
    // This doc comment is rendered verbatim in `--help`, so it must name a
    // concrete value rather than the `MIN_INTERVAL` constant.
    #[arg(long, default_value = DEFAULT_INTERVAL, value_parser = parse_interval)]
    interval: Duration,
}

/// Platform log directory for the watcher's own rotating log, matching the
/// reporter's (`crates/reporter/src/main.rs`'s `setup_tracing`) choice
/// exactly - the daemon's acceptance criteria call for diagnosing it "the
/// same way" as the reporter, not a different scheme like the GUI's
/// platform-specific `Library/Logs` path.
fn default_log_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".local/share/claude-session-monitor")
}

fn main() {
    // Install sentry's panic hook before tracing_subscriber so the chain is:
    // sentry hook -> previous (default) hook. tracing's init won't clobber it.
    let _sentry = common::sentry::init("watcher");

    let args = Args::parse();
    // Routed through the shared telemetry module (`common::telemetry`)
    // rather than a hand-rolled `tracing_subscriber` setup of this binary's
    // own, so the watcher does not add another copy of it. Note the
    // reporter still hand-rolls its own in `crates/reporter/src/main.rs`,
    // and so its log is still unpruned; converging that one is not part of
    // this change.
    //
    // Events from this
    // binary's own `main.rs` are logged under target `csm_watcher` (the
    // binary/package name), while events from the library code in
    // `crates/watcher/src` (sweep, registry, publish, status) are logged
    // under `watcher` (the `[lib] name` in Cargo.toml); both must be
    // covered, or the sweep's own log lines - emitted from `main.rs` - never
    // appear.
    // `info` by default, not `debug`: at `debug`, every sweep logs several
    // lines (the discovery/publish detail below `info`), which at the
    // default two-second poll interval measured out to roughly 27MB/day -
    // on the order of 10GB/year - with nothing pruning it before this fix.
    // `RUST_LOG` still overrides this for anyone who wants `debug` back
    // temporarily.
    let _guard = common::telemetry::init(
        "watcher",
        "csm_watcher=info,watcher=info",
        &default_log_dir(),
    );

    let config = match common::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let server_url = resolve_server_url(args.server_url.as_deref(), Some(&config.server.url));
    // Owned here, not inside `run_once`/`run_daemon`, so it is held across
    // every sweep for the life of the process - a fresh cache per sweep
    // would defeat its whole purpose, since every lookup would always miss.
    let git_cache = GitCache::new(
        watcher::git::DEFAULT_TTL,
        watcher::git::DEFAULT_COMMAND_TIMEOUT,
    );
    // Built once and reused for the life of the process, for the same
    // "don't defeat the point of caching/reuse" reason as `git_cache` above
    // - see `build_http_client`'s doc comment.
    let http_client = build_http_client();
    let mut sources = configured_sources();

    if args.once {
        run_once(&http_client, &server_url, &git_cache, &mut sources);
        return;
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    // Two handlers are registered per signal, in this specific order:
    //
    // 1. `register_conditional_shutdown` first. Its action only fires the
    //    process exit if `shutdown` is *already* true - on the first signal
    //    it does nothing, since `shutdown` starts `false`.
    // 2. `register` second. Its action unconditionally stores `true` into
    //    `shutdown` - which is what "arms" the first handler for next time.
    //
    // Registering them in the other order would arm-then-fire within the
    // same, first signal delivery, defeating the graceful shutdown path
    // entirely; this order and its rationale mirrors signal-hook's own
    // top-level "double Ctrl+C" documentation example exactly. The result:
    // the first SIGTERM/SIGINT sets `shutdown`, which `run_daemon` observes
    // between cycles (and now also between a sweep and its publish - see
    // `run_cycle`) and exits cleanly; a *second* signal - needed only if the
    // process is wedged somewhere neither of those checkpoints can reach in
    // a reasonable time - exits immediately from inside the handler itself,
    // rather than doing nothing as it did before this fix (finding 8,
    // PRO-210 review), which left a wedged watcher stoppable only by
    // SIGKILL. The exit status (128 + signal number) follows the
    // conventional shell/POSIX encoding for "terminated by signal N".
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        if let Err(e) =
            signal_hook::flag::register_conditional_shutdown(sig, 128 + sig, Arc::clone(&shutdown))
        {
            tracing::error!(
                signal = sig,
                error = %e,
                "failed to install conditional-shutdown (second-signal force-exit) handler"
            );
        }
        // `signal_hook::flag::register` installs an async-signal-safe handler
        // that only ever stores `true` into the flag - nothing unsafe or
        // allocation-heavy runs on the signal-handling thread itself, which is
        // what actually makes this sound to call from a signal handler. Matches
        // the `signal-hook` dependency `crates/reporter/src/bin/csm-codex.rs`
        // already uses for its own SIGINT/SIGTERM/SIGHUP forwarding, rather than
        // hand-rolling a second `libc::signal` setup in this crate.
        if let Err(e) = signal_hook::flag::register(sig, Arc::clone(&shutdown)) {
            // Not being able to install a signal handler is not fatal to a
            // single sweep succeeding, but it does mean this process can
            // only be stopped by SIGKILL from here on - worth a loud log,
            // not a silent degrade.
            tracing::error!(signal = sig, error = %e, "failed to install signal handler");
        }
    }

    tracing::info!(
        interval = %humantime::format_duration(args.interval),
        server_url,
        "starting watcher daemon"
    );
    run_daemon(
        &http_client,
        &server_url,
        &git_cache,
        &mut sources,
        args.interval,
        &shutdown,
    );
    tracing::info!("watcher daemon stopped");
}

/// Build the one `reqwest::blocking::Client` the daemon loop reuses for
/// every publish, for two independent reasons found in the PRO-210 review:
///
/// 1. **Keep-alive (finding 5).** A fresh `Client` spins up its own
///    background runtime thread and its own TCP connection; building one
///    per cycle - the previous shape, inside `publish::publish` itself -
///    defeats keep-alive entirely at a poll interval that can be as tight
///    as a couple of seconds.
/// 2. **A bounded request timeout (finding 4).** `reqwest::blocking::
///    Client::new()` carries no timeout of its own, so it inherits
///    reqwest's 30-second default. Against a server that accepts the TCP
///    connection but never replies, that left `publish` - and therefore the
///    whole cycle, since `shutdown` used to be checked only between cycles
///    - blocked for up to 30 seconds. Measured directly: SIGTERM took 28.3
///    seconds to actually stop the process against exactly that kind of
///    server, comfortably past launchd's default 20-second `ExitTimeOut`,
///    meaning the watcher would be SIGKILLed rather than exiting on its
///    own. [`PUBLISH_TIMEOUT`] keeps a real request (this crate's own small
///    JSON payload, to a same-host or same-LAN server) comfortably inside
///    the bound while still being short enough that one hung server cannot
///    single-handedly blow the shutdown budget. Combined with `run_cycle`
///    now also checking `shutdown` between the sweep and the publish (not
///    only between cycles), a signal received during discovery or sweep
///    skips the publish leg - and its timeout - entirely.
fn build_http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(PUBLISH_TIMEOUT)
        .build()
        .expect("failed to build the watcher's HTTP client")
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
/// `foreign_warnings` is threaded through to whichever of `discover`/
/// `discover_panes` actually runs (PRO-211 second-round review finding 2) -
/// the caller (`run_cycle`) owns one instance for the life of the process, so
/// a foreign-uid Claude process's unreadable environment warns once while it
/// stays in that state, not on every cycle. It is passed as an ordinary
/// parameter, not captured by either closure, so both closures can still be
/// plain `FnOnce` values without a double-mutable-borrow conflict between
/// them.
///
/// Returns the full `Discovery`, not just `registry_dirs`: PRO-209 needs
/// `tmux_panes` too, to resolve each session's activation target, and
/// PRO-211's caller (`run_cycle`) needs `live_pids` regardless of whether it
/// ends up feeding them to the orphaned-live-process check. On the
/// explicit-override path, `registry_dirs` comes from `explicit` and
/// discovery's own directory search is never run - but `tmux_panes` and
/// `live_pids` still come from `discover_panes`, a *second*, independently
/// injected closure that performs the same process read purely for pane and
/// live-pid capture. This fixes finding 3 from the PRO-209 review:
/// `CSM_WATCHER_REGISTRY_DIRS` is documented (PRO-204) as a permanent,
/// supported escape hatch, not scaffolding, so a session published while it
/// is set must still resolve a `tmux_target` like any other - losing it
/// silently was a real, user-visible downgrade, not a correct degrade.
/// `live_pids` returned here is deliberately *not* fed to the
/// orphaned-process check while the override is set (`run_cycle` decides
/// this, not this function) - see `sweep::sweep`'s doc comment for why
/// (PRO-211 review finding 3). `discover_panes` must never fail this
/// function: pane/live-pid capture is enrichment, not truth about which
/// sessions exist, so a failure there degrades to an empty snapshot (see
/// `discovery::discover_process_snapshot`'s doc comment), exactly like
/// `tmux`'s own degrade when `tmux` itself is unavailable.
fn resolve_registry_dirs(
    explicit: Vec<PathBuf>,
    discover: impl FnOnce(&mut ForeignUidWarnings) -> Result<Discovery, DiscoveryError>,
    discover_panes: impl FnOnce(&mut ForeignUidWarnings) -> ProcessSnapshot,
    foreign_warnings: &mut ForeignUidWarnings,
) -> Result<Discovery, DiscoveryError> {
    if !explicit.is_empty() {
        let snapshot = discover_panes(foreign_warnings);
        tracing::debug!(
            dir_count = explicit.len(),
            tmux_pane_count = snapshot.tmux_panes.len(),
            live_pid_count = snapshot.live_pids.len(),
            env_var = sweep::REGISTRY_DIRS_ENV,
            "using explicit registry directories; directory discovery bypassed, pane/live-pid \
             capture still run"
        );
        return Ok(Discovery {
            registry_dirs: explicit,
            tmux_panes: snapshot.tmux_panes,
            live_pids: snapshot.live_pids,
        });
    }

    tracing::debug!(
        env_var = sweep::REGISTRY_DIRS_ENV,
        "no explicit registry directories configured; discovering from live Claude processes"
    );
    let found = discover(foreign_warnings)?;
    // `debug`, not `info` (finding 3, PRO-210 review): this runs once per
    // cycle, so at `info` it was a steady, unbounded stream of per-sweep
    // noise into the rotated log rather than a genuine event - see
    // `common::telemetry`'s `MAX_LOG_FILES` doc comment for the measured
    // growth rate this contributed to.
    tracing::debug!(
        dir_count = found.registry_dirs.len(),
        tmux_pane_count = found.tmux_panes.len(),
        live_pid_count = found.live_pids.len(),
        "discovery complete"
    );
    Ok(found)
}

/// One independently swept agent source.
///
/// The cycle only depends on this narrow interface: a source identifies its
/// agent kind and returns one complete snapshot for that kind. Source-specific
/// discovery, parsing, and warning state stay behind the interface, so adding
/// another source does not change the Claude implementation.
trait SessionSource {
    fn agent_kind(&self) -> AgentKind;

    fn sweep(
        &mut self,
        git_cache: &GitCache,
        once: bool,
    ) -> Result<Vec<SnapshotSession>, SourceSweepFailure>;
}

/// State shared by the generic cycle but isolated for one agent kind.
///
/// In particular, each source owns a separate `Debounce`. Session IDs are not
/// globally unique across agent kinds, so sharing one would allow one source's
/// empty sweep to age or remove another source's sessions.
struct SourceState {
    source: Box<dyn SessionSource>,
    debounce: Debounce,
}

impl SourceState {
    fn new(source: impl SessionSource + 'static) -> Self {
        Self {
            source: Box::new(source),
            debounce: Debounce::new(),
        }
    }
}

enum SourceSweepFailure {
    Discovery,
    Sweep,
}

/// Claude Code's registry-backed session source.
///
/// All cross-sweep caches that contain Claude process/session identifiers live
/// here. A future source gets a separate source value and therefore cannot
/// collide with these caches even if its IDs happen to be identical.
struct ClaudeSource {
    process_cache: ProcessCache,
    orphan_warnings: OrphanWarnings,
    foreign_uid_warnings: ForeignUidWarnings,
}

impl ClaudeSource {
    fn new() -> Self {
        Self {
            process_cache: ProcessCache::new(watcher::discovery::DEFAULT_TTL),
            orphan_warnings: OrphanWarnings::new(),
            foreign_uid_warnings: ForeignUidWarnings::new(),
        }
    }
}

impl SessionSource for ClaudeSource {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn sweep(
        &mut self,
        git_cache: &GitCache,
        once: bool,
    ) -> Result<Vec<SnapshotSession>, SourceSweepFailure> {
        let explicit = sweep::registry_dirs_from_env();
        let registry_dirs_overridden = !explicit.is_empty();
        let discovery = match resolve_registry_dirs(
            explicit,
            |warnings| discovery::discover(&self.process_cache, warnings),
            |warnings| discovery::discover_process_snapshot(&self.process_cache, warnings),
            &mut self.foreign_uid_warnings,
        ) {
            Ok(discovery) => discovery,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to discover registry directories; refusing to publish an empty snapshot"
                );
                if once {
                    eprintln!(
                        "failed to discover Claude Code registry directories ({e}); refusing to \
                         publish an empty snapshot. Set {} to override discovery explicitly.",
                        sweep::REGISTRY_DIRS_ENV
                    );
                }
                return Err(SourceSweepFailure::Discovery);
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
            // `sweep::sweep` and `publish::publish`, which will POST an empty
            // snapshot and end every previously-published Claude session on
            // this host.
            tracing::warn!(
                "no registry directories to sweep; publishing an empty snapshot, which will end \
                 every previously-published Claude session on this host"
            );
            if once {
                eprintln!(
                    "no registry directories to sweep; publishing an empty snapshot, which will \
                     end every previously-published Claude session on this host"
                );
            }
        }

        let sessions = match sweep::sweep(
            &dirs,
            &discovery.tmux_panes,
            git_cache,
            &discovery.live_pids,
            if registry_dirs_overridden {
                None
            } else {
                Some(&mut self.orphan_warnings)
            },
        ) {
            Ok(sessions) => sessions,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "sweep failed to read a registry directory; refusing to publish an empty snapshot"
                );
                if once {
                    eprintln!(
                        "sweep failed to read a registry directory ({e}); refusing to publish an \
                         empty snapshot."
                    );
                }
                return Err(SourceSweepFailure::Sweep);
            }
        };
        tracing::debug!(session_count = sessions.len(), "sweep complete");
        Ok(sessions)
    }
}

/// Codex CLI's writer-lock-backed session source.
struct CodexSource;

impl SessionSource for CodexSource {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn sweep(
        &mut self,
        git_cache: &GitCache,
        _once: bool,
    ) -> Result<Vec<SnapshotSession>, SourceSweepFailure> {
        Ok(watcher::codex::sweep(git_cache))
    }
}

fn configured_sources() -> Vec<SourceState> {
    vec![
        SourceState::new(ClaudeSource::new()),
        SourceState::new(CodexSource),
    ]
}

/// The result of one discover-sweep-publish cycle, shared by `--once` and
/// the daemon loop.
///
/// `--once` (`run_once`) turns any failure variant into a non-zero exit,
/// preserving its exact pre-PRO-210 behaviour. The daemon loop (`run_daemon`)
/// instead uses this to decide the pace of the *next* cycle - backing off on
/// either kind of failure and resetting to the base interval on success -
/// without ever calling `publish` on a `DiscoveryFailed` cycle, which is what
/// keeps "a failed sweep publishes nothing" true under continuous polling
/// too, not just for a single `--once` invocation.
///
/// `PublishFailed` carries the underlying `PublishError` rather than just
/// signalling failure, so a caller can decide *how* to report it - e.g.
/// `run_daemon` capturing only the first failure of a run to Sentry (finding
/// 6, PRO-210 review) needs the error to hand to `common::sentry::
/// capture_error` at the point it decides to report it, not at the point it
/// occurred.
///
/// `ShutdownRequested` is distinct from either failure: it means `shutdown`
/// was observed between the sweep and the publish (see `run_cycle`'s doc
/// comment) and the publish leg was skipped entirely, on purpose, rather
/// than failing. `run_daemon` must not back off on this outcome - it means
/// the process is exiting, not that anything went wrong.
///
/// `SweepFailed` (PRO-211) means discovery itself succeeded, but `sweep`
/// could not read a discovered registry directory or an individual registry
/// file (`sweep::SweepError`) - a registry that exists but could not be
/// listed, or a file that exists but could not be read, not merely a
/// directory that does not exist yet (see `registry::ReadError`'s and
/// `sweep::SweepError`'s doc comments for that distinction). This is handled
/// identically to `DiscoveryFailed` everywhere: `run_once` exits non-zero,
/// `run_daemon` backs off, and - critically - `Debounce::apply` is never
/// called for the failing source, so its failed sweep never advances (or
/// resets) its debounce. Other sources are still driven independently.
enum CycleOutcome {
    Published,
    DiscoveryFailed,
    SweepFailed,
    PublishFailed(publish::PublishError),
    ShutdownRequested,
}

/// Run one sweep-publish cycle for every configured source.
///
/// Each source independently yields the complete session set for its own
/// agent kind. The generic path then applies that source's debounce and
/// publishes a kind-scoped snapshot. A source failure leaves its state
/// untouched and does not prevent the remaining sources from running.
///
/// `shutdown` is checked once more here, between `sweep` and `publish` (in
/// addition to `run_daemon`'s own between-cycle checks), so that a signal
/// arriving during a slow discovery or sweep does not still go on to spend
/// up to `PUBLISH_TIMEOUT` blocked in a publish the process is about to exit
/// right after anyway (finding 4, PRO-210 review) - see `build_http_client`'s
/// doc comment for the measured 28.3s SIGTERM latency this closes.
///
/// `once` controls whether a failure is also echoed to stderr with
/// `eprintln!`, in addition to being logged (finding 7, PRO-210 review): a
/// single `--once` invocation's failure is exactly the kind of thing a
/// human or a calling script watches stderr for, but under continuous
/// polling the same `eprintln!` on every failing cycle writes forever to an
/// unrotated stream under launchd/systemd - the log file, via `tracing`, is
/// already rotated and is where that detail belongs instead.
fn run_cycle(
    client: &reqwest::blocking::Client,
    server_url: &str,
    git_cache: &GitCache,
    sources: &mut [SourceState],
    shutdown: &AtomicBool,
    once: bool,
) -> CycleOutcome {
    let mut cycle_outcome = CycleOutcome::Published;

    for state in sources {
        let agent_kind = state.source.agent_kind();
        let sessions = match state.source.sweep(git_cache, once) {
            Ok(sessions) => sessions,
            Err(SourceSweepFailure::Discovery) => {
                if matches!(cycle_outcome, CycleOutcome::Published) {
                    cycle_outcome = CycleOutcome::DiscoveryFailed;
                }
                continue;
            }
            Err(SourceSweepFailure::Sweep) => {
                if matches!(cycle_outcome, CycleOutcome::Published) {
                    cycle_outcome = CycleOutcome::SweepFailed;
                }
                continue;
            }
        };

        // Applied only after this source's successful sweep. A failed source
        // therefore leaves its own debounce unchanged, while other sources
        // can still complete independently during the same cycle.
        let sessions = state.debounce.apply(sessions);

        if shutdown.load(Ordering::Relaxed) {
            tracing::debug!(
                ?agent_kind,
                "shutdown observed after sweep; skipping remaining publishes for this cycle"
            );
            return CycleOutcome::ShutdownRequested;
        }

        if let Err(e) = publish::publish(client, server_url, agent_kind, sessions) {
            tracing::error!(?agent_kind, error = %e, "failed to publish snapshot");
            if once {
                eprintln!("failed to publish snapshot: {e}");
            }
            if !matches!(cycle_outcome, CycleOutcome::PublishFailed(_)) {
                cycle_outcome = CycleOutcome::PublishFailed(e);
            }
        }
    }

    cycle_outcome
}

fn run_once(
    client: &reqwest::blocking::Client,
    server_url: &str,
    git_cache: &GitCache,
    sources: &mut [SourceState],
) {
    let shutdown = AtomicBool::new(false);
    match run_cycle(client, server_url, git_cache, sources, &shutdown, true) {
        CycleOutcome::Published => {}
        CycleOutcome::DiscoveryFailed => std::process::exit(1),
        CycleOutcome::SweepFailed => std::process::exit(1),
        CycleOutcome::PublishFailed(e) => {
            // A single `--once` invocation is exactly one cycle, so
            // reporting this failure to Sentry is trivially "the first (and
            // only) failure of a run" - finding 6's "only the first failure
            // of a run" cap exists to stop the daemon loop's *continuous*
            // polling from flooding Sentry, which cannot happen here.
            common::sentry::capture_error(&e);
            std::process::exit(1);
        }
        // `--once` never sets `shutdown` itself, so this is unreachable in
        // practice; treated as success rather than added as a `--once`
        // failure mode it was never meant to be.
        CycleOutcome::ShutdownRequested => {}
    }
}

/// Upper bound on the backoff a run of consecutive failed cycles can reach,
/// whether the failures are discovery (a broken local `ps`/`/proc` read) or
/// publish (an unreachable or erroring server). This is the trade behind
/// "recovers on its own once the server returns" (PRO-204's user story 26):
/// a longer cap makes a persistently-down dependency cheaper to poll but
/// slower to notice coming back; a shorter one notices faster at the cost of
/// polling a dependency that is still down more often. 30 seconds keeps a
/// downed server or broken enumerator from being hammered at the poll
/// interval indefinitely, while still bounding "how stale can the watcher's
/// recovery be" to a duration well short of anything a user would describe
/// as needing to restart the watcher over.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How often the wait between cycles wakes up to check the shutdown flag,
/// regardless of how long that wait actually is. This decouples shutdown
/// latency from the poll interval (or a backed-off wait, up to
/// [`MAX_BACKOFF`]) - a daemon currently backed off to 30 seconds must still
/// react to SIGINT/SIGTERM within a fraction of a second, not up to 30
/// seconds later.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Backoff for consecutive failed cycles, in either `run_daemon`'s sense of
/// "failed" (see [`CycleOutcome`]). Doubles on each consecutive failure,
/// capped at [`MAX_BACKOFF`]; a success resets it back to `base` (the
/// configured `--interval`) so a transient blip does not leave the watcher
/// polling more slowly than requested after it recovers.
///
/// `base` (and therefore every `current`) is floored at [`MIN_INTERVAL`] as
/// defense-in-depth, not the primary fix for finding 2 - `parse_interval`
/// already rejects a too-small `--interval` at parse time, so a `base` below
/// `MIN_INTERVAL` should never actually reach this constructor via the CLI.
/// It is floored here too anyway, since a `Backoff` that starts at zero
/// doubles to zero forever (`0.saturating_mul(2) == 0`), which is precisely
/// the busy-loop this struct exists to prevent; a second, cheap guard at the
/// type's own boundary is worth having independently of whichever call site
/// happens to validate its input today.
struct Backoff {
    base: Duration,
    current: Duration,
}

impl Backoff {
    fn new(base: Duration) -> Self {
        let base = base.max(MIN_INTERVAL);
        Self {
            base,
            current: base.min(MAX_BACKOFF),
        }
    }

    /// Record a failure and return how long to wait before the next cycle.
    fn fail(&mut self) -> Duration {
        let wait = self.current;
        self.current = self
            .current
            .max(MIN_INTERVAL)
            .saturating_mul(2)
            .min(MAX_BACKOFF);
        wait
    }

    /// Record a success: the next failure (if any) starts backing off from
    /// `base` again, not from wherever a prior run of failures left off.
    fn reset(&mut self) {
        self.current = self.base.min(MAX_BACKOFF);
    }
}

/// Sleep for `duration`, but wake every [`SHUTDOWN_POLL_INTERVAL`] to check
/// `shutdown`, returning early the moment it is set rather than sleeping the
/// wait out in full. This is what keeps SIGINT/SIGTERM responsive even
/// during a long interval or a backed-off wait after repeated failures.
fn sleep_interruptible(duration: Duration, shutdown: &AtomicBool) {
    let deadline = Instant::now() + duration;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::sleep(remaining.min(SHUTDOWN_POLL_INTERVAL));
    }
}

/// Run discover-sweep-publish cycles continuously until `shutdown` is
/// observed, starting the first cycle immediately rather than after waiting
/// out one `interval` first - so restarting the watcher reconciles stale
/// state at once (PRO-204's user story 27), rather than leaving it stale for
/// up to one more interval.
///
/// **Why a cycle can never overlap or pile up.** This is a single
/// synchronous loop: `run_cycle` runs to completion (discovery, sweep, and -
/// only if discovery succeeded and `shutdown` was not observed in between -
/// publish) before this function ever looks at the clock to decide how long
/// to wait, and the wait itself happens only after that. So `interval` (or a
/// backed-off wait, once a failure has occurred) means "wait this long after
/// a cycle finishes", never "start a cycle every `interval` on a fixed
/// schedule". A cycle slower than `interval` simply pushes the next cycle's
/// start back by the overrun rather than causing a second cycle to start
/// concurrently with the first, or several queuing up behind it. There is no
/// separate scheduler or timer thread that could do that; the only thing
/// driving the next cycle is this loop reaching the top again.
///
/// **Worst-case time from signal to exit.** This is *not* bounded by a
/// single cycle's discovery/sweep work the way an earlier version of this
/// comment claimed (measured at 3.8s for three sessions with `git` and
/// `tmux` both hung - that figure was correct for discovery/sweep alone, but
/// wrong as a claim about the *whole* cycle, because it ignored `publish`).
/// Before this fix, `publish` used a bare `reqwest::blocking::Client::new()`
/// with no request timeout, inheriting reqwest's 30-second default, and
/// `shutdown` was checked only between cycles - never between a cycle's
/// sweep and its publish - so a signal arriving just as a slow cycle reached
/// `publish` against a server that accepts the connection but never replies
/// left the process blocked for however long that request took. Measured
/// directly: SIGTERM took 28.3 seconds to actually stop the process against
/// exactly that kind of server. Two changes close this: `run_cycle` now also
/// checks `shutdown` between `sweep` and `publish`, skipping the publish leg
/// (and its timeout) entirely once a signal has been observed; and the
/// shared client from `build_http_client` carries an explicit
/// [`PUBLISH_TIMEOUT`] (5s) instead of reqwest's 30-second default. The
/// worst case is therefore now bounded by whichever is larger: one
/// [`SHUTDOWN_POLL_INTERVAL`] (a signal observed during the wait between
/// cycles), or discovery/sweep's own timeouts plus, if a publish was already
/// underway when the signal arrived, up to `PUBLISH_TIMEOUT` for that
/// in-flight request to give up - not the previous 30s worst case.
///
/// **Why a signal never corrupts a cycle in flight.** `shutdown` is consulted
/// at two points around a cycle - once right before it starts, and once
/// between `sweep` and `publish`, inside `run_cycle` itself - plus
/// repeatedly (every [`SHUTDOWN_POLL_INTERVAL`]) during the wait after a
/// cycle finishes, via [`sleep_interruptible`]. None of these ever abort
/// `sweep` partway through: `publish` is only ever reached after `sweep` has
/// already returned its full result, so there is no "partial sweep" data to
/// accidentally publish regardless of when a signal arrives, and a signal
/// observed before `sweep` has finished always lets it finish rather than
/// tearing it down mid-flight.
fn run_daemon(
    client: &reqwest::blocking::Client,
    server_url: &str,
    git_cache: &GitCache,
    sources: &mut [SourceState],
    interval: Duration,
    shutdown: &AtomicBool,
) {
    let mut backoff = Backoff::new(interval);
    // Tracks whether a publish failure has already been reported to Sentry
    // during the *current* unbroken run of failures, reset on the next
    // success (finding 6, PRO-210 review). Without this, a server that is
    // down for the length of a workday at the default 2s interval produces
    // on the order of tens of thousands of `capture_error` calls - one per
    // failed cycle - for what is, from an alerting standpoint, exactly one
    // incident, not that many.
    let mut sentry_reported_this_run = false;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let wait = match run_cycle(client, server_url, git_cache, sources, shutdown, false) {
            CycleOutcome::Published => {
                backoff.reset();
                sentry_reported_this_run = false;
                interval
            }
            CycleOutcome::DiscoveryFailed => backoff.fail(),
            CycleOutcome::SweepFailed => backoff.fail(),
            CycleOutcome::PublishFailed(e) => {
                if !sentry_reported_this_run {
                    common::sentry::capture_error(&e);
                    sentry_reported_this_run = true;
                }
                backoff.fail()
            }
            // A signal was observed mid-cycle; the outer `shutdown` check at
            // the top of the next iteration is what actually ends the loop,
            // so this arm only needs to avoid treating "exiting on purpose"
            // as a failure worth backing off from.
            CycleOutcome::ShutdownRequested => break,
        };
        sleep_interruptible(wait, shutdown);
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
    fn omitting_once_defaults_to_daemon_mode_with_the_default_interval() {
        // Unlike before PRO-210, omitting `--once` is no longer a clap
        // error: it is how the daemon (continuous-polling) mode is
        // selected, so it must parse successfully with `once` false and
        // `interval` defaulted.
        let args = Args::parse_from(["csm-watcher"]);
        assert!(!args.once);
        assert_eq!(args.interval, parse_interval(DEFAULT_INTERVAL).unwrap());
    }

    #[test]
    fn parse_custom_interval_as_a_humantime_duration() {
        // `--interval` takes a humantime string (finding 1, PRO-210
        // review), not raw milliseconds - matching PRO-204's stated CLI and
        // what PRO-212's launchd/systemd unit files bake in.
        let args = Args::parse_from(["csm-watcher", "--interval", "500ms"]);
        assert_eq!(args.interval, Duration::from_millis(500));

        let args = Args::parse_from(["csm-watcher", "--interval", "2s"]);
        assert_eq!(args.interval, Duration::from_secs(2));
    }

    #[test]
    fn interval_below_min_interval_is_rejected_at_parse_time() {
        // Finding 2, PRO-210 review: a zero (or merely tiny) interval turns
        // a down server into a genuine busy loop, since `Backoff`'s base -
        // and therefore every wait it ever produces - can never exceed it.
        // Measured directly against the pre-fix code with `--interval-ms
        // 0`: 48 failed publish attempts in 10 seconds at 22-27% CPU.
        let result = Args::try_parse_from(["csm-watcher", "--interval", "0ms"]);
        assert!(
            result.is_err(),
            "an interval of 0 must be rejected, not silently accepted as a busy loop"
        );

        let result = Args::try_parse_from(["csm-watcher", "--interval", "50ms"]);
        assert!(
            result.is_err(),
            "an interval below MIN_INTERVAL (100ms) must be rejected, got {result:?}"
        );

        let result = Args::try_parse_from(["csm-watcher", "--interval", "100ms"]);
        assert!(
            result.is_ok(),
            "MIN_INTERVAL itself must be accepted, not just values strictly above it"
        );
    }

    #[test]
    fn interval_rejects_an_unparseable_duration_string() {
        let result = Args::try_parse_from(["csm-watcher", "--interval", "not-a-duration"]);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_registry_dirs_uses_explicit_override_without_calling_directory_discovery() {
        let explicit = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        let tmux_panes = HashMap::from([(123, "%9".to_string())]);
        let live_pids = std::collections::HashSet::from([123, 456]);
        let snapshot = ProcessSnapshot {
            tmux_panes: tmux_panes.clone(),
            live_pids: live_pids.clone(),
        };
        let result = resolve_registry_dirs(
            explicit.clone(),
            |_fw| panic!("directory discovery must not be called when an explicit override is set"),
            |_fw| snapshot.clone(),
            &mut ForeignUidWarnings::new(),
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
        assert_eq!(
            discovery.live_pids, live_pids,
            "live_pids from the override's own process snapshot must be passed through too, or \
             the orphaned-live-process warning (PRO-211) has nothing to compare against while \
             the override is set"
        );
    }

    #[test]
    fn resolve_registry_dirs_falls_back_to_discovery_when_explicit_is_empty() {
        let discovered = vec![PathBuf::from("/home/alice/.claude")];
        let tmux_panes = HashMap::from([(123, "%9".to_string())]);
        let live_pids = std::collections::HashSet::from([123]);
        let result = resolve_registry_dirs(
            Vec::new(),
            |_fw| {
                Ok(Discovery {
                    registry_dirs: discovered.clone(),
                    tmux_panes: tmux_panes.clone(),
                    live_pids: live_pids.clone(),
                })
            },
            |_fw| panic!("pane capture must not run separately when discovery already ran"),
            &mut ForeignUidWarnings::new(),
        );
        let discovery = result.unwrap();
        assert_eq!(discovery.registry_dirs, discovered);
        assert_eq!(
            discovery.tmux_panes, tmux_panes,
            "tmux_panes from a successful discovery must be passed through, not dropped"
        );
        assert_eq!(
            discovery.live_pids, live_pids,
            "live_pids from a successful discovery must be passed through, not dropped"
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
            |_fw| Ok(Discovery::default()),
            |_fw| panic!("pane capture must not run separately when discovery already ran"),
            &mut ForeignUidWarnings::new(),
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
            |_fw| Err(DiscoveryError::Enumerate(std::io::Error::other("boom"))),
            |_fw| panic!("pane capture must not run separately when discovery already ran"),
            &mut ForeignUidWarnings::new(),
        );
        assert!(result.is_err());
    }

    // --- Backoff ---

    #[test]
    fn backoff_doubles_on_each_consecutive_failure_and_caps_at_max_backoff() {
        let mut backoff = Backoff::new(Duration::from_millis(100));
        assert_eq!(backoff.fail(), Duration::from_millis(100));
        assert_eq!(backoff.fail(), Duration::from_millis(200));
        assert_eq!(backoff.fail(), Duration::from_millis(400));
        // Keep doubling well past MAX_BACKOFF to prove it actually caps
        // rather than merely growing slowly.
        for _ in 0..10 {
            backoff.fail();
        }
        assert_eq!(backoff.fail(), MAX_BACKOFF);
    }

    #[test]
    fn backoff_reset_returns_to_the_base_interval_after_failures() {
        let mut backoff = Backoff::new(Duration::from_millis(100));
        backoff.fail();
        backoff.fail();
        backoff.reset();
        assert_eq!(
            backoff.fail(),
            Duration::from_millis(100),
            "a reset must forget prior failures, not resume doubling from where they left off"
        );
    }

    #[test]
    fn backoff_new_caps_a_base_interval_already_above_max_backoff() {
        // An operator-configured `--interval` larger than MAX_BACKOFF must
        // not make the *first* failure wait even longer than the cap.
        let mut backoff = Backoff::new(MAX_BACKOFF + Duration::from_secs(60));
        assert_eq!(backoff.fail(), MAX_BACKOFF);
    }

    #[test]
    fn backoff_floors_a_zero_base_at_min_interval_instead_of_looping_at_zero() {
        // Defense-in-depth for finding 2 (PRO-210 review): `parse_interval`
        // already rejects a too-small `--interval` before it ever reaches
        // here, but `Backoff` floors its own `base` anyway, since a `base`
        // of zero would otherwise double to zero forever
        // (`0.saturating_mul(2) == 0`) - a busy loop this struct exists to
        // prevent, independent of whichever call site validates its input.
        let mut backoff = Backoff::new(Duration::ZERO);
        assert_eq!(backoff.fail(), MIN_INTERVAL);
        assert_eq!(backoff.fail(), MIN_INTERVAL * 2);
    }

    // --- sleep_interruptible ---

    #[test]
    fn sleep_interruptible_returns_promptly_when_shutdown_is_already_set() {
        let shutdown = AtomicBool::new(true);
        let start = Instant::now();
        sleep_interruptible(Duration::from_secs(30), &shutdown);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "must not wait out anywhere near the full duration once shutdown is set, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn sleep_interruptible_returns_promptly_once_shutdown_is_set_from_another_thread() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let start = Instant::now();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::Relaxed);
        });
        sleep_interruptible(Duration::from_secs(30), &shutdown);
        setter.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "must wake within about one SHUTDOWN_POLL_INTERVAL of the flag being set, not wait \
             out the full duration, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn sleep_interruptible_waits_out_the_full_duration_when_never_interrupted() {
        let shutdown = AtomicBool::new(false);
        let duration = Duration::from_millis(150);
        let start = Instant::now();
        sleep_interruptible(duration, &shutdown);
        assert!(
            start.elapsed() >= duration,
            "must not return early when shutdown is never set, took {:?}",
            start.elapsed()
        );
    }
}
