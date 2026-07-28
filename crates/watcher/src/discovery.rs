//! Automatic discovery of Claude Code session registry directories from
//! live process environments (PRO-208).
//!
//! Enumerate every process whose invoked executable looks like a Claude
//! binary, read each one's environment, and take the union of their
//! `CLAUDE_CONFIG_DIR` values (defaulting to `~/.claude` per process where
//! unset) to get the set of registry directories to sweep. A profile
//! created after the watcher started is picked up automatically, because
//! this is a plain function called fresh on every sweep - there is no
//! cached process list to go stale.
//!
//! The same read also captures each process's `TMUX_PANE`, keyed by pid:
//! the registry's own `tmux` field was observed `null` even for sessions
//! demonstrably running inside tmux (see
//! `docs/research/2026-07-24-claude-session-tracking.md`), so the process
//! environment is the only reliable source, and one read serves both
//! purposes. This crate does not resolve a pane id into an activation
//! target - that is PRO-209 - but the pane ids are captured here, alongside
//! the registry directories, from the exact same enumeration, so PRO-209
//! only has to consume [`Discovery::tmux_panes`] rather than read the
//! environment a second time.
//!
//! The same enumeration also captures every live Claude process's pid, kept
//! as [`Discovery::live_pids`] (PRO-211): a "fail loudly" check independent
//! of directory discovery entirely, comparing this set against the pids
//! actually found in the registry to warn on a live process the registry
//! does not know about - see `sweep::sweep`'s use of it.
//!
//! [`sweep::registry_dirs_from_env`](crate::sweep::registry_dirs_from_env)
//! remains a real escape hatch: whenever it yields at least one directory,
//! the caller must use that list directly and never call [`discover`] at
//! all. Discovery itself is only invoked when that override is absent.
//! The override bypasses *directory* discovery only, not pane capture or
//! live-pid capture: [`discover_process_snapshot`] runs the same process
//! enumeration purely for `TMUX_PANE`s and live pids, so a session found via
//! the override still resolves a `tmux_target` rather than silently losing
//! it. `live_pids` is still captured here too, but (PRO-211 review finding
//! 3) `main.rs` deliberately does not feed it to the orphaned-live-process
//! check while the override is set: under the override `live_pids` covers
//! every Claude process on the *host*, while the entries the sweep can
//! possibly see are scoped to only the override's own directories, so every
//! real session outside those directories would compare as an "orphan" -
//! reproduced directly as a false positive on every live process on the
//! machine, every sweep, not a real gap the check exists to surface.
//!
//! The impure part - actually enumerating OS processes - is confined to
//! the per-platform `imp::enumerate_claude_processes` (macOS: `ps -Eww`;
//! Linux: `/proc/<pid>/{cmdline,environ}`). Everything else in this file is
//! a pure function of already-read bytes and is tested against fixture
//! data, including - per PRO-204's testing decisions - the Linux
//! `/proc/<pid>/environ` parser, even though the impure Linux enumeration
//! itself cannot be exercised from macOS.
//!
//! On Linux, `imp::PROC_ROOT_ENV` (`CSM_WATCHER_PROC_ROOT`) is a second,
//! narrower permanent escape hatch alongside `sweep::REGISTRY_DIRS_ENV`
//! (PRO-216): it replaces `/proc` as the root the Linux enumeration reads
//! pid directories, `cmdline`, and `environ` from, so a test can point it at
//! a fake `/proc`-shaped tree and exercise the real enumeration end to end,
//! the Linux equivalent of the stub-`ps`-on-`PATH` interception the macOS
//! integration tests use. Unlike `REGISTRY_DIRS_ENV`, this is not something
//! a real deployment is expected to ever set - it exists for test
//! substitution, not as an end-user-facing configuration knob.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The environment variable Claude Code reads to relocate its config
/// directory (and therefore its session registry, at
/// `<dir>/sessions/<pid>.json`).
const CLAUDE_CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";

/// The environment variable tmux sets in every process running inside a
/// pane, identifying that pane (e.g. `%38`).
const TMUX_PANE_VAR: &str = "TMUX_PANE";

/// The environment variable a process's own default config directory is
/// derived from (`<HOME>/.claude`), read from that *process's* environment -
/// see [`default_config_dir_for`] (PRO-211 second-round review finding 3).
const HOME_VAR: &str = "HOME";

/// What automatic discovery found: the deduplicated registry directories to
/// sweep, each live Claude process's tmux pane keyed by pid, and the full
/// set of live Claude process pids (PRO-211's orphaned-live-process check).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Discovery {
    pub registry_dirs: Vec<PathBuf>,
    pub tmux_panes: HashMap<i32, String>,
    pub live_pids: HashSet<i32>,
}

/// Process enumeration failed outright: the `ps` invocation errored, or
/// `/proc` itself could not be read.
///
/// This is deliberately narrow. It must **not** cover "enumeration
/// succeeded and found zero *Claude* processes" - that is a genuinely
/// empty, genuinely successful result (see [`discover`]). It also must not
/// cover a *non*-Claude process whose environment happens to be unreadable
/// (e.g. `/sbin/launchd`, owned by another user) - that process was never
/// going to contribute a registry directory, so its unreadable environment
/// is not a "cannot determine" case at all. Only a failure of enumeration
/// itself, or a confirmed Claude process's environment being unreadable
/// (see [`UnreadableEnvironment`](DiscoveryError::UnreadableEnvironment)
/// below), belongs here, because only those failures mean the true set of
/// registry directories (or live Claude processes) is unknown, and
/// publishing an empty or short snapshot in that case would be
/// indistinguishable from "no sessions exist".
///
/// [`EmptyProcessList`](DiscoveryError::EmptyProcessList) is the other side
/// of that same line: zero processes *total* (before any Claude filter is
/// applied) is never a genuine observation on a live host - there is always
/// at least the OS's own init process and the enumerator itself - so it can
/// only mean the enumerator itself is broken (a stubbed or shadowed `ps`
/// that exits 0 printing nothing, for instance). This was reproduced
/// directly: with such a `ps` on `PATH`, the pre-fix code treated an empty
/// *unfiltered* list exactly like a filtered list matching no Claude
/// processes, reported "no live Claude Code processes found", and
/// published (and thereby ended) every real session on the host. Both
/// enumerators must check the unfiltered count - before `is_claude_command`
/// narrows it - and return this variant when it is zero.
///
/// [`UnreadableEnvironment`](DiscoveryError::UnreadableEnvironment) (PRO-211
/// review finding 4) is for a process **already confirmed** to be a Claude
/// binary by its invoked command (`is_claude_command`) whose environment could
/// then not be determined at all, and whose *uid matches this watcher's
/// own* - see below for why uid matters. On Linux, `/proc/<pid>/environ`
/// failed to read; on macOS, `ps -Eww` reported zero environment tokens for
/// that line. Before this fix both cases were logged and skipped, silently
/// dropping that profile's `CLAUDE_CONFIG_DIR` from `union_discovery`'s
/// input and thereby ending its sessions with a successful exit - exactly
/// the class of bug this ticket exists to close structurally. This is
/// narrower than it might look: a process confirmed *not* to be Claude with
/// a genuinely empty environment is unaffected - see
/// `parse_ps_output_handles_a_process_with_no_environment_at_all`'s pid-1
/// (`/sbin/launchd`) case, which is filtered out by `is_claude_command` before
/// this check ever runs - and a *Claude* process with a present-but-narrow
/// environment (no `CLAUDE_CONFIG_DIR` or `TMUX_PANE` key, but other keys
/// still read) is also unaffected - see
/// `discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_and_tmux_pane_from_one_read`'s
/// pid-23195 case; both are real, successful observations, not this error.
///
/// **uid matters (PRO-211 second-round review finding 2).** A Claude-matched
/// line with zero environment tokens does not only mean "`ps` could not read
/// this" - `ps -Eww` prints the full command line for a process owned by
/// *another* user while suppressing its environment entirely, rather than
/// erroring, which is indistinguishable by content alone from a genuine read
/// failure. Verified directly on a real host: `1 /sbin/launchd` and `653
/// /usr/libexec/corebrightnessd --launchd` (both root-owned) both print a
/// full command with zero environment tokens under this watcher's own,
/// unprivileged uid. Before this fix, `build_claude_processes` treated *any*
/// Claude-matched empty-environment line as this error - so a legitimate,
/// unrelated `sudo claude` (or any Claude process on a shared/multi-user
/// host owned by someone else) turned into a **permanent** discovery
/// failure: every sweep, for as long as that foreign process lived, `ps`
/// reported it the same way, `discover()` returned `Err` every time, and the
/// watcher published nothing at all - loud rather than silent, the right
/// direction, but a total, indefinite outage from a legitimate
/// configuration, not a transient one. A foreign-uid Claude process with an
/// unreadable environment is now a warning and a skip (see
/// [`ForeignUidWarnings`]), not this error; only a *same-uid* Claude process
/// - one this watcher genuinely should have been able to read - still
/// becomes this error.
/// [`MalformedPsLine`](DiscoveryError::MalformedPsLine) (PRO-211 third-round
/// review finding 2) is for a `ps -Eww` output line that is not blank but
/// still cannot be parsed into the `pid uid command...` shape the invocation
/// asks for. `ps -o pid=,uid=,command=` is machine-formatted specifically so
/// this project does not have to guess at column widths or headers, so a
/// line that violates that shape - a pid or uid column containing something
/// non-numeric, or a missing uid column entirely - means something is
/// actually wrong (a `ps` version mismatch, an unexpected locale, a
/// genuinely corrupt invocation), not an exotic-but-valid process this
/// project simply doesn't recognise. Before this fix, `parse_ps_output`
/// silently dropped any line it could not parse via `filter_map`; reachable
/// today only because the `uid=` column (added for PRO-211 second-round
/// review finding 2) widened the required-column count from one to two, so
/// a line whose *uid* column alone is malformed can now fail to parse while
/// its pid still looks valid - and a silently-incomplete process list is
/// exactly the shape PRO-211 exists to eliminate: reproduced directly with
/// two live Claude profiles, malforming only one profile's uid column
/// published the other profile alone, with a successful exit, ending every
/// session the malformed profile had. Two shapes are deliberately *not*
/// this error, and stay silently skipped, because they are real, benign
/// output rather than a violation of the expected shape - see
/// `parse_ps_line`'s doc comment.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("failed to enumerate processes: {0}")]
    Enumerate(#[source] std::io::Error),
    #[error(
        "process enumeration returned zero processes total, which is impossible on a live \
         host; treating this as a broken enumerator rather than a genuine observation"
    )]
    EmptyProcessList,
    #[error(
        "process {pid} matched as a Claude Code process but its environment could not be \
         determined: {source}"
    )]
    UnreadableEnvironment {
        pid: i32,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse a line of `ps -Eww -ax -o pid=,uid=,command=` output: {line:?}")]
    MalformedPsLine { line: String },
}

/// One live process whose invoked executable looked like a Claude binary,
/// with the three environment values this project reads from it.
///
/// `home` (PRO-211 second-round review finding 3) is this *process's own*
/// `HOME`, captured from the same environment read as `config_dir` and
/// `tmux_pane` - see [`default_config_dir_for`] for why a process's own
/// `HOME` must be preferred over the watcher's when resolving its default
/// config directory.
#[derive(Debug, Clone, PartialEq)]
struct ClaudeProcess {
    pid: i32,
    config_dir: Option<String>,
    tmux_pane: Option<String>,
    home: Option<String>,
}

/// Default time-to-live for the cached process enumeration (PRO-217).
///
/// The enumeration itself - `ps -Eww -ax -o pid=,uid=,command=` on macOS, a
/// full `/proc` walk on Linux - was measured (PRO-217) at 0.11s wall / 0.10s
/// CPU dumping 3.1MB on an 883-process host, roughly 60% of one sweep's
/// total CPU; run unconditionally on every sweep at the default 2s interval,
/// that is 7.7% of one core sustained, at rest, for information that changes
/// on the scale of minutes: a new `CLAUDE_CONFIG_DIR` profile appearing, or a
/// session's `TMUX_PANE` moving because it was dragged to a different window.
/// Session *status* - the thing that genuinely needs two-second freshness -
/// never comes from this enumeration at all: it is read fresh from the
/// registry JSON every sweep (`sweep::sweep` -> `registry::read_entries`),
/// and `registry::is_live` checks each session's own pid directly against the
/// OS (`common::process::is_alive`/`start_time`), independent of anything
/// this module caches. So caching the enumeration trades a bounded,
/// documented delay in noticing a new profile, or the pane of a process that
/// started since the last refresh, against eliminating most of that CPU - the
/// same trade `git::GitCache` already makes for git lookups, at a similar
/// order of TTL.
///
/// A pane that *moves* is not a staleness hazard: `TMUX_PANE` is fixed in a
/// process's environment when it spawns, and tmux pane ids are stable and not
/// reused, so a cached pid -> pane mapping stays correct however the user
/// drags windows around. Where the mapping does lag it degrades to no target
/// at all, since a fresh `list-panes` will not contain an id that has gone,
/// rather than to a jump into the wrong pane.
///
/// 15 seconds (roughly 7-8 sweeps at the default 2s interval) was chosen
/// over `git::DEFAULT_TTL`'s 30 seconds because the ticket asked for
/// "roughly 10 to 15 seconds" specifically, narrower than git's "a person
/// runs `git checkout` a handful of times an hour" - a new profile appearing
/// is rarer still, but this cache's failure mode if ever wrong (a stale
/// directory set) is more consequential than a stale branch label, so it
/// stays at the tighter end of "clearly still cheap" rather than reaching for
/// git's own ceiling.
pub const DEFAULT_TTL: Duration = Duration::from_secs(15);

/// A cache of one full process-enumeration result (every [`ClaudeProcess`]
/// [`discover`] and [`discover_process_snapshot`] would otherwise re-derive
/// from a fresh `ps`/`/proc` read on every single sweep), with a
/// time-to-live, in the same spirit as [`crate::git::GitCache`] - see
/// [`DEFAULT_TTL`] for why this is safe to cache at all.
///
/// **What this must never do (PRO-217's constraint, restated as code):** a
/// failed refresh must surface as `Err` to the caller, exactly as an
/// uncached call would, never silently reused from a stale entry and never
/// degraded to an empty-but-successful result. [`get_or_fetch`](Self::get_or_fetch)
/// enforces this structurally: a fetch failure is returned immediately and
/// is never written into `entry` - so a transient `ps` hiccup while the
/// cache is warm never reaches the real enumeration at all (the fresh cached
/// value is served instead, insulating a currently-published profile's
/// directory from ever dropping out over a blip), while a hiccup after the
/// TTL has expired propagates exactly like an uncached failure always did,
/// and is retried on the very next call rather than being remembered as a
/// failure for the rest of the TTL.
///
/// Construct one per watcher process and reuse it across every sweep, for
/// the same reason as `GitCache`: a fresh cache per sweep would defeat the
/// whole point, since every lookup would always miss.
pub struct ProcessCache {
    ttl: Duration,
    entry: Mutex<Option<(Instant, Vec<ClaudeProcess>)>>,
}

impl ProcessCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entry: Mutex::new(None),
        }
    }

    /// Reuse a cached enumeration younger than this cache's TTL, or call
    /// `fetch` and cache its result - but only on success; see this type's
    /// doc comment for why a failed `fetch` is never cached.
    ///
    /// The mutex is never held across `fetch()`, exactly like `GitCache::
    /// get_or_fetch` - the same bounded-race trade-off applies (see its doc
    /// comment): two concurrent misses would both fetch, and the second
    /// `store` wins, which is acceptable wasted work rather than a
    /// correctness problem, safe today because sweeps run serially.
    fn get_or_fetch(
        &self,
        fetch: impl FnOnce() -> Result<Vec<ClaudeProcess>, DiscoveryError>,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        if let Some(processes) = self.fresh_entry() {
            return Ok(processes);
        }
        let processes = fetch()?;
        self.store(processes.clone());
        Ok(processes)
    }

    /// A cached enumeration younger than this cache's TTL, if any.
    fn fresh_entry(&self) -> Option<Vec<ClaudeProcess>> {
        let entry = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        entry.as_ref().and_then(|(fetched_at, processes)| {
            (fetched_at.elapsed() < self.ttl).then(|| processes.clone())
        })
    }

    /// Store a freshly-fetched enumeration, replacing whatever was cached
    /// before (there is only ever one entry here, unlike `GitCache`'s
    /// per-`cwd` map, since one enumeration serves the whole sweep).
    fn store(&self, processes: Vec<ClaudeProcess>) {
        let mut entry = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        *entry = Some((Instant::now(), processes));
    }
}

/// Enumerate live Claude Code processes and derive the registry
/// directories and tmux panes to use for this sweep.
///
/// Returns `Ok` with a [`Discovery`] whose `registry_dirs` always contains
/// at least the default config directory (see `union_discovery`'s doc
/// comment), even when enumeration succeeded but matched zero Claude
/// processes - there genuinely are no *extra* profiles, and sweeping just
/// the default directory (which itself degrades to a successful empty
/// sweep per PRO-207 if it has no registry) is the correct, self-healing
/// result. Returns `Err` only when enumeration itself could not be
/// completed, including the unfiltered-empty-list floor both platform
/// enumerators apply (see `DiscoveryError::EmptyProcessList`); the caller
/// must treat that as a read failure and publish nothing, exactly as
/// PRO-207 refused to publish when no registry directory was configured at
/// all.
///
/// `cache` (PRO-217) is consulted before any real enumeration is attempted -
/// see [`ProcessCache`]. `registry_dirs`, `tmux_panes`, and `live_pids` all
/// derive from the same cached read, since all three came from one process
/// enumeration to begin with (see this module's own doc comment) - there is
/// no cheaper way to refresh only one of them. `live_pids` feeds only
/// `sweep::sweep`'s orphaned-live-process warning (PRO-211), never which
/// sessions get published (see `registry::is_live`, which checks each
/// session's own pid against the OS directly, on every sweep, regardless of
/// this cache), so letting it lag by up to one TTL only delays a diagnostic
/// warning, not a correctness property.
///
/// `foreign_warnings` carries the cross-sweep warn-once state for a
/// foreign-uid Claude process whose environment cannot be read (PRO-211
/// second-round review finding 2) - see [`ForeignUidWarnings`]. The caller
/// owns it for the life of the process, exactly like `sweep::
/// OrphanWarnings`, so a given pid warns once while it stays in that state
/// rather than on every sweep. Note this bookkeeping only advances on a
/// cache miss (a real enumeration actually ran) - a cache hit skips it
/// entirely, which is fine: nothing needs re-warning about on a cycle that
/// never re-read the process table.
pub fn discover(
    cache: &ProcessCache,
    foreign_warnings: &mut ForeignUidWarnings,
) -> Result<Discovery, DiscoveryError> {
    let processes = cache.get_or_fetch(|| imp::enumerate_claude_processes(foreign_warnings))?;
    Ok(union_discovery(&processes))
}

/// Tmux panes and live pids captured from one process enumeration, without
/// deriving any registry directory from it. See [`discover_process_snapshot`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProcessSnapshot {
    pub tmux_panes: HashMap<i32, String>,
    pub live_pids: HashSet<i32>,
}

/// Capture tmux panes and live pids only, from the same process enumeration
/// [`discover`] uses, without deriving any registry directory from it.
///
/// This exists for `main.rs`'s explicit-directory-override path
/// (`CSM_WATCHER_REGISTRY_DIRS`): that override supplies its own registry
/// directories directly and so has no need of directory *discovery*, but it
/// still needs pane ids to resolve `tmux_target` - the override bypasses
/// discovering *where to look for sessions*, not enrichment of the sessions
/// once found. Before pane capture existed here, the override path reported
/// an empty pane map unconditionally, so no session published under the
/// override ever had a `tmux_target`, even when running inside tmux - a
/// silent, user-visible loss of jump-to-session for what PRO-204 documents
/// as a permanent, supported escape hatch, not scaffolding.
///
/// Live pids are captured here too, from the same enumeration (avoiding
/// paying for `ps`/`/proc` a second time per sweep under the override), but
/// - unlike the normal discovery path - `main.rs` deliberately does not feed
/// them to `sweep::sweep`'s orphaned-live-process check while the override
/// is set: see [`Discovery`]'s module-level doc comment above for why doing
/// so would be a pure false positive (PRO-211 review finding 3), not the
/// real gap the check exists to surface. `live_pids` is still returned here
/// rather than dropped, since it costs nothing to capture and keeps this
/// function's shape simple and uniform with the normal discovery path.
///
/// Unlike [`discover`], enumeration failing here is not propagated as an
/// error: this is enrichment, not the truth about which sessions exist, so
/// it degrades to an empty snapshot - "no tmux panes, no live pids this
/// sweep" - exactly like `tmux::resolve_all_panes` degrades when `tmux`
/// itself is unavailable; the caller must never let a capture failure
/// block, delay, or fail the sweep the override path is otherwise
/// perfectly able to complete.
///
/// `cache` (PRO-217) is the same [`ProcessCache`] instance `discover` uses -
/// under the `CSM_WATCHER_REGISTRY_DIRS` override, `main.rs` calls this
/// instead of `discover`, never both, so there is exactly one process
/// enumeration to cache per sweep either way.
pub fn discover_process_snapshot(
    cache: &ProcessCache,
    foreign_warnings: &mut ForeignUidWarnings,
) -> ProcessSnapshot {
    match cache.get_or_fetch(|| imp::enumerate_claude_processes(foreign_warnings)) {
        Ok(processes) => {
            let tmux_panes = processes
                .iter()
                .filter_map(|p| p.tmux_pane.clone().map(|pane| (p.pid, pane)))
                .collect();
            let live_pids = processes.iter().map(|p| p.pid).collect();
            ProcessSnapshot {
                tmux_panes,
                live_pids,
            }
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "process enumeration failed while capturing tmux panes and live pids for the \
                 explicit registry-dirs override; degrading to no enrichment and no \
                 orphaned-process check this sweep"
            );
            ProcessSnapshot::default()
        }
    }
}

/// Union `processes`' config directories - defaulting each unset, blank, or
/// non-absolute one to `~/.claude` - into a deduplicated directory list, and
/// collect their tmux panes keyed by pid.
///
/// The default config directory is seeded into the result unconditionally,
/// before any process is even considered - not only as the per-process
/// fallback. This is a deliberate floor against a total miss in
/// [`is_claude_command`]: an install invoked in some shape that function
/// still does not recognise (PRO-216 widened it to cover a `node <cli>`
/// wrapper and an install path containing a space - see its doc comment -
/// but cannot enumerate every shape a Claude Code install could ever
/// present as) matches zero entries in `processes`, and without this seed
/// `union_discovery` would return an empty directory list - a silent, total
/// wipe of every session on the host, indistinguishable from a genuine
/// "nothing running" sweep by the time it reaches `sweep`. Reproduced
/// directly, pre-PRO-216: feeding `union_discovery` a process list
/// containing only a `node <cli>`-shaped entry (no match in the
/// then-argv0-only `is_claude_exe`) produced zero directories before this
/// seed existed.
///
/// The seed is a floor, not a cure. It rescues only the *default* profile:
/// a session under a non-default `CLAUDE_CONFIG_DIR` whose process
/// `is_claude_command` still fails to recognise is still ended, and still
/// with a success exit, because nothing else knows that directory exists.
/// Widening recognition (PRO-216) closes the two concretely reproduced
/// shapes; this seed remains the backstop for whatever shape it still
/// misses.
///
/// This can never *resurrect* a session: every directory this function
/// returns is only ever consulted by `sweep`, which pid- and
/// `procStart`-verifies every entry it reads before treating it as live.
/// The seeded directory either has no `sessions/` subdirectory (a
/// successful empty read, per PRO-207), or has one but every entry in it is
/// independently verified. The only observable effect of seeding it is that
/// a total `is_claude_command` miss degrades to "the default profile is
/// still tracked" instead of a silent wipe.
///
/// This function is only ever reached on the discovery path - `main.rs`
/// calls it exclusively when the explicit `CSM_WATCHER_REGISTRY_DIRS`
/// override is absent - so seeding it here does not weaken that override's
/// promise to bypass discovery (and this default) entirely.
fn union_discovery(processes: &[ClaudeProcess]) -> Discovery {
    let mut registry_dirs = vec![default_config_dir()];
    let mut tmux_panes = HashMap::new();
    let mut live_pids = HashSet::new();
    for p in processes {
        let dir = resolve_process_config_dir(p.config_dir.as_deref(), p.home.as_deref());
        if !registry_dirs.contains(&dir) {
            registry_dirs.push(dir);
        }
        if let Some(pane) = &p.tmux_pane {
            tmux_panes.insert(p.pid, pane.clone());
        }
        live_pids.insert(p.pid);
    }
    Discovery {
        registry_dirs,
        tmux_panes,
        live_pids,
    }
}

/// Resolve one process's `CLAUDE_CONFIG_DIR` value into the directory to
/// sweep for it, falling back to [`default_config_dir_for`] (that same
/// process's own `HOME`, not the watcher's - see its doc comment) when the
/// value is absent, blank, or not an absolute path.
///
/// A relative value is rejected rather than resolved against the watcher's
/// own working directory: the value came from the *Claude process's*
/// environment, and its cwd is not the watcher's, so resolving it against
/// the watcher's cwd would sweep an arbitrary, almost certainly wrong,
/// directory - silently ending that profile's sessions on every future
/// sweep - rather than the one Claude Code actually uses. Falling back to
/// the process's own default config directory instead is not correct either
/// (Claude Code's own resolution of a relative `CLAUDE_CONFIG_DIR` is
/// unspecified here), but it fails toward "still tracked under some
/// directory" rather than toward "silently sweeping the wrong one".
fn resolve_process_config_dir(config_dir: Option<&str>, process_home: Option<&str>) -> PathBuf {
    match config_dir {
        Some(d) if !d.trim().is_empty() => {
            let trimmed = d.trim();
            let path = PathBuf::from(trimmed);
            if path.is_absolute() {
                path
            } else {
                tracing::warn!(
                    value = trimmed,
                    "CLAUDE_CONFIG_DIR is not an absolute path; falling back to the default \
                     config directory rather than resolving it against the watcher's own cwd"
                );
                default_config_dir_for(process_home)
            }
        }
        _ => default_config_dir_for(process_home),
    }
}

/// Resolve one Claude process's default config directory (`<HOME>/.claude`)
/// using *that process's own* `HOME` - captured from the same environment
/// read as `CLAUDE_CONFIG_DIR` - falling back to the watcher's own
/// [`default_config_dir`] only when the process's `HOME` is absent, blank,
/// or not an absolute path (PRO-211 second-round review finding 3,
/// pre-existing since PRO-208).
///
/// Before this fix, every process's default (`CLAUDE_CONFIG_DIR` unset) was
/// resolved against the *watcher's* `$HOME` unconditionally, via
/// `default_config_dir` alone, even though that process's own `HOME` was
/// already sitting right there in the very environment this module reads
/// `CLAUDE_CONFIG_DIR` and `TMUX_PANE` from. A watcher whose own `$HOME`
/// differs from the session owner's - a system-installed launchd/systemd
/// unit, a service account, a watcher started via `sudo` or under `su` -
/// swept the *watcher's* default profile instead of the real one, with a
/// successful exit: the wrong directory either has no `sessions/`
/// subdirectory (an empty sweep) or an unrelated one, and either way the
/// session owner's real default-profile sessions are absent from the
/// published snapshot and get ended.
///
/// A process's `HOME` reaching here as `None` is never itself the
/// "unreadable environment" case: `build_claude_processes` (and the Linux
/// enumerator) already turn a genuinely unreadable environment into
/// `DiscoveryError::UnreadableEnvironment` or a foreign-uid skip before a
/// `ClaudeProcess` is ever constructed - see their doc comments. So a
/// `None` here is an honest observation ("this process's environment was
/// read successfully and does not set `HOME`"), and falling back to the
/// watcher's own default is the documented, acceptable degrade for that
/// case, not a silent resolution to the wrong directory.
fn default_config_dir_for(process_home: Option<&str>) -> PathBuf {
    match process_home {
        Some(h) if !h.trim().is_empty() => {
            let trimmed = h.trim();
            let path = PathBuf::from(trimmed);
            if path.is_absolute() {
                path.join(".claude")
            } else {
                tracing::warn!(
                    value = trimmed,
                    "a Claude process's HOME is not an absolute path; falling back to the \
                     watcher's own default config directory rather than resolving it against \
                     the watcher's own cwd"
                );
                default_config_dir()
            }
        }
        _ => default_config_dir(),
    }
}

/// `~/.claude`, using *the watcher's own* `$HOME` the same way `main.rs`
/// already does for the log directory. This is a floor for two cases only:
/// `union_discovery`'s unconditional seed (there is no specific process to
/// derive it from), and [`default_config_dir_for`]'s own fallback when a
/// process's `HOME` is unavailable - it is deliberately **not** used
/// directly as any *process's* default config directory anymore (PRO-211
/// second-round review finding 3); see `default_config_dir_for`. Falling
/// back to `/.claude` when `$HOME` is unset mirrors `main.rs`'s log
/// directory precedent rather than inventing a new one; a watcher running
/// with no `$HOME` at all has bigger problems than this fallback's exact
/// value.
fn default_config_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
}

/// Whether a single token - typically an invoked executable path/name, but
/// also tried as a script-path candidate by [`is_claude_command`] - looks
/// like a Claude Code binary: its file stem (name without extension) is
/// `claude`, case-insensitively.
///
/// Matches a bare `claude` on `PATH`, a full install path
/// (`/opt/homebrew/Caskroom/claude-code/<ver>/claude`,
/// `~/.local/share/claude/versions/<ver>/claude`), and a `.exe`-suffixed
/// shim name observed from an npm-based install. Does not match
/// differently-named tools such as `claude-code` or `claudex`, which are
/// not Claude Code's own CLI process.
///
/// This alone is *narrower* than what a process actually looks like on the
/// wire: see [`is_claude_command`], which is what every real caller uses,
/// for the two additional shapes PRO-216 widened recognition to cover (a
/// `node <cli>` wrapper, and an executable path containing a literal
/// space). This function stays exactly what it was before that widening -
/// a single-token, exact-stem check - because [`is_claude_command`] needs
/// it as a building block for *several* candidate strings, not only argv0
/// verbatim.
fn is_claude_exe(exe: &str) -> bool {
    Path::new(exe)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("claude"))
}

/// How many consecutive command-line tokens [`is_claude_command`] will join
/// with a single space to reconstruct one candidate path/argument, when
/// working around `ps -Eww`'s and this parser's whitespace-joined, unquoted
/// token dump giving no way to mark where one argument ends and the next
/// begins (the same fundamental ambiguity [`parse_env_tokens`]'s doc
/// comment describes for environment values - this is the argv-side
/// counterpart). Four is comfortably above any real install path this
/// project has observed needing more than one merged segment for (a path
/// like `/Applications/My Claude App/2.1.206/claude` needs two), while
/// keeping the search bounded and cheap regardless of how many genuine CLI
/// arguments a line carries before its first real environment variable.
const MAX_TOKEN_MERGE: usize = 4;

/// Every candidate string formed by joining `tokens[start..]` one token at a
/// time, up to [`MAX_TOKEN_MERGE`] tokens - `tokens[start]` alone first (the
/// common, unambiguous case), then `tokens[start..=start+1]` joined with a
/// space, and so on. See [`MAX_TOKEN_MERGE`] for why this is bounded, and
/// [`is_claude_command`] for why only two starting positions (the
/// executable itself, and - for the `node` case - the token right after it)
/// are ever tried, not every position: trying every position would risk
/// folding an unrelated, later CLI argument into a false match.
fn merge_candidates(tokens: &[String], start: usize) -> impl Iterator<Item = String> + '_ {
    (start..tokens.len())
        .take(MAX_TOKEN_MERGE)
        .map(move |end| tokens[start..=end].join(" "))
}

/// Whether `candidate` - already put through [`merge_candidates`] - looks
/// like `node`, case-insensitively, allowing a `.exe` suffix the same way
/// [`is_claude_exe`] allows one for `claude` itself.
fn looks_like_node(candidate: &str) -> bool {
    Path::new(candidate)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("node"))
}

/// Whether `candidate` looks like Claude Code's own npm entry point: a file
/// stem of `cli` (case-insensitively) inside a path with a component named
/// `claude-code` (case-insensitively, matching the published package,
/// `@anthropic-ai/claude-code`).
///
/// Both conditions are required. `cli.js` alone is an extremely common
/// npm bin-shim filename - eslint, npm itself, and many other CLI tools all
/// ship a `cli.js` - so matching on it alone would recognise almost any
/// global npm tool running under `node` as a Claude process, an
/// unacceptably wide false-positive surface for something that changes
/// which sessions get published. Requiring the installed package's own
/// directory name narrows this back down to installs that are actually
/// Claude Code, at the cost of not recognising a hypothetical install laid
/// out under some entirely different directory name - a real but
/// deliberately accepted gap, backstopped by `union_discovery`'s
/// unconditional default-profile seed like every other recognition miss.
fn looks_like_claude_code_script(candidate: &str) -> bool {
    let path = Path::new(candidate);
    let is_cli_entry_point = path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("cli"));
    let under_claude_code_package = path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("claude-code"))
    });
    is_cli_entry_point && under_claude_code_package
}

/// Whether a process's command-line tokens (the invoked executable followed
/// by its own arguments, *before* the environment-variable boundary - see
/// [`parse_ps_line`] and the Linux `imp::enumerate_claude_processes`, which
/// both produce this from their respective raw formats) look like an
/// invocation of Claude Code, by more than [`is_claude_exe`] on argv0 alone
/// (PRO-216).
///
/// Two signals, tried in order:
///
/// 1. **The executable itself, space-tolerant.** [`is_claude_exe`] on
///    `tokens[0]` alone first (the fast, unchanged common case), then on
///    progressively space-joined candidates starting at `tokens[0]` (see
///    [`merge_candidates`]) - because a real install path can itself
///    contain a literal space (e.g. `/Applications/My Claude
///    App/2.1.206/claude`), which the unquoted token dump this project
///    reads on both platforms gives no way to mark a boundary for. Only
///    tried starting at position 0: merging tokens starting anywhere else
///    would risk folding an unrelated later argument into a false match.
/// 2. **A `node <script>` wrapper.** If some space-tolerant candidate
///    starting at `tokens[0]` is [`looks_like_node`], every space-tolerant
///    candidate starting right after it is tried against
///    [`looks_like_claude_code_script`]. This is the shape produced by an
///    `exec node … cli.js` wrapper and by an older npm install whose
///    `package.json` `bin` field is the literal file `cli.js` (both invoke
///    Claude Code as `node <path-to-cli.js>` rather than as a `claude`-named
///    binary at all), and the whole reason this ticket exists: PRO-208's
///    review reproduced that shape's process going unrecognised, which
///    rescues only the *default* config profile via `union_discovery`'s
///    seed and still silently ends every session under a non-default
///    `CLAUDE_CONFIG_DIR` with a successful exit, because nothing else
///    learns that directory exists.
///
/// Deliberately does **not** attempt to recognise every conceivable
/// wrapper shape (a Python launcher, an arbitrary shell shim, `bun run
/// cli.js`, ...) - only the two shapes PRO-216 concretely reproduced. Any
/// install this still misses degrades to the same default-profile-only
/// floor every prior recognition miss has always degraded to (see
/// `union_discovery`'s doc comment), not a silent total wipe.
fn is_claude_command(tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    for candidate in merge_candidates(tokens, 0) {
        if is_claude_exe(&candidate) {
            return true;
        }
    }
    for exe_end in 0..tokens.len().min(MAX_TOKEN_MERGE) {
        let exe_candidate = tokens[0..=exe_end].join(" ");
        if !looks_like_node(&exe_candidate) {
            continue;
        }
        for candidate in merge_candidates(tokens, exe_end + 1) {
            if looks_like_claude_code_script(&candidate) {
                return true;
            }
        }
    }
    false
}

/// Parse `ps -Eww -ax -o pid=,uid=,command=` output: one process per line,
/// `pid`, then `uid`, then the invoked command, its arguments, and its
/// environment - all whitespace-joined by `ps` with no quoting or escaping.
/// Returns every parseable line as `(pid, uid, command_tokens, env)`,
/// unfiltered; callers narrow to Claude processes with
/// [`is_claude_command`]. `command_tokens` is every whitespace-separated
/// token before the first token that looks like a real `KEY=VALUE`
/// environment assignment (PRO-216) - the invoked executable at position 0,
/// followed by its own arguments, exactly what `is_claude_command` needs to
/// see the `node <script>` and space-in-path shapes it recognises. `uid` is
/// what lets [`build_claude_processes`] distinguish a foreign user's process
/// from a genuine read failure of this watcher's own user's process
/// (PRO-211 second-round review finding 2).
///
/// A line that cannot be parsed is a discovery failure
/// ([`DiscoveryError::MalformedPsLine`], PRO-211 third-round review finding
/// 2), not a silent skip - see the error variant's doc comment for why, and
/// `parse_ps_line`'s for the two shapes of non-parsing line that are *not*
/// this error.
///
/// Compiled whenever this is the real parser in use (macOS) or whenever
/// tests are running (so the fixture tests below exercise it on every CI
/// platform, not only macOS) - never in a plain non-macOS, non-test build,
/// where it would otherwise be legitimately unused dead code.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_output(
    output: &str,
) -> Result<Vec<(i32, u32, Vec<String>, HashMap<String, String>)>, DiscoveryError> {
    let mut result = Vec::new();
    for line in output.lines() {
        match parse_ps_line(line) {
            PsLineOutcome::Entry(entry) => result.push(entry),
            PsLineOutcome::Benign => continue,
            PsLineOutcome::Malformed => {
                tracing::error!(
                    line,
                    "failed to parse a line of `ps -Eww -ax -o pid=,uid=,command=` output; \
                     treating this as a discovery failure rather than silently dropping the \
                     line, since this output is machine-formatted and a parse failure means \
                     something is actually wrong"
                );
                return Err(DiscoveryError::MalformedPsLine {
                    line: line.to_string(),
                });
            }
        }
    }
    Ok(result)
}

/// The three ways one line of `ps -Eww -ax -o pid=,uid=,command=` output can
/// come back from [`parse_ps_line`].
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq)]
enum PsLineOutcome {
    /// A full `pid uid command...` line, parsed successfully.
    Entry((i32, u32, Vec<String>, HashMap<String, String>)),
    /// Not a parse failure at all: a real, legitimate line that simply
    /// carries no process entry - see `parse_ps_line`'s doc comment for the
    /// two shapes this covers. Silently skipped, exactly like the pre-fix
    /// behaviour, but *only* for these specific shapes rather than for any
    /// unparseable line whatsoever.
    Benign,
    /// A non-blank line that does not fit the `pid uid command...` shape at
    /// all - see [`DiscoveryError::MalformedPsLine`]. The caller must fail
    /// discovery over this, not skip it.
    Malformed,
}

/// Parse the `uid=` column of one `ps` line.
///
/// macOS `ps` formats the uid column as a *signed* int, so uids above
/// `i32::MAX` come back negative: the `nobody` uid (`4294967294`) prints as
/// `-2`, and several system daemons run as it on any normal desktop. A plain
/// `parse::<u32>()` rejected those lines, which made [`parse_ps_line`] call
/// them [`PsLineOutcome::Malformed`] and abort discovery outright even though
/// nothing was wrong with the line or with the machine.
///
/// So: accept the unsigned spelling first, and fall back to the signed one,
/// reinterpreting its bits as the `uid_t` `ps` started from (`-2i32 as u32
/// == 4294967294`). Both spellings of a given uid therefore land on the same
/// value, and comparisons against [`current_uid`] stay correct.
#[cfg(any(target_os = "macos", test))]
fn parse_uid(uid_str: &str) -> Option<u32> {
    uid_str
        .parse::<u32>()
        .ok()
        .or_else(|| uid_str.parse::<i32>().ok().map(|uid| uid as u32))
}

/// Parse one line of `ps -Eww -ax -o pid=,uid=,command=` output.
///
/// Two shapes are legitimate, real output rather than a violation of the
/// expected format, and come back as [`PsLineOutcome::Benign`] rather than
/// [`PsLineOutcome::Malformed`] (PRO-211 third-round review finding 2):
///
/// - **A blank line.** Nothing at all, or only whitespace - not a process
///   entry to begin with. `ps` invocations do not normally emit one, but a
///   stubbed or wrapped `ps` (as this project's own test suite uses) might,
///   and there is no reason to treat an empty line as a sign something is
///   wrong when it plainly carries no data either way.
/// - **A process with an empty command column.** `pid` and `uid` both parse
///   as real numbers, but nothing follows: `ps` printed nothing at all in
///   the `command=` column for that process. This is a real, if rare, shape
///   `ps` can produce (not every process necessarily has a non-empty argv[0]
///   `ps` is willing to print), and it can never be a Claude process either
///   way (`is_claude_command` needs at least one token to match against), so
///   it is dropped as uninteresting rather than failing discovery over it.
///
/// Every other non-parsing shape - a garbled or missing pid, a garbled or
/// entirely missing uid column, anything else that does not fit
/// `pid uid command...` - is [`PsLineOutcome::Malformed`]: `ps -o
/// pid=,uid=,command=` guarantees pid and uid are always present and
/// numeric for any real process line, so a line that violates that
/// guarantee is never simply an unusual-but-valid process; a header line
/// would fall in this bucket too, but this project's `=`-suffixed column
/// spec (`pid=,uid=,command=`) already suppresses `ps` from ever emitting
/// one.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_line(line: &str) -> PsLineOutcome {
    if line.trim().is_empty() {
        return PsLineOutcome::Benign;
    }
    let line = line.trim_start();
    let Some((pid_str, rest)) = line.split_once(char::is_whitespace) else {
        return PsLineOutcome::Malformed;
    };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        return PsLineOutcome::Malformed;
    };
    let rest = rest.trim_start();
    let Some((uid_str, rest)) = rest.split_once(char::is_whitespace) else {
        return PsLineOutcome::Malformed;
    };
    let Some(uid) = parse_uid(uid_str.trim()) else {
        return PsLineOutcome::Malformed;
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        // pid and uid both parsed; there is simply no command left - see
        // this function's doc comment for why that is benign, not malformed.
        return PsLineOutcome::Benign;
    }
    let tokens: Vec<&str> = rest.split(' ').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        // rest is non-empty but contains no non-whitespace token at all
        // (e.g. embedded non-space whitespace only) - the same "no command"
        // shape as above.
        return PsLineOutcome::Benign;
    }
    // The command (invoked executable at position 0, followed by its own
    // arguments) is every token up to the first one that looks like a real
    // `KEY=VALUE` environment assignment - see `is_claude_command`'s doc
    // comment for why this project needs the whole command, not only
    // position 0. `parse_env_tokens` already computes this same boundary
    // internally (a token before its first recognised assignment has no
    // `current` pair to fold into, so it is silently dropped there); slicing
    // here up front just keeps a copy of what it would otherwise discard,
    // with no change to the resulting `env` map.
    let env_start = tokens
        .iter()
        .position(|token| env_assignment_key_len(token).is_some())
        .unwrap_or(tokens.len());
    let command_tokens: Vec<String> = tokens[..env_start].iter().map(|t| t.to_string()).collect();
    let env = parse_env_tokens(tokens[env_start..].iter().copied());
    PsLineOutcome::Entry((pid, uid, command_tokens, env))
}

/// Reconstruct `KEY=VALUE` pairs from `ps -E`'s whitespace-joined, unquoted
/// dump of a command's arguments followed by its environment.
///
/// A token starts a new pair only when it looks like a real env-var
/// assignment (`^[A-Za-z_][A-Za-z0-9_]*=`, checked by
/// [`env_assignment_key_len`]); any other token is folded into the value of
/// whichever pair most recently started. This is how a value containing
/// spaces - observed in the wild, e.g. a `PATH` entry
/// `/Applications/VMware Fusion.app/...` - gets put back together. Tokens
/// before the first such pair (the command's own name and arguments) are
/// dropped.
///
/// This reconstruction is inherently ambiguous when a *value* happens to
/// contain a substring shaped like `IDENT=...` of its own - `ps -E` gives
/// no way to disambiguate that from a real boundary. This is not merely a
/// hypothetical about `CLAUDE_CONFIG_DIR` or `TMUX_PANE`'s own values (which
/// are indeed always a bare filesystem path or a `%<n>` pane id and never
/// contain such a substring): it is a real shape produced by an exported
/// bash function, whose `ps`/`environ` representation is
/// `BASH_FUNC_name%%=() { ...body... }`, where `...body...` is
/// unconstrained shell source and can itself contain `CLAUDE_CONFIG_DIR=` -
/// exactly the two-profile helper shape
/// `BASH_FUNC_cw%%=() { CLAUDE_CONFIG_DIR=/opt/personal claude "$@" }`
/// reproduces. A token sequence like that creates a *second*, spurious
/// `CLAUDE_CONFIG_DIR=` boundary inside another variable's value, after the
/// real one.
///
/// The fix is first-wins (`entry(..).or_insert(..)` below), not last-wins
/// (`insert`, which this file used before this fix and which a spurious
/// later boundary silently overwrites). A real environment block from the
/// OS never has a duplicate key - every key in `/proc/<pid>/environ` and in
/// a live process's real environment is unique by construction - so any
/// repeat encountered here is not a second real variable but a false split
/// of some other value. First-wins keeps the genuine assignment and drops
/// the impostor; last-wins does the opposite.
///
/// This is an ordering argument, not a proof: first-wins is correct only
/// because the impostor appears *after* the real assignment. It does so in
/// the case that matters, since bash emits its `BASH_FUNC_*` exports after
/// regular variables - verified on bash 3.2.57 and 5.3.15. An impostor
/// planted ahead of the real assignment would still win, so this narrows
/// the hazard rather than eliminating it.
///
/// PRO-216 tried, and reverted, a brace-depth suppression scheme to close
/// that remaining gap: once a token containing `%%=` was seen, every
/// following token was folded opaquely until net `{`/`}` depth returned to
/// zero, so a function body's own `IDENT=...`-shaped substrings would never
/// reach [`env_assignment_key_len`] regardless of which side of the real
/// assignment they landed on. It was a regression, not a narrowing: it
/// consumed the token *after* any `%%=` marker unconditionally (no brace
/// imbalance or function required - `FOO=100%%=done` immediately before the
/// real assignment was enough to eat it), and an ordinary exported function
/// whose body contained an unbalanced brace inside a quoted string (e.g.
/// `echo "use { to open"`) kept suppression active far past the function's
/// real end, swallowing every subsequent real variable. Do not attempt a
/// cleverer version of this - the impostor-planted-before-the-real-
/// assignment gap is real but low severity and is not required to be closed
/// here; two attempts at closing it have now each produced something worse
/// than the gap itself. See the regression tests below
/// (`parse_env_tokens_*`) for the exact adversarial inputs that broke it.
///
/// Reproduced directly against a `ps -Eww`-shaped line carrying that exact
/// `BASH_FUNC_cw%%=` token sequence: pre-fix (`insert`), the recovered
/// `CLAUDE_CONFIG_DIR` was the fake path embedded in the function body, not
/// the process's real one, so the real profile's registry directory was
/// never swept and all its sessions were ended.
#[cfg(any(target_os = "macos", test))]
fn parse_env_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for token in tokens {
        if let Some(key_len) = env_assignment_key_len(token) {
            if let Some((key, value)) = current.take() {
                env.entry(key).or_insert_with(|| value.join(" "));
            }
            current = Some((token[..key_len].to_string(), vec![&token[key_len + 1..]]));
        } else if let Some((_, value)) = current.as_mut() {
            value.push(token);
        }
    }
    if let Some((key, value)) = current {
        env.entry(key).or_insert_with(|| value.join(" "));
    }
    env
}

/// If `token` looks like `IDENT=...`, returns the byte length of `IDENT`
/// (the index of the `=`). A valid `IDENT` starts with a letter or
/// underscore and continues with letters, digits, or underscores - the
/// same shape every real environment variable name has.
#[cfg(any(target_os = "macos", test))]
fn env_assignment_key_len(token: &str) -> Option<usize> {
    let eq = token.find('=')?;
    let mut chars = token[..eq].chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(eq)
}

/// Cross-sweep memory of which foreign-uid Claude processes' unreadable
/// environments have already produced a warning (PRO-211 second-round
/// review finding 2), so a given pid warns once while it stays in that
/// state, not on every sweep it remains alive. This is the exact same
/// problem, and the exact same fix, as `sweep::OrphanWarnings` - see its doc
/// comment - kept as a separate type since the two track unrelated pid sets
/// (orphaned-live-process vs. foreign-uid-unreadable-environment) that
/// happen to need identical warn-once treatment.
///
/// The caller (`main.rs`) owns one instance for the life of the process,
/// threading `&mut` through every [`discover`] / [`discover_process_snapshot`]
/// call, exactly like `OrphanWarnings`.
#[derive(Debug, Default)]
pub struct ForeignUidWarnings {
    warned: HashSet<i32>,
}

impl ForeignUidWarnings {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Pure decision, unit-tested directly: given `current` (this sweep's full
/// set of confirmed-Claude, foreign-uid, unreadable-environment pids),
/// return which of them must actually log a warning this call, and update
/// `warned` accordingly. Mirrors `sweep::new_orphan_pids`'s `retain`-then-
/// `insert` idiom exactly: a pid already warned about is forgotten the
/// moment it stops appearing in `current` (its process exited, or its
/// environment became readable), so a later, unrelated process reusing the
/// same pid - or the same process becoming readable and then unreadable
/// again - can still warn again rather than staying suppressed forever.
fn new_foreign_uid_pids(current: &HashSet<i32>, warned: &mut ForeignUidWarnings) -> Vec<i32> {
    warned.warned.retain(|pid| current.contains(pid));
    let mut new: Vec<i32> = current
        .iter()
        .copied()
        .filter(|pid| warned.warned.insert(*pid))
        .collect();
    new.sort_unstable();
    new
}

/// The watcher's own uid, used to distinguish "this Claude process belongs
/// to another user and its environment is genuinely unreadable to us" from
/// "we should have been able to read this and could not" (PRO-211
/// second-round review finding 2).
///
/// **Known limitation (PRO-211 third-round review finding 3):** this uid
/// comparison is a proxy for "unreadable by design", and that proxy is
/// false for a watcher running as root. A system-launchd/systemd deployment
/// (or anything else started as root) can read *every* user's environment,
/// so a genuine read failure on a uid-501 Claude process's environment
/// would, under this check, be misclassified as "foreign uid, expected to
/// be unreadable" and warn-and-skip (see [`build_claude_processes`] and the
/// Linux `imp::enumerate_claude_processes`) rather than fail discovery
/// loudly as PRO-211 otherwise intends for a same-uid read failure.
/// Reachability is low - root can normally read any process's environment
/// on both platforms this crate supports, so the read itself essentially
/// never fails for a root watcher in the first place - but the assumption
/// this check rests on ("a uid mismatch means we were never going to be
/// able to read it") is not universally true, so it is written down here
/// explicitly rather than left implicit.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn current_uid() -> u32 {
    // SAFETY: `getuid()` takes no arguments, dereferences no pointers, and
    // cannot fail - it is unconditionally safe to call.
    unsafe { libc::getuid() }
}

/// Filter `parsed` (every line `ps -Eww` produced, unfiltered) down to
/// Claude processes, and turn each into a [`ClaudeProcess`] - failing loudly
/// (PRO-211 review finding 4) rather than silently resolving to the default
/// config directory when a confirmed Claude process's environment could not
/// actually be read, *except* when that process belongs to another user
/// (PRO-211 second-round review finding 2 - see [`ForeignUidWarnings`] and
/// `DiscoveryError`'s doc comment for why uid matters), in which case it is
/// warned about once and skipped rather than failing discovery.
///
/// `ps -Eww` gives no way to distinguish "this process genuinely has an
/// empty environment" (not really possible for a real process - even a
/// minimal one inherits `PATH` at least) from "`ps` could not read this
/// process's environment at all and silently printed nothing for it"
/// (observed for processes owned by another user, where `ps` degrades
/// rather than erroring). An empty `env` map for a line `is_claude_command`
/// already matched is therefore never simply accepted as "no environment":
/// for a *same-uid* process (one this watcher should have been able to
/// read), it becomes [`DiscoveryError::UnreadableEnvironment`] - unlike a
/// *non*-Claude process with an empty environment (see
/// `parse_ps_output_handles_a_process_with_no_environment_at_all`'s pid-1
/// case, filtered out before this function ever sees it), a Claude
/// process's `CLAUDE_CONFIG_DIR` is truth this sweep needs, so "cannot
/// determine it" must not silently fall through to [`default_config_dir_for`]
/// via a `None` `config_dir`. For a *foreign-uid* process, it is instead
/// treated as a legitimate, expected gap - `ps` cannot see into another
/// user's environment at all, ever, regardless of anything this watcher
/// does - so it is warned about (once - see [`new_foreign_uid_pids`]) and
/// excluded from the result, rather than failing the whole sweep for as
/// long as that unrelated process happens to be alive.
///
/// A present-but-*narrow* environment (no `CLAUDE_CONFIG_DIR` or
/// `TMUX_PANE` key, but other keys still read - see
/// `discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_and_tmux_pane_from_one_read`'s
/// pid-23195 case) is unaffected by any of this: that is a real, successful
/// observation (the process simply never set those variables), not either
/// error/skip path above.
#[cfg(any(target_os = "macos", test))]
fn build_claude_processes(
    parsed: Vec<(i32, u32, Vec<String>, HashMap<String, String>)>,
    current_uid: u32,
    foreign_warnings: &mut ForeignUidWarnings,
) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
    let mut result = Vec::new();
    let mut foreign = HashSet::new();
    for (pid, uid, command_tokens, env) in parsed {
        if !is_claude_command(&command_tokens) {
            continue;
        }
        if env.is_empty() {
            if uid != current_uid {
                foreign.insert(pid);
                continue;
            }
            return Err(DiscoveryError::UnreadableEnvironment {
                pid,
                source: std::io::Error::other(
                    "ps -Eww reported no environment at all for this process, and it is owned \
                     by the same user this watcher runs as, so ps ought to have been able to \
                     read it - this is a genuine read failure, not another user's process ps \
                     cannot see into",
                ),
            });
        }
        result.push(ClaudeProcess {
            pid,
            config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
            tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
            home: env.get(HOME_VAR).cloned(),
        });
    }
    for pid in new_foreign_uid_pids(&foreign, foreign_warnings) {
        tracing::warn!(
            pid,
            "matched a Claude process owned by another user; ps could not read its \
             environment, so its CLAUDE_CONFIG_DIR (if any) cannot be determined - skipping \
             this process rather than treating it as a discovery failure"
        );
    }
    Ok(result)
}

/// Parse a `/proc/<pid>/environ` blob: NUL-separated `KEY=VALUE` entries.
/// Unlike `ps -E`, this is unambiguous - NUL cannot appear inside a real
/// environment variable's key or value, so no reconstruction heuristic is
/// needed.
///
/// Compiled on Linux (the real caller) or under test (so this runs on
/// whatever platform CI happens to be, including macOS, per PRO-208's
/// testing decisions - the impure `/proc` walk in `imp::
/// enumerate_claude_processes` below cannot be exercised outside real
/// Linux, but this pure parser can and must be).
#[cfg(any(target_os = "linux", test))]
fn parse_environ_blob(raw: &[u8]) -> HashMap<String, String> {
    raw.split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let s = String::from_utf8_lossy(entry);
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::time::Duration;

    /// Upper bound on the `ps -Eww -ax -o pid=,command=` invocation.
    ///
    /// This is heavier than `git::DEFAULT_COMMAND_TIMEOUT` or
    /// `tmux::LIST_PANES_TIMEOUT`: it dumps the full command line *and*
    /// environment of every process on the host, not one repository's
    /// worth of plumbing output. Measured directly on this (otherwise
    /// idle) machine: ~114ms at 876 processes, ~370ms at 1874 (after
    /// spawning ~1000 extra `sleep` processes), ~615ms at 2868 (after
    /// ~2000 extra) - a roughly linear ~0.2ms/process. Extrapolating that
    /// slope, a genuinely busy host would need tens of thousands of
    /// processes to approach this bound, which 5 seconds comfortably
    /// outlives with real headroom to spare, while still being far short
    /// of "unbounded": before this timeout existed, a hung `ps` wedged
    /// `csm-watcher --once` past 15 seconds (still running when force-
    /// terminated) rather than degrading.
    ///
    /// Routed through `crate::command::run`, the same bounded runner
    /// `git` and `tmux` use, for the same reason: under PRO-210's polling
    /// loop, a bare `Command::output()` with no timeout - the shape this
    /// module used before this fix - wedges the daemon permanently the
    /// first time `ps` itself hangs, on both the discovery path (`ps`
    /// timing out becomes a discovery failure, publishing nothing - see
    /// `DiscoveryError`) and the `CSM_WATCHER_REGISTRY_DIRS` override path
    /// (`discover_process_snapshot` degrades the same timeout to an empty
    /// snapshot instead, since pane/live-pid capture there is enrichment,
    /// not truth about which sessions exist).
    const PS_TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) fn enumerate_claude_processes(
        foreign_warnings: &mut ForeignUidWarnings,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        let stdout = run_ps("ps")?;
        let parsed = parse_ps_output(&stdout)?;
        // Floor: the *unfiltered* process list - before `is_claude_command`
        // narrows it - must never be empty on a live host (there is always
        // at least this watcher's own `ps` child and the OS's init
        // process). A successful-but-empty `ps` run is indistinguishable
        // from a broken one (a stub or shadowed `ps` on `PATH` that exits 0
        // printing nothing), so treat it as a failed enumeration rather
        // than "zero Claude processes found" - see `DiscoveryError`'s doc
        // comment for the reproduction.
        if parsed.is_empty() {
            return Err(DiscoveryError::EmptyProcessList);
        }
        build_claude_processes(parsed, current_uid(), foreign_warnings)
    }

    /// Run `<program> -Eww -ax -o pid=,uid=,command=`, bounded by
    /// [`PS_TIMEOUT`], and return its stdout. `-E` includes each process's
    /// environment; `-ww` disables output truncation (the default width
    /// would cut off exactly the tail end - the environment - that this
    /// module needs); `-ax` lists every process, not just ones attached to
    /// a terminal; `uid=` (PRO-211 second-round review finding 2) is what
    /// lets [`build_claude_processes`] tell a foreign user's process apart
    /// from a genuine read failure of this watcher's own user's process.
    ///
    /// A missing binary, a non-zero exit, empty output, or exceeding
    /// [`PS_TIMEOUT`] are all indistinguishable failures of enumeration
    /// itself from this function's caller's point of view, so all four
    /// collapse to the same `DiscoveryError::Enumerate`. That is
    /// deliberately *not* the same thing as `EmptyProcessList` above: this
    /// path fires when `ps` itself could not be run to completion at all,
    /// while `EmptyProcessList` fires when it ran and returned parseable-
    /// but-empty output. Both are still `DiscoveryError`, so both
    /// propagate identically through `discover()` (refuse to publish) and
    /// both degrade identically through `discover_process_snapshot()`
    /// (empty snapshot) - the distinction exists only for the log message.
    ///
    /// `program` is parameterised only so a test can point this at a
    /// binary that does not exist and exercise the "enumeration failed
    /// outright" error path, without mutating process-global state like
    /// `PATH`.
    fn run_ps(program: &str) -> Result<String, DiscoveryError> {
        crate::command::run(
            program,
            &["-Eww", "-ax", "-o", "pid=,uid=,command="],
            None,
            PS_TIMEOUT,
        )
        .ok_or_else(|| {
            DiscoveryError::Enumerate(std::io::Error::other(format!(
                "{program} -Eww -ax -o pid=,uid=,command= failed, produced no output, or \
                 exceeded {PS_TIMEOUT:?}"
            )))
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn run_ps_surfaces_a_missing_binary_as_a_discovery_error() {
            // Also stands in for the timeout path: `run_ps` maps whatever
            // reason `command::run` returns `None` for (missing binary
            // here, a timeout in production) to the same
            // `DiscoveryError::Enumerate` - a real enumeration failure that
            // `discover()` propagates and refuses to publish over, never an
            // empty `Ok` indistinguishable from a genuinely idle host.
            // `EmptyProcessList` is a distinct, narrower case (`ps` ran and
            // returned parseable-but-empty output), asserted separately by
            // the `union_discovery` tests below.
            let err = run_ps("definitely-not-a-real-ps-binary-xyz").unwrap_err();
            assert!(matches!(err, DiscoveryError::Enumerate(_)));
        }

        #[test]
        fn ps_timeout_bounds_a_hung_invocation_well_under_its_own_duration() {
            // Reproduces finding 1 from the second PRO-209 review round
            // directly: before this fix, `enumerate_claude_processes` shelled
            // out to `ps` via a bare `Command::output()` with no timeout at
            // all, so a hung `ps` on `PATH` left `csm-watcher --once` still
            // running - having published nothing - past 15 seconds before it
            // had to be force-terminated. This proves `PS_TIMEOUT` actually
            // bounds a hung invocation, exercised through `command::run`
            // directly (mirroring `tmux::run_list_panes`'s identical timeout
            // test) since `run_ps` itself always invokes the real `ps`
            // binary name and cannot be pointed at a hanging script without
            // mutating process-global `PATH`.
            let start = std::time::Instant::now();
            let result = crate::command::run("sh", &["-c", "sleep 30"], None, PS_TIMEOUT);
            assert_eq!(result, None);
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "must not block anywhere near the hung command's own sleep duration, took {:?}",
                start.elapsed()
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    /// Environment variable that, if set to a non-blank value, replaces
    /// `/proc` as the root this module reads pid directories, `cmdline`, and
    /// `environ` from.
    ///
    /// Mirrors `sweep::REGISTRY_DIRS_ENV`'s own doc comment almost exactly,
    /// deliberately: same shape of problem (a real OS surface this crate
    /// reads directly, that integration tests need to substitute a
    /// controlled fixture for), same fix (a permanent, documented override,
    /// not test-only scaffolding removed later). Production code never sets
    /// this - it exists so `crates/server/tests/reconciliation.rs`'s
    /// anti-wipe tests, which intercept macOS discovery by putting a stub
    /// `ps` on `PATH`, have a Linux equivalent: a fake `/proc/<pid>/
    /// {cmdline,environ}` tree under a tempdir, pointed at by this variable,
    /// exercising the *real* `enumerate_claude_processes` body end to end
    /// (PRO-216) rather than leaving it covered on macOS only.
    pub const PROC_ROOT_ENV: &str = "CSM_WATCHER_PROC_ROOT";

    /// The effective `/proc` root: [`PROC_ROOT_ENV`] if set to a non-blank
    /// value, `/proc` otherwise. Blank or whitespace-only is treated as
    /// unset, matching `sweep::registry_dirs_from_env`'s handling of its own
    /// override (a launchd/systemd unit or an unset shell substitution can
    /// each produce an empty value without meaning to configure anything).
    fn proc_root() -> PathBuf {
        match std::env::var_os(PROC_ROOT_ENV) {
            Some(val) => match val.to_str() {
                Some(s) if !s.trim().is_empty() => PathBuf::from(s),
                _ => PathBuf::from("/proc"),
            },
            None => PathBuf::from("/proc"),
        }
    }

    pub(super) fn enumerate_claude_processes(
        foreign_warnings: &mut ForeignUidWarnings,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        let proc_root = proc_root();
        let read_dir = fs::read_dir(&proc_root).map_err(DiscoveryError::Enumerate)?;
        let mut result = Vec::new();
        let mut foreign = HashSet::new();
        let this_uid = current_uid();
        // Unfiltered count of pid directories actually seen under `/proc`,
        // independent of whether any of them turn out to be Claude
        // processes. A live host always has at least this process itself,
        // so zero here can only mean `/proc` was not really read (e.g. it
        // was bind-mounted empty, or listing raced a container/namespace
        // boundary) - the Linux analogue of the macOS floor below: see
        // `DiscoveryError::EmptyProcessList`'s doc comment.
        let mut pid_dir_count = 0usize;
        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                // Not a pid directory (e.g. `/proc/self`, `/proc/net`).
                continue;
            };
            pid_dir_count += 1;
            // `cmdline` gives argv, NUL-separated - the full command line,
            // not just argv[0] - the same thing the macOS path reconstructs
            // from `ps`'s command column via `parse_ps_line`. Unlike the
            // macOS side, this is already unambiguous: NUL cannot appear
            // inside a real argument, so no space-merging heuristic is
            // needed to recover it, only to consume it (`is_claude_command`
            // still tries merged candidates on this side, purely so the
            // same PRO-216 recognition logic runs unchanged on both
            // platforms). A read failure here almost always means the
            // process has since exited (a race against enumeration, not a
            // real error) or belongs to another user; either way it is not
            // a Claude process we can identify, so it is skipped rather
            // than failing the whole enumeration.
            let Ok(cmdline) = fs::read(proc_root.join(pid.to_string()).join("cmdline")) else {
                continue;
            };
            let argv: Vec<String> = cmdline
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
            if !is_claude_command(&argv) {
                continue;
            }
            // From here the process is confirmed to be a Claude binary, so
            // its `CLAUDE_CONFIG_DIR` (if any) is truth this sweep needs,
            // not enrichment - unlike `cmdline` above, a failure here must
            // not be swallowed. Before this fix (PRO-211 review finding 4)
            // this branch logged a warning and skipped the process, which
            // silently dropped that profile's registry directory from
            // `union_discovery`'s input and ended its sessions with a
            // successful exit. Propagating `DiscoveryError::
            // UnreadableEnvironment` instead makes `discover()` refuse to
            // publish, matching the "fail loudly" contract this ticket
            // exists to make structural. This is not the same leniency as
            // `registry::read_entries`'s per-file skip-and-warn: a
            // malformed *registry* file is Claude Code's own format, which
            // this project does not control and cannot assume is always
            // well-formed, whereas a Claude process's environment being
            // unreadable here means this sweep's very knowledge of which
            // directories exist is incomplete - the same "cannot determine"
            // class as an unreadable registry directory itself.
            //
            // Exactly one exception (PRO-211 second-round review finding 2,
            // applied here for the same reason it was applied to the macOS
            // `ps` path even though the finding's own reproduction was
            // macOS-specific): a process owned by another uid is never
            // readable to this watcher, by design of the OS - `/proc/<pid>/
            // environ` is `0400`, owned by that process's own user - so an
            // `EACCES` here for a foreign-uid process is not "we should
            // have been able to read this and could not", it is "we were
            // never going to be able to read this, and that is fine
            // provided we say so". Failing discovery outright over an
            // unrelated `sudo claude` or a shared host's other users would
            // otherwise be a permanent, self-inflicted outage exactly like
            // the macOS case - see `DiscoveryError`'s doc comment.
            let raw = match fs::read(proc_root.join(pid.to_string()).join("environ")) {
                Ok(raw) => raw,
                Err(source) => {
                    let owner_uid = fs::metadata(proc_root.join(pid.to_string()))
                        .ok()
                        .map(|m| m.uid());
                    if owner_uid.is_some_and(|uid| uid != this_uid) {
                        foreign.insert(pid);
                        continue;
                    }
                    return Err(DiscoveryError::UnreadableEnvironment { pid, source });
                }
            };
            let env = parse_environ_blob(&raw);
            result.push(ClaudeProcess {
                pid,
                config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
                tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
                home: env.get(HOME_VAR).cloned(),
            });
        }
        if pid_dir_count == 0 {
            return Err(DiscoveryError::EmptyProcessList);
        }
        for pid in new_foreign_uid_pids(&foreign, foreign_warnings) {
            tracing::warn!(
                pid,
                "matched a Claude process owned by another user; /proc/<pid>/environ could \
                 not be read, so its CLAUDE_CONFIG_DIR (if any) cannot be determined - \
                 skipping this process rather than treating it as a discovery failure"
            );
        }
        Ok(result)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs;

        // These tests mutate the process-global `PROC_ROOT_ENV` variable;
        // Rust runs tests in the same binary concurrently by default, so
        // they're serialized on this lock to avoid racing each other -
        // mirrors `sweep::tests::ENV_LOCK` exactly, for the same reason.
        static ENV_LOCK: Mutex<()> = Mutex::new(());

        /// PRO-216: the Linux analogue of the macOS `discovery_path` e2e
        /// tests in `crates/server/tests/reconciliation.rs`, which intercept
        /// discovery by putting a stub `ps` on `PATH` - a mechanism with no
        /// Linux equivalent, since Linux reads `/proc` directly rather than
        /// shelling out. `PROC_ROOT_ENV` is what gives this same kind of
        /// test a Linux-side seam: point it at a directory that was never
        /// created at all, and enumeration must fail outright rather than
        /// silently degrading to "no processes found".
        #[test]
        fn enumerate_claude_processes_fails_outright_when_the_proc_root_does_not_exist() {
            let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let missing = tmp.path().join("does-not-exist");
            // SAFETY: env mutation is serialized via ENV_LOCK above.
            unsafe { std::env::set_var(PROC_ROOT_ENV, &missing) };
            let result = enumerate_claude_processes(&mut ForeignUidWarnings::new());
            unsafe { std::env::remove_var(PROC_ROOT_ENV) };
            assert!(
                matches!(result, Err(DiscoveryError::Enumerate(_))),
                "expected Enumerate for a proc root that was never created, got {result:?}"
            );
        }

        /// The Linux analogue of
        /// `watcher_refuses_to_publish_when_ps_reports_zero_processes_total`
        /// in `reconciliation.rs`: an existing-but-empty proc root is the
        /// `EmptyProcessList` floor's own direct trigger, exercised here
        /// through the real `enumerate_claude_processes` body rather than
        /// only asserting the `pid_dir_count == 0` check in isolation.
        #[test]
        fn enumerate_claude_processes_reports_empty_process_list_for_an_empty_proc_root() {
            let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            // SAFETY: env mutation is serialized via ENV_LOCK above.
            unsafe { std::env::set_var(PROC_ROOT_ENV, tmp.path()) };
            let result = enumerate_claude_processes(&mut ForeignUidWarnings::new());
            unsafe { std::env::remove_var(PROC_ROOT_ENV) };
            assert!(
                matches!(result, Err(DiscoveryError::EmptyProcessList)),
                "expected EmptyProcessList for a proc root with zero pid directories, got \
                 {result:?}"
            );
        }

        /// The Linux analogue of
        /// `watcher_refuses_to_publish_when_a_ps_lines_uid_column_is_malformed`:
        /// on the `ps` text-parsing side that test corrupts the uid column
        /// of one of two profiles' lines and asserts the whole sweep fails
        /// rather than silently publishing the other profile alone. Linux
        /// never parses a uid column at all (`fs::metadata` gives the owner
        /// uid directly, unambiguously), so the equivalent fault this
        /// enumerator can actually hit is a confirmed same-uid Claude
        /// process whose `environ` cannot be read - proven directly here by
        /// a fake pid directory with a Claude-shaped `cmdline` but no
        /// `environ` file at all, alongside one fully-readable profile.
        /// Discovery must still fail the whole enumeration, not silently
        /// drop the unreadable profile and return only the other one.
        #[test]
        fn enumerate_claude_processes_fails_outright_on_a_same_uid_unreadable_environment() {
            let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();

            let readable_pid = tmp.path().join("111");
            fs::create_dir(&readable_pid).unwrap();
            fs::write(readable_pid.join("cmdline"), b"claude\0").unwrap();
            fs::write(
                readable_pid.join("environ"),
                b"CLAUDE_CONFIG_DIR=/opt/profile-a/.claude\0",
            )
            .unwrap();

            let unreadable_pid = tmp.path().join("222");
            fs::create_dir(&unreadable_pid).unwrap();
            fs::write(unreadable_pid.join("cmdline"), b"claude\0").unwrap();
            // No `environ` file at all: this pid directory is owned by the
            // test process itself (the same uid `current_uid()` returns
            // here), so this is indistinguishable from a genuine same-uid
            // read failure, not a foreign-uid gap.

            // SAFETY: env mutation is serialized via ENV_LOCK above.
            unsafe { std::env::set_var(PROC_ROOT_ENV, tmp.path()) };
            let result = enumerate_claude_processes(&mut ForeignUidWarnings::new());
            unsafe { std::env::remove_var(PROC_ROOT_ENV) };
            assert!(
                matches!(
                    result,
                    Err(DiscoveryError::UnreadableEnvironment { pid: 222, .. })
                ),
                "expected UnreadableEnvironment for pid 222, got {result:?} - the readable \
                 profile (pid 111) must not be silently published alone"
            );
        }

        #[test]
        fn proc_root_falls_back_to_proc_when_the_override_is_unset_or_blank() {
            let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: env mutation is serialized via ENV_LOCK above.
            unsafe { std::env::remove_var(PROC_ROOT_ENV) };
            assert_eq!(proc_root(), PathBuf::from("/proc"));
            // SAFETY: env mutation is serialized via ENV_LOCK above.
            unsafe { std::env::set_var(PROC_ROOT_ENV, "   ") };
            let blank = proc_root();
            unsafe { std::env::remove_var(PROC_ROOT_ENV) };
            assert_eq!(
                blank,
                PathBuf::from("/proc"),
                "a blank or whitespace-only override must be treated as unset, matching \
                 sweep::registry_dirs_from_env's handling of its own override"
            );
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub(super) fn enumerate_claude_processes(
        _foreign_warnings: &mut ForeignUidWarnings,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        Err(DiscoveryError::Enumerate(std::io::Error::other(
            "process discovery is not implemented on this platform",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // --- is_claude_exe ---

    #[test]
    fn is_claude_exe_matches_bare_name_full_path_and_exe_suffix_case_insensitively() {
        assert!(is_claude_exe("claude"));
        assert!(is_claude_exe(
            "/opt/homebrew/Caskroom/claude-code/2.1.206/claude"
        ));
        assert!(is_claude_exe(
            "/Users/x/.local/share/mise/installs/node/24/bin/claude.exe"
        ));
        assert!(is_claude_exe("Claude"));
    }

    #[test]
    fn is_claude_exe_rejects_similarly_named_tools() {
        assert!(!is_claude_exe("claude-code"));
        assert!(!is_claude_exe("claudex"));
        assert!(!is_claude_exe("/usr/bin/vim"));
        assert!(!is_claude_exe(""));
    }

    // --- is_claude_command (PRO-216) ---

    fn toks(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn is_claude_command_matches_the_unchanged_common_case_directly() {
        assert!(is_claude_command(&toks(&["claude", "--model", "opus"])));
        assert!(is_claude_command(&toks(&[
            "/opt/homebrew/Caskroom/claude-code/2.1.206/claude"
        ])));
    }

    #[test]
    fn is_claude_command_recognises_an_install_path_containing_a_literal_space() {
        // Reproduces the ps/`/proc` tokenizer ambiguity directly: an install
        // path like `/Applications/My Claude App/2.1.206/claude` arrives as
        // several separate whitespace-split tokens with no way to mark
        // where the path itself ends, since neither ps -Eww nor /proc/<pid>/
        // cmdline's argv boundary survives this project's whitespace-joined
        // reconstruction the same way. `is_claude_exe` on the first token
        // alone ("/Applications/My") matches nothing.
        assert!(!is_claude_exe("/Applications/My"));
        let command = toks(&["/Applications/My", "Claude", "App/2.1.206/claude"]);
        assert!(is_claude_command(&command));
    }

    #[test]
    fn is_claude_command_bounds_space_merging_to_position_zero_only() {
        // A later, unrelated argument that happens to merge into something
        // claude-shaped must not create a false match - only tokens
        // starting at position 0 (the executable itself) are ever tried for
        // merging, never an arbitrary later position.
        let command = toks(&["node", "server.js", "--name", "claude", "runner"]);
        assert!(
            !is_claude_command(&command),
            "a `claude`-shaped later argument must not itself trigger a match"
        );
    }

    #[test]
    fn is_claude_command_recognises_a_node_wrapped_claude_code_cli_script() {
        // The shape this ticket exists for: an `exec node ... cli.js`
        // wrapper, or an older npm install with `bin: cli.js`, invokes
        // Claude Code as `node <path>/cli.js` rather than as a
        // `claude`-named binary at all.
        assert!(is_claude_command(&toks(&[
            "node",
            "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            "--model",
            "opus"
        ])));
        // Also space-tolerant on the script side, and recognises a `node`
        // invoked via a full path or `.exe` suffix.
        assert!(is_claude_command(&toks(&[
            "/usr/local/bin/node",
            "/opt/My",
            "Apps/claude-code/cli.js"
        ])));
        assert!(is_claude_command(&toks(&[
            "node.exe",
            "/opt/npm/claude-code/cli.js"
        ])));
    }

    #[test]
    fn is_claude_command_does_not_widen_to_match_an_unrelated_node_tool() {
        // The false-positive bound this ticket asks for explicit
        // justification of: `cli.js` alone is an extremely common npm
        // bin-shim filename (eslint, npm itself, and many other tools all
        // ship one), so matching on it without also requiring a
        // `claude-code` package-directory component would recognise nearly
        // any global npm tool run under `node` as a Claude process.
        assert!(!is_claude_command(&toks(&[
            "node",
            "/opt/homebrew/lib/node_modules/eslint/bin/cli.js"
        ])));
        // A node process not running any recognisable Claude Code script at
        // all - the ordinary case this must never match.
        assert!(!is_claude_command(&toks(&["node", "/opt/app/server.js"])));
        // `claude-code` appears in the path, but the entry point is not
        // `cli` - both signals are required, not either alone.
        assert!(!is_claude_command(&toks(&[
            "node",
            "/opt/homebrew/lib/node_modules/claude-code/index.js"
        ])));
    }

    #[test]
    fn is_claude_command_rejects_empty_and_unrelated_commands() {
        assert!(!is_claude_command(&[]));
        assert!(!is_claude_command(&toks(&["/usr/bin/vim", "file.txt"])));
    }

    // --- macOS `ps -Eww` parsing (pure - fixture bytes, no subprocess) ---

    /// The uid every fixture "own-user" process below is owned by, standing
    /// in for the watcher's own `current_uid()` in tests - see
    /// [`ForeignUidWarnings`]-related tests below for the contrasting
    /// foreign-uid case (pid 99999, owned by `0`/root).
    const TEST_UID: u32 = 501;

    /// A representative capture of `ps -Eww -ax -o pid=,uid=,command=`
    /// output: a right-padded pid column, two Claude processes under
    /// different `CLAUDE_CONFIG_DIR`s (two profiles), a full-path Claude
    /// install, a `.exe`-suffixed Claude process, two non-Claude processes
    /// (one with no environment at all), a `PATH` value containing a
    /// literal space to prove reconstruction survives it without
    /// corrupting a neighbouring key, and a *foreign-uid* Claude process
    /// (pid 99999, uid 0) with no environment at all - reproducing,
    /// directly on this project's own host, the reviewer's `1 /sbin/launchd`
    /// and `653 /usr/libexec/corebrightnessd --launchd` observation that
    /// `ps -Eww` prints a full command line for another user's process
    /// while silently suppressing its environment, rather than erroring
    /// (PRO-211 second-round review finding 2).
    const SAMPLE_PS_OUTPUT: &str = "\
65682   501 claude --model claude-opus-5 SSH_TTY=/dev/ttys017 CLAUDE_CONFIG_DIR=/opt/profile-a/.claude TMUX_PANE=%38 HOME=/opt/profile-a
 1760   501 claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work TMUX_PANE=%2 HOME=/opt/profile-b
23195   501 /opt/homebrew/Caskroom/claude-code/2.1.206/claude HOME=/opt/profile-a
  131   501 /Applications/Ghostty.app/Contents/MacOS/ghostty OSLogRateLimit=64 USER=simon HOME=/opt/profile-a
    1     0 /sbin/launchd
70773   501 /Users/x/.local/share/mise/installs/node/24/bin/claude.exe CLAUDE_CONFIG_DIR=/opt/profile-a/.claude TMUX_PANE=%5 HOME=/opt/profile-a
95255   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude PATH=/opt/my dir/bin:/usr/bin TMUX_PANE=%7 HOME=/opt/profile-a
99999     0 claude
";

    #[test]
    fn parse_ps_output_extracts_pid_uid_exe_and_full_env_per_line() {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT).unwrap();
        assert_eq!(parsed.len(), 8, "every line, Claude or not, is parsed");

        let (pid, uid, command_tokens, env) =
            parsed.iter().find(|(pid, ..)| *pid == 65682).unwrap();
        assert_eq!(*pid, 65682);
        assert_eq!(*uid, 501);
        assert_eq!(
            command_tokens.as_slice(),
            ["claude", "--model", "claude-opus-5"],
            "the full command, not only argv0, must survive parsing"
        );
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(env.get(TMUX_PANE_VAR).map(String::as_str), Some("%38"));
    }

    #[test]
    fn parse_ps_output_reconstructs_a_value_containing_a_literal_space_without_corrupting_neighbours()
     {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT).unwrap();
        let (_, _, _, env) = parsed.iter().find(|(pid, ..)| *pid == 95255).unwrap();
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/opt/my dir/bin:/usr/bin"),
            "the space inside the PATH value must be preserved, not treated as a new key"
        );
        // The keys either side of the space-containing PATH value must
        // still be extracted correctly.
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(env.get(TMUX_PANE_VAR).map(String::as_str), Some("%7"));
    }

    #[test]
    fn parse_ps_output_handles_a_process_with_no_environment_at_all() {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT).unwrap();
        let (_, uid, command_tokens, env) = parsed.iter().find(|(pid, ..)| *pid == 1).unwrap();
        assert_eq!(command_tokens.as_slice(), ["/sbin/launchd"]);
        assert_eq!(
            *uid, 0,
            "launchd is root-owned - part of why this case must not be an error"
        );
        assert!(env.is_empty());
    }

    // --- PRO-211 third-round review finding 2: a malformed ps line must
    // fail discovery, not be silently dropped ---

    #[test]
    fn parse_ps_output_fails_on_a_line_with_an_unparseable_uid_column_instead_of_dropping_it() {
        // Reproduces the reviewer's demonstration directly: two live
        // profiles, one line's uid column corrupted. Before this fix,
        // `parse_ps_output`'s `filter_map` silently dropped the malformed
        // line, `build_claude_processes` never saw it at all, and
        // discovery succeeded with only the other profile - a
        // silently-incomplete process list ending every session the
        // dropped profile had, with a successful exit.
        let two_profiles_one_corrupt = "\
65682   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude
 1760   notauid claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work
";
        let err = parse_ps_output(two_profiles_one_corrupt).unwrap_err();
        assert!(
            matches!(err, DiscoveryError::MalformedPsLine { .. }),
            "expected MalformedPsLine, got {err:?}"
        );
    }

    #[test]
    fn parse_ps_output_accepts_the_negative_uid_macos_prints_for_nobody() {
        // macOS `ps` formats the uid column signed, so the `nobody` uid
        // (4294967294) prints as `-2` - and system daemons run as it on any
        // normal desktop. Treating that as a malformed line made
        // `csm-watcher --once` abort discovery on a perfectly healthy
        // machine, with no snapshot published at all.
        let with_nobody = "\
  605    -2 /usr/sbin/distnoted agent
65682   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude
";
        let parsed = parse_ps_output(with_nobody).unwrap();
        assert_eq!(
            parsed
                .iter()
                .map(|(pid, uid, ..)| (*pid, *uid))
                .collect::<Vec<_>>(),
            vec![(605, u32::MAX - 1), (65682, 501)],
            "the -2 line must parse as uid 4294967294, not fail discovery"
        );
    }

    #[test]
    fn parse_ps_output_fails_on_a_line_missing_the_uid_column_entirely() {
        // A pid column that parses fine but nothing at all after it (no uid,
        // no command) is not "a process with an empty command" - see
        // `parse_ps_line`'s doc comment - it is missing a column `ps`
        // guarantees is always present, so it must fail rather than be
        // silently skipped.
        let err = parse_ps_output("12345\n").unwrap_err();
        assert!(matches!(err, DiscoveryError::MalformedPsLine { .. }));
    }

    #[test]
    fn parse_ps_output_skips_a_blank_line_without_failing() {
        // A blank line carries no process entry at all - not a violation of
        // the expected shape, just nothing to parse - so it must not fail
        // discovery over it.
        let with_blank_line = "\
65682   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude

 1760   501 claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work
";
        let parsed = parse_ps_output(with_blank_line).unwrap();
        assert_eq!(
            parsed.iter().map(|(pid, ..)| *pid).collect::<Vec<_>>(),
            vec![65682, 1760],
            "both real lines must still be parsed around the blank one"
        );
    }

    #[test]
    fn parse_ps_output_skips_a_process_with_an_empty_command_column_without_failing() {
        // pid and uid both parse; `ps` simply printed nothing in the
        // `command=` column for this process - a real, if rare, shape (see
        // `parse_ps_line`'s doc comment), not a sign the line is malformed.
        // Such a line can never be `is_claude_exe` either way, so it is
        // dropped rather than failing discovery over it.
        let empty_command = "55555   501   \n";
        let parsed = parse_ps_output(empty_command).unwrap();
        assert!(
            parsed.is_empty(),
            "a process with an empty command column must be silently dropped, not surfaced as \
             a discovery failure"
        );
    }

    #[test]
    fn parse_env_tokens_first_wins_when_a_later_value_impersonates_a_key_boundary() {
        // Reproduces the exact wipe the reviewer found against real
        // `ps -Eww` output: a two-profile bash helper exported as a
        // function, `BASH_FUNC_cw%%=() { CLAUDE_CONFIG_DIR=/opt/personal
        // claude "$@" }` (PRO-204's user stories 11 and 12's two-profile
        // shape). `BASH_FUNC_cw%%=` itself is not a valid `IDENT=` (the
        // `%%` characters fail `env_assignment_key_len`), so it folds into
        // the real `CLAUDE_CONFIG_DIR`'s value - but the
        // `CLAUDE_CONFIG_DIR=/opt/personal` token *inside* that function
        // body still looks exactly like a fresh, valid key boundary, and is
        // read as one.
        //
        // Before this fix, `parse_env_tokens` used `insert` (last-wins), so
        // this spurious second boundary silently overwrote the real
        // `CLAUDE_CONFIG_DIR`, and the real profile was never swept.
        let tokens = [
            "HOME=/opt/real-profile",
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            // A real, unrelated env var sits between the genuine
            // assignment and the exported function - the ordinary shape of
            // a real `ps -Eww` environment block - so the fold from the
            // non-key `BASH_FUNC_cw%%=()` token below lands on *this*
            // pair's value, not on the already-flushed, already-correct
            // `CLAUDE_CONFIG_DIR` entry.
            "PATH=/usr/bin:/bin",
            "BASH_FUNC_cw%%=()",
            "{",
            "CLAUDE_CONFIG_DIR=/opt/personal",
            "claude",
            "\"$@\"",
            "}",
        ];
        let env = parse_env_tokens(tokens.into_iter());
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/real-profile/.claude"),
            "the first, real CLAUDE_CONFIG_DIR= boundary must win over a later one embedded \
             inside another variable's value"
        );
    }

    // The four tests below are PRO-216 regression coverage for the reverted
    // brace-depth suppression scheme (see `parse_env_tokens`'s doc comment).
    // Each was verified, by hand, to FAIL against the brace-depth code and
    // PASS against the restored first-wins-only parser below.

    #[test]
    fn parse_env_tokens_is_not_derailed_by_a_percent_percent_equals_substring_in_an_unrelated_value()
     {
        // The brace-depth regression's simplest trigger: a token containing
        // the literal `%%=` marker with no bash function and no brace
        // imbalance involved at all. The suppression code unconditionally
        // consumed the *next* token once it saw `%%=` anywhere, so this
        // alone ate the real `CLAUDE_CONFIG_DIR=` assignment that followed
        // it. The control case immediately below (`100pct` instead of
        // `100%%`) has no `%%=` substring and must parse identically, to
        // show the fix is not accidentally about `%` or `=` in general.
        let tokens = [
            "FOO=100%%=done",
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            "HOME=/opt/real-profile",
        ];
        let env = parse_env_tokens(tokens.into_iter());
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/real-profile/.claude"),
            "a `%%=` substring in an unrelated value's own value must not eat the next token"
        );
        assert_eq!(
            env.get(HOME_VAR).map(String::as_str),
            Some("/opt/real-profile")
        );

        let control_tokens = [
            "FOO=100pct=done",
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            "HOME=/opt/real-profile",
        ];
        let control_env = parse_env_tokens(control_tokens.into_iter());
        assert_eq!(
            control_env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/real-profile/.claude"),
            "control case with no %%= substring must parse the same way"
        );
        assert_eq!(
            control_env.get(HOME_VAR).map(String::as_str),
            Some("/opt/real-profile")
        );
    }

    #[test]
    fn parse_env_tokens_is_not_derailed_by_an_unbalanced_brace_inside_a_quoted_string() {
        // An ordinary exported bash function whose body contains an
        // unbalanced literal brace inside a quoted string - e.g.
        // `echo "use { to open"` - never has a `%%=` marker at all here (the
        // function name token itself is what would carry `%%=`; this test
        // isolates the brace-counting half of the regression by starting
        // suppression already active and showing it never re-synchronises).
        // Under the brace-depth scheme this kept suppression active past
        // the function's real end, swallowing every following real
        // variable.
        let tokens = [
            "BASH_FUNC_cw%%=()",
            "{",
            "echo",
            "\"use",
            "{",
            "to",
            "open\"",
            "}",
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            "HOME=/opt/real-profile",
        ];
        let env = parse_env_tokens(tokens.into_iter());
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/real-profile/.claude"),
            "an unbalanced brace inside a quoted function body must not swallow the real \
             assignment that follows"
        );
        assert_eq!(
            env.get(HOME_VAR).map(String::as_str),
            Some("/opt/real-profile")
        );
    }

    #[test]
    fn parse_env_tokens_first_wins_over_a_genuine_bash_func_impostor_either_side_of_the_real_assignment()
     {
        // A genuine `BASH_FUNC_x%%=() { ... }` export, both after the real
        // assignment (where first-wins alone already handles it correctly -
        // see `parse_env_tokens_first_wins_when_a_later_value_impersonates_a_key_boundary`
        // above) and before it (the documented, low-severity, deliberately
        // unclosed gap: first-wins alone lets the impostor win here, and
        // that is accepted, not fixed, per this function's doc comment).
        let after = [
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            "HOME=/opt/real-profile",
            // A real, unrelated env var between the genuine assignments and
            // the exported function, exactly like
            // `parse_env_tokens_first_wins_when_a_later_value_impersonates_a_key_boundary`
            // above - so the fold from the non-key `BASH_FUNC_cw%%=()`
            // token lands on *this* pair's value, not on the
            // already-flushed, already-correct `HOME` entry.
            "PATH=/usr/bin:/bin",
            "BASH_FUNC_cw%%=()",
            "{",
            "CLAUDE_CONFIG_DIR=/opt/personal",
            "claude",
            "\"$@\"",
            "}",
        ];
        let after_env = parse_env_tokens(after.into_iter());
        assert_eq!(
            after_env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/real-profile/.claude"),
            "a genuine BASH_FUNC impostor after the real assignment must not win"
        );
        assert_eq!(
            after_env.get(HOME_VAR).map(String::as_str),
            Some("/opt/real-profile")
        );

        let before = [
            "BASH_FUNC_cw%%=()",
            "{",
            "CLAUDE_CONFIG_DIR=/opt/personal",
            "claude",
            "\"$@\"",
            "}",
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            "HOME=/opt/real-profile",
        ];
        let before_env = parse_env_tokens(before.into_iter());
        assert_eq!(
            before_env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/personal claude \"$@\" }"),
            "documented residual gap, deliberately not closed here: an impostor planted \
             ahead of the real assignment still wins under first-wins alone (its value \
             additionally absorbs every trailing non-assignment token, since nothing after \
             it flushes the entry)"
        );
    }

    #[test]
    fn discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_tmux_pane_and_home_from_one_read()
     {
        let claude_processes = build_claude_processes(
            parse_ps_output(SAMPLE_PS_OUTPUT).unwrap(),
            TEST_UID,
            &mut ForeignUidWarnings::new(),
        )
        .unwrap();

        // Non-Claude processes (ghostty, launchd) are excluded; so is the
        // foreign-uid Claude process (pid 99999, see the tests below) - the
        // full-path install, the bare name, and the `.exe`-suffixed
        // *same-uid* Claude processes are all included.
        let pids: Vec<i32> = claude_processes.iter().map(|p| p.pid).collect();
        assert_eq!(
            pids,
            vec![65682, 1760, 23195, 70773, 95255],
            "exactly the same-uid Claude-binary lines survive filtering, in source order"
        );

        let pid_65682 = claude_processes.iter().find(|p| p.pid == 65682).unwrap();
        assert_eq!(
            pid_65682.config_dir.as_deref(),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(pid_65682.tmux_pane.as_deref(), Some("%38"));
        assert_eq!(pid_65682.home.as_deref(), Some("/opt/profile-a"));

        // pid 23195 has no CLAUDE_CONFIG_DIR or TMUX_PANE in its
        // environment at all - both come back None, not an empty string -
        // but it does have HOME (PRO-211 second-round review finding 3).
        let pid_23195 = claude_processes.iter().find(|p| p.pid == 23195).unwrap();
        assert_eq!(pid_23195.config_dir, None);
        assert_eq!(pid_23195.tmux_pane, None);
        assert_eq!(pid_23195.home.as_deref(), Some("/opt/profile-a"));
    }

    // --- PRO-216: full pipeline reproductions, before and after ---
    //
    // The two tests below run the *real* `parse_ps_output` -> `is_claude_command`
    // -> `build_claude_processes` -> `union_discovery` pipeline against the
    // two shapes this ticket concretely reproduced, not just the leaf
    // `is_claude_command` function in isolation, to prove the wipe is closed
    // end to end - and, in the same breath, prove it by showing the old,
    // narrower `is_claude_exe` (still present as `is_claude_command`'s own
    // building block) never matched either shape, which is exactly what the
    // pre-PRO-216 `build_claude_processes` called on argv0 alone.

    #[test]
    fn pipeline_rescues_a_node_wrapped_claude_process_under_a_non_default_config_dir() {
        // Before PRO-216: `build_claude_processes` called `is_claude_exe` on
        // just this line's first token, "node" - which never matches, since
        // its file stem is "node", not "claude".
        assert!(
            !is_claude_exe("node"),
            "sanity: the old, narrow check misses this shape, which is exactly the bug"
        );

        // The `exec node ... cli.js` wrapper shape from the ticket: Claude
        // Code invoked as `node <path-to-cli.js>`, under a *non-default*
        // `CLAUDE_CONFIG_DIR` - the case `union_discovery`'s unconditional
        // default-profile seed does not rescue, because that seed only ever
        // points at `default_config_dir()`, never a directory only a
        // recognised process's own environment reveals.
        let ps_line = "\
44444   501 node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js CLAUDE_CONFIG_DIR=/opt/personal/.claude TMUX_PANE=%9 HOME=/opt/personal\n";

        let parsed = parse_ps_output(ps_line).unwrap();
        let claude_processes =
            build_claude_processes(parsed, TEST_UID, &mut ForeignUidWarnings::new()).unwrap();
        assert_eq!(
            claude_processes.len(),
            1,
            "the node-wrapped process must now be recognised as Claude, not filtered out"
        );

        let discovery = union_discovery(&claude_processes);
        assert!(
            discovery
                .registry_dirs
                .contains(&PathBuf::from("/opt/personal/.claude")),
            "the non-default profile's registry directory must be swept, not silently \
             dropped to only the default-profile seed: got {:?}",
            discovery.registry_dirs
        );
    }

    #[test]
    fn pipeline_rescues_a_claude_process_under_a_non_default_config_dir_whose_install_path_contains_a_space()
     {
        // Before PRO-216: `is_claude_exe` on just this line's first token,
        // "/Applications/My", never matches - the tokenizer has no way to
        // know the install path continues into the next whitespace-joined
        // token.
        assert!(
            !is_claude_exe("/Applications/My"),
            "sanity: the old, narrow check misses this shape too"
        );

        let ps_line = "\
55555   501 /Applications/My Claude App/2.1.206/claude CLAUDE_CONFIG_DIR=/opt/personal/.claude TMUX_PANE=%2 HOME=/opt/personal\n";

        let parsed = parse_ps_output(ps_line).unwrap();
        let claude_processes =
            build_claude_processes(parsed, TEST_UID, &mut ForeignUidWarnings::new()).unwrap();
        assert_eq!(
            claude_processes.len(),
            1,
            "the space-containing install path must now be recognised as Claude"
        );

        let discovery = union_discovery(&claude_processes);
        assert!(
            discovery
                .registry_dirs
                .contains(&PathBuf::from("/opt/personal/.claude")),
            "the non-default profile's registry directory must be swept: got {:?}",
            discovery.registry_dirs
        );
    }

    #[test]
    fn a_same_uid_claude_matched_process_with_no_environment_at_all_is_a_discovery_error_not_a_silent_default()
     {
        // PRO-211 review finding 4, narrowed by the second-round review's
        // finding 2: `ps -Eww` reporting zero environment tokens for a line
        // already confirmed to be a Claude process (by its invoked name)
        // *and* owned by this watcher's own uid means `ps` could not read a
        // process's environment that it ought to have been able to - not
        // that the process genuinely has no environment, and not a foreign
        // user's process it was never going to be able to read (see the
        // foreign-uid test below). Before finding 4's fix,
        // `env.get(CLAUDE_CONFIG_DIR_VAR)` on the empty map silently
        // returned `None`, which `resolve_process_config_dir` then resolved
        // to the *default* config directory - sweeping the wrong directory
        // instead of refusing to publish, and ending that profile's real
        // sessions with a successful exit.
        let claude_line_no_env = "\
55555   501 claude
";
        let parsed = parse_ps_output(claude_line_no_env).unwrap();
        assert_eq!(parsed.len(), 1, "sanity: the line parses at all");
        let err =
            build_claude_processes(parsed, TEST_UID, &mut ForeignUidWarnings::new()).unwrap_err();
        assert!(
            matches!(
                err,
                DiscoveryError::UnreadableEnvironment { pid: 55555, .. }
            ),
            "expected UnreadableEnvironment for pid 55555, got {err:?}"
        );
    }

    #[test]
    fn a_non_claude_process_with_no_environment_at_all_is_not_an_error() {
        // The counterpart to the test above: pid 1 (`/sbin/launchd`) in
        // `SAMPLE_PS_OUTPUT` also has zero environment tokens, but it is
        // filtered out by `is_claude_exe` before `build_claude_processes`'s
        // empty-env check ever runs, since a non-Claude process's
        // environment is never truth this project needs.
        let claude_processes = build_claude_processes(
            parse_ps_output(SAMPLE_PS_OUTPUT).unwrap(),
            TEST_UID,
            &mut ForeignUidWarnings::new(),
        )
        .unwrap();
        assert!(
            claude_processes.iter().all(|p| p.pid != 1),
            "pid 1 (/sbin/launchd) must be filtered out, not surfaced as an error"
        );
    }

    // --- PRO-211 second-round review finding 2: foreign-uid Claude processes ---

    #[test]
    fn a_foreign_uid_claude_matched_process_with_no_environment_is_skipped_not_a_discovery_error() {
        // Pid 99999 in SAMPLE_PS_OUTPUT: a Claude-named process owned by
        // uid 0 (root) - standing in for `sudo claude`, or any Claude
        // process on a shared host owned by a different user - with zero
        // environment tokens, the same shape `ps -Eww` reports for a
        // genuine same-uid read failure. Before this fix, this was
        // indistinguishable from that failure and turned into
        // `DiscoveryError::UnreadableEnvironment`, which - since this
        // process's environment can *never* be read by this watcher,
        // regardless of retries - became a permanent discovery failure for
        // as long as the foreign process stayed alive: every sweep,
        // `discover()` returned `Err`, and the watcher published nothing at
        // all.
        let result = build_claude_processes(
            parse_ps_output(SAMPLE_PS_OUTPUT).unwrap(),
            TEST_UID,
            &mut ForeignUidWarnings::new(),
        );
        let claude_processes = result.expect(
            "a foreign-uid Claude process with an unreadable environment must not fail \
             discovery outright",
        );
        assert!(
            claude_processes.iter().all(|p| p.pid != 99999),
            "the foreign-uid process must be excluded from the result, not silently defaulted"
        );
    }

    #[test]
    fn new_foreign_uid_pids_warns_once_then_forgets_a_pid_that_stops_appearing() {
        // Mirrors `sweep::new_orphan_pids`'s warn-once/forget behaviour
        // exactly - see its doc comment - proven directly here since this
        // pure decision function is what `build_claude_processes` and the
        // Linux enumerator both delegate to for the actual warn-once
        // bookkeeping.
        let mut warnings = ForeignUidWarnings::new();

        let first = new_foreign_uid_pids(&HashSet::from([100]), &mut warnings);
        assert_eq!(first, vec![100], "must warn the first time a pid is seen");

        let second = new_foreign_uid_pids(&HashSet::from([100]), &mut warnings);
        assert!(
            second.is_empty(),
            "must not warn again for the same pid while it stays foreign every call"
        );

        let third = new_foreign_uid_pids(&HashSet::new(), &mut warnings);
        assert!(third.is_empty(), "an empty current set warns about nothing");

        let fourth = new_foreign_uid_pids(&HashSet::from([100]), &mut warnings);
        assert_eq!(
            fourth,
            vec![100],
            "a pid forgotten because it stopped appearing (process exited, or its environment \
             became readable) must be able to warn again if it reappears, not stay suppressed \
             forever"
        );
    }

    // --- Linux `/proc/<pid>/environ` parsing (pure - fixture bytes) ---
    //
    // The impure half of Linux discovery - walking `/proc` and reading
    // these files for real - cannot be exercised on macOS, where this
    // suite runs; see the module doc comment. This is the pure half, and
    // it runs on every platform this crate builds for, proving the parser
    // itself is correct independently of ever running on real Linux.

    #[test]
    fn parse_environ_blob_extracts_config_dir_and_tmux_pane_from_one_blob() {
        let raw =
            b"HOME=/home/alice\0CLAUDE_CONFIG_DIR=/home/alice/.claude-personal\0TMUX_PANE=%3\0PATH=/usr/bin:/bin\0";
        let env = parse_environ_blob(raw);
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/home/alice/.claude-personal")
        );
        assert_eq!(env.get(TMUX_PANE_VAR).map(String::as_str), Some("%3"));
    }

    #[test]
    fn parse_environ_blob_splits_only_on_the_first_equals() {
        // A value that itself contains `=` (plausible for e.g. a base64 or
        // query-string-shaped value) must not be truncated - NUL, not `=`,
        // is the field separator here.
        let raw = b"SOME_TOKEN=abc=def=ghi\0";
        let env = parse_environ_blob(raw);
        assert_eq!(
            env.get("SOME_TOKEN").map(String::as_str),
            Some("abc=def=ghi")
        );
    }

    #[test]
    fn parse_environ_blob_ignores_empty_entries() {
        // A trailing NUL (the common real-world shape of this file) must
        // not produce a bogus empty entry.
        let raw = b"HOME=/home/alice\0\0";
        let env = parse_environ_blob(raw);
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn parse_environ_blob_empty_input_yields_empty_map() {
        assert!(parse_environ_blob(b"").is_empty());
    }

    // --- union_discovery ---

    fn cp(
        pid: i32,
        config_dir: Option<&str>,
        tmux_pane: Option<&str>,
        home: Option<&str>,
    ) -> ClaudeProcess {
        ClaudeProcess {
            pid,
            config_dir: config_dir.map(str::to_string),
            tmux_pane: tmux_pane.map(str::to_string),
            home: home.map(str::to_string),
        }
    }

    #[test]
    fn union_discovery_two_profiles_under_different_config_dirs_both_appear() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), None, None),
            cp(200, Some("/opt/profile-b/.claude-work"), None, None),
        ]);
        assert_eq!(
            discovery.registry_dirs,
            vec![
                // The default config directory is always seeded first, as
                // a floor against a total `is_claude_exe` miss - see
                // `union_discovery`'s doc comment - even though every
                // process here explicitly named its own directory.
                default_config_dir(),
                PathBuf::from("/opt/profile-a/.claude"),
                PathBuf::from("/opt/profile-b/.claude-work"),
            ]
        );
    }

    #[test]
    fn union_discovery_deduplicates_the_same_config_dir_across_processes() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), None, None),
            cp(200, Some("/opt/profile-a/.claude"), None, None),
        ]);
        assert_eq!(
            discovery.registry_dirs,
            vec![
                default_config_dir(),
                PathBuf::from("/opt/profile-a/.claude")
            ]
        );
    }

    #[test]
    fn union_discovery_defaults_unset_or_blank_config_dir_to_home_claude() {
        let discovery =
            union_discovery(&[cp(100, None, None, None), cp(200, Some("   "), None, None)]);
        // Both processes and the unconditional seed all resolve to the same
        // default directory, so it still dedupes down to exactly one entry.
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_rejects_a_relative_config_dir_and_falls_back_to_default() {
        // A relative CLAUDE_CONFIG_DIR was read from the *Claude process's*
        // environment, not the watcher's; resolving it against the
        // watcher's own working directory would sweep the wrong directory
        // entirely, silently ending that profile's sessions - see
        // `resolve_process_config_dir`'s doc comment.
        let discovery = union_discovery(&[cp(100, Some("relative/config/dir"), None, None)]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_captures_tmux_panes_keyed_by_pid() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), Some("%1"), None),
            cp(200, Some("/opt/profile-a/.claude"), None, None),
            cp(300, Some("/opt/profile-b/.claude"), Some("%9"), None),
        ]);
        assert_eq!(
            discovery.tmux_panes.get(&100).map(String::as_str),
            Some("%1")
        );
        assert_eq!(discovery.tmux_panes.get(&200), None);
        assert_eq!(
            discovery.tmux_panes.get(&300).map(String::as_str),
            Some("%9")
        );
        assert_eq!(discovery.tmux_panes.len(), 2);
    }

    #[test]
    fn union_discovery_captures_live_pids_for_every_process_regardless_of_tmux_pane() {
        // PRO-211: `live_pids` is the set discovery hands to `sweep::sweep`
        // for the orphaned-live-process warning, so it must include every
        // process's pid - not only ones running inside tmux (pid 200 has no
        // tmux pane at all, unlike `union_discovery_captures_tmux_panes_keyed_by_pid`
        // above, which only asserts on `tmux_panes`).
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), Some("%1"), None),
            cp(200, Some("/opt/profile-a/.claude"), None, None),
        ]);
        assert_eq!(discovery.live_pids, HashSet::from([100, 200]));
    }

    #[test]
    fn union_discovery_of_no_processes_still_seeds_the_default_config_dir() {
        // This is the fix for finding 2: a total `is_claude_exe` miss -
        // every real process filtered out, e.g. because Claude Code
        // presented as `node <cli>` in the process list - must not degrade
        // to an empty `Discovery`. Before this fix, `union_discovery(&[])`
        // produced `Discovery::default()` (empty `registry_dirs`), which
        // `sweep` would publish as an empty snapshot and end every session
        // on the host.
        let discovery = union_discovery(&[]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
        assert!(discovery.tmux_panes.is_empty());
        assert!(discovery.live_pids.is_empty());
    }

    // --- PRO-211 second-round review finding 3: per-process HOME, not the watcher's ---

    #[test]
    fn union_discovery_resolves_a_process_with_no_claude_config_dir_to_its_own_home_not_the_watchers()
     {
        // Before this fix, a process with no CLAUDE_CONFIG_DIR resolved to
        // `default_config_dir()`, which reads the *watcher's own* `$HOME` -
        // not the Claude process's. A watcher whose `$HOME` differs from
        // the session owner's (e.g. run under a service account, or by
        // `sudo -u`, or simply a differently configured shell) swept the
        // wrong default profile entirely, silently ending the real one with
        // a successful exit even though that process's own `HOME` was
        // sitting right there in the environment already parsed for it.
        let discovery = union_discovery(&[cp(100, None, None, Some("/home/alice"))]);
        assert!(
            discovery
                .registry_dirs
                .contains(&PathBuf::from("/home/alice/.claude")),
            "must resolve the process's own HOME, not the watcher's, got {:?}",
            discovery.registry_dirs
        );
    }

    #[test]
    fn union_discovery_falls_back_to_the_watchers_home_when_a_process_has_no_home_at_all() {
        // A process whose environment could be read but simply had no HOME
        // entry (rare, but not impossible) must not crash or silently
        // resolve to some bogus path - falling back to the watcher's own
        // default is the least-wrong available answer, and is exactly what
        // happened unconditionally before this fix.
        let discovery = union_discovery(&[cp(100, None, None, None)]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_rejects_a_relative_process_home_and_falls_back_to_the_watchers_default() {
        // A relative HOME is nonsensical to resolve (relative to what? -
        // the watcher's cwd would be arbitrary and wrong) - this must fall
        // back to the watcher's own default rather than silently joining
        // ".claude" onto a relative path and resolving it against whatever
        // the watcher's current directory happens to be.
        let discovery = union_discovery(&[cp(100, None, None, Some("relative/home"))]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    // --- ProcessCache (PRO-217) ---
    //
    // Mirrors `git::GitCache`'s own test suite in shape (reuse-within-TTL,
    // refetch-after-expiry), plus the failure-semantics tests that are this
    // cache's whole reason for extra care: a cached *enumeration* feeds
    // whether sessions get published (via `registry_dirs`), unlike a cached
    // git lookup which only feeds a cosmetic label, so a failed refresh must
    // never be cached, silently degrade to empty, or fall back to serving a
    // stale value in place of a real failure.

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn one_process(pid: i32) -> Vec<ClaudeProcess> {
        vec![cp(pid, Some("/opt/profile-a/.claude"), None, None)]
    }

    #[test]
    fn process_cache_reuses_a_fetch_within_ttl() {
        let cache = ProcessCache::new(Duration::from_secs(60));
        let counter = AtomicUsize::new(0);
        let fetch = || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(one_process(100))
        };

        let first = cache.get_or_fetch(fetch).unwrap();
        let second = cache.get_or_fetch(fetch).unwrap();

        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            1,
            "the second call within the TTL must not re-run the real enumeration"
        );
        assert_eq!(first, second);
    }

    #[test]
    fn process_cache_refetches_once_the_ttl_expires() {
        let cache = ProcessCache::new(Duration::from_millis(20));
        let counter = AtomicUsize::new(0);
        let fetch = || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(one_process(100))
        };

        cache.get_or_fetch(fetch).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        cache.get_or_fetch(fetch).unwrap();

        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            2,
            "a lookup after the TTL has elapsed must re-run the real enumeration - this is what \
             bounds how stale a new profile or a moved tmux pane can ever be"
        );
    }

    #[test]
    fn process_cache_never_caches_a_failed_fetch_and_retries_on_the_very_next_call() {
        // A failed fetch must not be remembered as "the cached result" for
        // the rest of the TTL: the next call - even immediately after, well
        // within what would otherwise be a fresh window - must attempt a
        // real enumeration again, exactly as an uncached call always would.
        let cache = ProcessCache::new(Duration::from_secs(60));
        let counter = AtomicUsize::new(0);

        let first = cache.get_or_fetch(|| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Err(DiscoveryError::EmptyProcessList)
        });
        assert!(first.is_err(), "a fetch failure must propagate as Err");

        let second = cache.get_or_fetch(|| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(one_process(100))
        });
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            2,
            "the call after a failure must retry the real enumeration immediately, not wait out \
             the TTL as if the failure had been cached"
        );
        assert!(second.is_ok());
    }

    #[test]
    fn process_cache_propagates_a_fetch_error_rather_than_degrading_to_an_empty_success() {
        // The exact failure-mode PRO-217's constraint names explicitly: a
        // refresh that fails must remain a discovery failure, never quietly
        // become an empty-but-successful enumeration (which `union_discovery`
        // would otherwise turn into "no extra profiles" - indistinguishable
        // from a genuinely idle host).
        let cache = ProcessCache::new(Duration::from_secs(60));
        let result =
            cache.get_or_fetch(|| Err(DiscoveryError::Enumerate(std::io::Error::other("boom"))));
        assert!(
            matches!(result, Err(DiscoveryError::Enumerate(_))),
            "must surface the real error, not Ok(vec![]), got {result:?}"
        );
    }

    #[test]
    fn process_cache_serves_the_warm_cache_through_a_transient_refresh_failure() {
        // The other side of the same coin: while the cache is still fresh,
        // a fetch that *would* fail must never even be attempted - a
        // currently-published profile's directory must not be able to drop
        // out of the set over a transient `ps` hiccup that happens to land
        // inside the TTL window.
        let cache = ProcessCache::new(Duration::from_secs(60));
        let counter = AtomicUsize::new(0);

        let warm = cache.get_or_fetch(|| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(one_process(100))
        });
        assert!(warm.is_ok());

        let served_from_cache = cache.get_or_fetch(|| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            panic!("must not be called - the fresh cached value must be served instead")
        });

        assert_eq!(served_from_cache.unwrap(), one_process(100));
        assert_eq!(counter.load(AtomicOrdering::SeqCst), 1);
    }
}
