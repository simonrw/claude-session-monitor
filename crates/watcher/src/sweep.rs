//! The one-shot sweep: read every configured registry directory, filter to
//! live interactive sessions, and translate them into the watcher's publish
//! shape (`common::api::SnapshotSession`).
//!
//! `sweep` is a plain function, not a loop: PRO-210's daemon (`main.rs`'s
//! `run_daemon`) calls it repeatedly on an interval, without this crate
//! needing to know anything about intervals. Cross-sweep debouncing (a
//! session must be absent from two consecutive *successful* sweeps before it
//! is omitted) is separate again and not added by PRO-210 - that is
//! PRO-211's `debounce` module, which builds directly on the daemon loop and
//! consumes this function's `Ok` result. This function's job stops at "one
//! sweep, honestly" - it fails the whole sweep loudly (see [`SweepError`])
//! rather than ever guessing, and leaves cross-sweep judgment calls to the
//! caller.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::api::SnapshotSession;
use common::session::Status;

use crate::git::GitCache;
use crate::registry::{RegistryEntry, is_live, read_entries};
use crate::tmux;

/// Environment variable naming the registry directories to sweep, as a
/// PATH-style list (`:`-delimited on Unix).
///
/// This is an explicit, permanent override, not scaffolding: PRO-208 added
/// automatic discovery of registry directories from running Claude Code
/// process environments (see `crate::discovery`), but this variable remains
/// a real escape hatch - and the seam integration tests use - on top of it.
/// The caller (`main.rs`) treats an empty result from
/// [`registry_dirs_from_env`] as "no override configured" and falls back to
/// `discovery::discover`; any non-empty result bypasses discovery entirely.
pub const REGISTRY_DIRS_ENV: &str = "CSM_WATCHER_REGISTRY_DIRS";

/// Read [`REGISTRY_DIRS_ENV`]. Returns an empty list if it is unset - the
/// signal callers use to fall back to automatic discovery instead (see
/// above).
///
/// Components that name no directory - empty or entirely whitespace - are
/// dropped. `split_paths` yields one empty `PathBuf` for an empty value
/// rather than yielding nothing, so without this filter
/// `CSM_WATCHER_REGISTRY_DIRS=""` would look like one configured directory,
/// slip past the caller's override-vs-discovery branch, resolve `sessions/`
/// relative to the process's working directory, find nothing, and publish
/// an empty snapshot that ends every session on the host. A blank value is
/// reachable from a launchd or systemd unit, from `export VAR=""`, and from
/// `VAR="$SOMETHING_UNSET"`, so it must land on the same side as unset -
/// which now means "fall back to discovery", not "refuse to publish": an
/// unset or blank override is a normal, self-healing state, not a
/// misconfiguration. Whitespace-only components are refused the same way,
/// since no real directory is named by whitespace.
///
/// This is distinct from a configured directory that simply does not exist
/// yet, which stays a successful empty sweep per PRO-207.
pub fn registry_dirs_from_env() -> Vec<PathBuf> {
    match std::env::var_os(REGISTRY_DIRS_ENV) {
        Some(val) => std::env::split_paths(&val)
            .filter(|p| !names_no_directory(p))
            .collect(),
        None => Vec::new(),
    }
}

/// Whether a path component carries no directory at all: empty, or nothing
/// but whitespace.
fn names_no_directory(path: &std::path::Path) -> bool {
    match path.to_str() {
        Some(s) => s.trim().is_empty(),
        // Not UTF-8, so it cannot be all-whitespace; only an empty OsStr
        // names nothing.
        None => path.as_os_str().is_empty(),
    }
}

/// A sweep could not be completed honestly: either a discovered registry
/// directory exists but could not be opened or listed, or an individual
/// registry file exists but could not be read, or process discovery itself
/// could not determine a confirmed Claude process's environment.
///
/// This is deliberately whole-sweep, not per-directory or per-file: `sweep`
/// stops at the first such error rather than continuing past it, because the
/// caller's policy (PRO-211, `main.rs`'s `run_cycle`) is "a failed sweep
/// publishes nothing at all" - a snapshot assembled from only the
/// directories/files that happened to succeed would be exactly the short,
/// silently-incomplete result this exists to prevent. This is distinct from
/// a directory that simply does not exist yet, which `registry::
/// read_entries` already treats as a successful empty read (see its own doc
/// comment), and from a malformed/empty registry *file*, which stays a
/// lenient per-file warn-and-skip (see `registry::ReadError`'s doc comment)
/// - only genuine I/O failures reach here.
#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    #[error("failed to read registry directory {}: {source}", dir.display())]
    Directory {
        dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read registry file {}: {source}", path.display())]
    File {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<crate::registry::ReadError> for SweepError {
    fn from(e: crate::registry::ReadError) -> Self {
        match e {
            crate::registry::ReadError::Dir { dir, source } => Self::Directory { dir, source },
            crate::registry::ReadError::File { path, source } => Self::File { path, source },
        }
    }
}

/// Sweep every directory in `registry_dirs` and return the live interactive
/// sessions found, translated into the watcher's publish shape.
///
/// Most per-directory/per-file concerns (a missing directory, malformed or
/// empty JSON, dead or reused pids, non-interactive entries) are handled
/// leniently inside `registry::read_entries`/`registry::is_live`; this
/// function only aggregates their results and maps them to
/// `SnapshotSession`. The exceptions - and the reason this returns `Result`
/// at all - are genuine I/O failures: a registry directory that exists but
/// could not be opened or iterated, or an individual registry file that
/// exists but could not be read (`registry::ReadError`, both variants); each
/// is propagated as [`SweepError`] rather than silently treated as empty or
/// partial, so the caller can refuse to publish rather than end every
/// session that happened to live in the directory or file that failed to
/// read. See [`SweepError`]'s doc comment for why this stops at the first
/// such error instead of continuing past it.
///
/// `model` has no equivalent in the registry, so it is always `None` here;
/// it remains populated only via the (soon to be retired) hook-reporting
/// path. `git_branch`, `git_remote`, and `tmux_target` are enriched here
/// (PRO-209): `tmux_panes` (pid -> `TMUX_PANE`, from `discovery::discover`)
/// is joined against exactly one `tmux list-panes -a` listing for the whole
/// sweep, and `git_cache` derives and caches branch/remote by `cwd`. Neither
/// enrichment can fail this function - see `tmux` and `git`'s module doc
/// comments for how each degrades to "no enrichment" instead.
///
/// `live_pids` (PRO-211, from `discovery::Discovery::live_pids` /
/// `discovery::ProcessSnapshot::live_pids`) is the set of pids the same
/// process enumeration found running a Claude binary. After every directory
/// has been read successfully, any pid in `live_pids` with no corresponding
/// entry anywhere in the registry - not just no *live* entry; a stale or
/// non-interactive entry for that pid still counts as "the registry knows
/// about it" - is logged at `warn`: a live Claude process the registry
/// itself has no record of at all is exactly the kind of silent gap PRO-211
/// exists to surface rather than pass over. This check only runs once every
/// directory has read successfully - a partial entry set from a sweep that
/// is about to fail outright (and publish nothing) would produce false
/// positives for every pid the unread directory would otherwise have
/// accounted for.
///
/// `orphan_warnings` is `None` under the `CSM_WATCHER_REGISTRY_DIRS`
/// override and `Some` otherwise (`main.rs` decides which). Under the
/// override, `live_pids` still comes from a whole-host process enumeration
/// (see `discovery::discover_process_snapshot`) while `registry_dirs` - and
/// therefore every entry this sweep can possibly see - is scoped to only
/// the override's own directories, so every real session on the host outside
/// those directories would compare as an "orphan": a pure false positive,
/// not a real gap, reproduced directly (every live Claude process on the
/// machine warned about, every sweep, under a two-directory override).
/// `None` skips the comparison entirely rather than producing it. When
/// `Some`, each pid is only ever warned about once - not once per sweep, see
/// [`OrphanWarnings`] - since the pre-fix unconditional per-sweep check was
/// itself reproduced at 72 warnings in 20 seconds for 3 real orphaned
/// processes (roughly 43,000 identical lines/day at the default interval),
/// which trains a reader to ignore the log rather than act on it.
pub fn sweep(
    registry_dirs: &[PathBuf],
    tmux_panes: &HashMap<i32, String>,
    git_cache: &GitCache,
    live_pids: &HashSet<i32>,
    orphan_warnings: Option<&mut OrphanWarnings>,
) -> Result<Vec<SnapshotSession>, SweepError> {
    // One tmux listing for the entire sweep, regardless of session count -
    // see `tmux::resolve_all_panes`'s doc comment. Skipped entirely when
    // `tmux_panes` is empty: with nothing to join a listing against, the
    // invocation could only ever contribute an unused map, so there is no
    // reason to pay for it (or wait out `tmux::LIST_PANES_TIMEOUT` if tmux
    // happens to be hung). `tmux_panes` legitimately is empty on more than
    // one path - not just "no processes are running under tmux" but also
    // `discovery::discover_process_snapshot`'s own enumeration failing
    // (finding 3 from the PRO-209 review) - so this guard stays meaningful
    // rather than becoming dead code after that fix.
    let pane_targets = if tmux_panes.is_empty() {
        HashMap::new()
    } else {
        tmux::resolve_all_panes()
    };

    let mut entries: Vec<RegistryEntry> = Vec::new();
    for dir in registry_dirs {
        entries.extend(read_entries(dir)?);
    }

    if let Some(warnings) = orphan_warnings {
        for pid in new_orphan_pids(&entries, live_pids, warnings) {
            tracing::warn!(
                pid,
                "live Claude process has no corresponding registry entry"
            );
        }
    }

    Ok(entries
        .into_iter()
        .filter(is_live)
        .map(|entry| {
            let tmux_target = tmux::resolve_target(entry.pid, tmux_panes, &pane_targets);
            let git_info = git_cache.get(&entry.cwd);
            if !is_known_registry_status(&entry.status) {
                // See `Status::from_registry`'s doc comment: the registry's
                // `status` enum is undocumented and owned by Claude Code, so
                // it can change without warning. Falling back to `Idle` there
                // is deliberate (the weakest possible claim), but PRO-204
                // user story 31 still requires the degradation to be visible,
                // not silent - an unrecognized value renamed by a Claude Code
                // upgrade would otherwise make every affected session quietly
                // stop counting as busy or waiting with no signal anywhere
                // that anything changed. This warning is that signal.
                tracing::warn!(
                    session_id = %entry.session_id,
                    status = %entry.status,
                    "unrecognized registry status; treating session as idle"
                );
            }
            SnapshotSession {
                session_id: entry.session_id,
                cwd: entry.cwd,
                status: Status::from_registry(&entry.status, entry.waiting_for.as_deref()),
                name: entry.name,
                git_branch: git_info.branch,
                git_remote: git_info.remote,
                tmux_target,
                model: None,
            }
        })
        .collect())
}

/// Whether `status` is one of the registry's own values that
/// `Status::from_registry` maps directly, as opposed to a value it has never
/// seen and therefore degrades to `Idle`. Pure and unit-tested directly
/// below, matching this module's existing pattern (`registry_dirs_from_env`,
/// `new_orphan_pids`) of testing the decision a log statement is driven by
/// rather than the exact text of the log line.
fn is_known_registry_status(status: &str) -> bool {
    matches!(status, "busy" | "shell" | "idle" | "waiting")
}

/// Cross-sweep memory of which live-but-unregistered pids have already
/// produced the "live Claude process has no corresponding registry entry"
/// warning (PRO-211 review finding 3), so a given pid warns exactly once
/// rather than on every sweep it remains orphaned. Before this existed the
/// check re-ran unconditionally every sweep - reproduced directly at 72
/// warnings in 20 seconds for 3 real orphaned processes, roughly 43,000
/// identical lines/day at the default 2s interval - and a warning that
/// fires that constantly is worse than no warning at all, since it trains a
/// reader to ignore the log.
///
/// Lives here for the same reason [`crate::debounce::Debounce`] lives in its
/// own module rather than inside `sweep`: this is cross-sweep state that a
/// stateless `sweep` cannot hold itself (see `sweep`'s own module doc
/// comment), so `main.rs` owns one for the life of the daemon, alongside its
/// `Debounce`, and threads it through every call to [`sweep`].
///
/// A pid that stops being live is forgotten on the next call that observes
/// it gone (see [`new_orphan_pids`]), so a later, unrelated process that
/// reuses the same pid and is itself orphaned warns again in its own right,
/// rather than being silently suppressed by a stale entry for a different,
/// long-gone process.
#[derive(Debug, Default)]
pub struct OrphanWarnings {
    warned: HashSet<i32>,
}

impl OrphanWarnings {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The subset of `live_pids` with no corresponding entry anywhere in
/// `entries` - see `sweep`'s doc comment for why this compares against
/// every entry the registry has, not only live/interactive ones - that have
/// not already been recorded in `warned`. Every pid returned is recorded
/// into `warned` before returning, so a second call with the same pid still
/// live and still orphaned returns nothing for it (see [`OrphanWarnings`]).
///
/// Pure and unit-tested directly below (PRO-211 review finding 6) rather
/// than through captured log output, matching this crate's existing
/// pattern (`registry_dirs_from_env`, `discovery::union_discovery`) of
/// testing the decision a log statement is driven by, not the exact text of
/// the log line itself.
fn new_orphan_pids(
    entries: &[RegistryEntry],
    live_pids: &HashSet<i32>,
    warned: &mut OrphanWarnings,
) -> Vec<i32> {
    // Forget any previously-warned pid that is no longer live, so a reused
    // pid is judged fresh rather than inheriting a stale warning state - see
    // `OrphanWarnings`'s doc comment.
    warned.warned.retain(|pid| live_pids.contains(pid));

    let registry_pids: HashSet<i32> = entries.iter().map(|e| e.pid).collect();
    let mut new_orphans: Vec<i32> = live_pids
        .iter()
        .filter(|pid| !registry_pids.contains(pid))
        .copied()
        .filter(|pid| warned.warned.insert(*pid))
        .collect();
    new_orphans.sort_unstable();
    new_orphans
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Both tests below mutate the process-global `REGISTRY_DIRS_ENV`
    // variable; Rust runs tests in the same binary concurrently by default,
    // so they're serialized on this lock to avoid racing each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn registry_dirs_from_env_empty_when_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation is serialized via ENV_LOCK above.
        unsafe { std::env::remove_var(REGISTRY_DIRS_ENV) };
        assert!(registry_dirs_from_env().is_empty());
    }

    #[test]
    fn registry_dirs_from_env_empty_when_set_but_blank() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation is serialized via ENV_LOCK above.
        unsafe { std::env::set_var(REGISTRY_DIRS_ENV, "") };
        let dirs = registry_dirs_from_env();
        unsafe { std::env::remove_var(REGISTRY_DIRS_ENV) };
        assert!(dirs.is_empty());
    }

    #[test]
    fn registry_dirs_from_env_empty_when_set_to_whitespace() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation is serialized via ENV_LOCK above.
        unsafe { std::env::set_var(REGISTRY_DIRS_ENV, "   ") };
        let dirs = registry_dirs_from_env();
        unsafe { std::env::remove_var(REGISTRY_DIRS_ENV) };
        assert!(dirs.is_empty());
    }

    #[test]
    fn registry_dirs_from_env_drops_empty_components() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation is serialized via ENV_LOCK above.
        unsafe { std::env::set_var(REGISTRY_DIRS_ENV, ":/tmp/a::/tmp/b:") };
        let dirs = registry_dirs_from_env();
        unsafe { std::env::remove_var(REGISTRY_DIRS_ENV) };
        assert_eq!(dirs, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
    }

    #[test]
    fn registry_dirs_from_env_splits_path_style_list() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation is serialized via ENV_LOCK above.
        unsafe { std::env::set_var(REGISTRY_DIRS_ENV, "/tmp/a:/tmp/b") };
        let dirs = registry_dirs_from_env();
        unsafe { std::env::remove_var(REGISTRY_DIRS_ENV) };
        assert_eq!(dirs, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
    }

    // A missing registry directory yielding a successful empty sweep, and
    // entries from multiple registry directories aggregating into one
    // snapshot, are both covered end to end by
    // `watcher_treats_missing_registry_directory_as_empty_not_an_error` and
    // `watcher_aggregates_sessions_from_multiple_registry_directories` in
    // `crates/server/tests/reconciliation.rs`. Per PRO-204's testing
    // decisions this crate must not duplicate that coverage as unit tests
    // against `sweep`'s own internals.

    // --- new_orphan_pids / OrphanWarnings (PRO-211 review finding 6) ---

    fn entry_with_pid(pid: i32) -> RegistryEntry {
        RegistryEntry {
            session_id: format!("s{pid}"),
            pid,
            proc_start: "Fri Jul 24 20:55:59 2026".into(),
            kind: "interactive".into(),
            status: "busy".into(),
            waiting_for: None,
            cwd: "/tmp".into(),
            name: None,
        }
    }

    #[test]
    fn a_live_pid_absent_from_the_registry_produces_a_warning() {
        let mut warned = OrphanWarnings::new();
        let live_pids = HashSet::from([100]);
        let orphans = new_orphan_pids(&[], &live_pids, &mut warned);
        assert_eq!(
            orphans,
            vec![100],
            "the unregistered live pid must be reported"
        );
    }

    #[test]
    fn a_live_pid_present_in_the_registry_is_never_reported() {
        let mut warned = OrphanWarnings::new();
        let live_pids = HashSet::from([100]);
        let entries = [entry_with_pid(100)];
        let orphans = new_orphan_pids(&entries, &live_pids, &mut warned);
        assert!(
            orphans.is_empty(),
            "a pid the registry has an entry for is not an orphan, even though `is_live` never \
             runs here - see `sweep`'s doc comment for why this checks every entry, not only \
             live/interactive ones"
        );
    }

    #[test]
    fn a_pid_is_only_ever_reported_once() {
        let mut warned = OrphanWarnings::new();
        let live_pids = HashSet::from([100]);

        let first = new_orphan_pids(&[], &live_pids, &mut warned);
        assert_eq!(first, vec![100], "the first sweep must still warn");

        let second = new_orphan_pids(&[], &live_pids, &mut warned);
        assert!(
            second.is_empty(),
            "the same pid, still orphaned, must not be reported a second time - reproduces \
             the pre-fix storm (72 warnings in 20 seconds for 3 real processes)"
        );

        let third = new_orphan_pids(&[], &live_pids, &mut warned);
        assert!(third.is_empty(), "and not a third time either");
    }

    #[test]
    fn a_pid_that_stops_being_live_can_warn_again_if_reused() {
        let mut warned = OrphanWarnings::new();

        let still_live = HashSet::from([100]);
        let first = new_orphan_pids(&[], &still_live, &mut warned);
        assert_eq!(first, vec![100]);

        // pid 100 disappears from `live_pids` entirely (the process exited).
        let gone = HashSet::new();
        let while_gone = new_orphan_pids(&[], &gone, &mut warned);
        assert!(while_gone.is_empty());

        // The OS reuses pid 100 for a new, unrelated orphaned process.
        let reused = HashSet::from([100]);
        let after_reuse = new_orphan_pids(&[], &reused, &mut warned);
        assert_eq!(
            after_reuse,
            vec![100],
            "a reused pid must be free to warn again, not be permanently suppressed by a \
             long-gone process's stale warning state"
        );
    }

    // --- is_known_registry_status (PRO-214 review finding 4) ---

    #[test]
    fn known_registry_statuses_are_recognized() {
        for status in ["busy", "shell", "idle", "waiting"] {
            assert!(is_known_registry_status(status), "{status} should be known");
        }
    }

    #[test]
    fn unrecognized_registry_status_is_not_known() {
        assert!(!is_known_registry_status("some-future-status"));
        assert!(!is_known_registry_status(""));
        assert!(!is_known_registry_status("Busy"));
    }

    #[test]
    fn multiple_orphaned_pids_are_all_reported_independently() {
        let mut warned = OrphanWarnings::new();
        let live_pids = HashSet::from([100, 200, 300]);
        let entries = [entry_with_pid(200)]; // only 200 is registered
        let orphans = new_orphan_pids(&entries, &live_pids, &mut warned);
        assert_eq!(orphans, vec![100, 300]);
    }
}
