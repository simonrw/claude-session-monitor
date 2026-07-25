//! The one-shot sweep: read every configured registry directory, filter to
//! live interactive sessions, and translate them into the watcher's publish
//! shape (`common::api::SnapshotSession`).
//!
//! `sweep` is a plain function, not a loop: PRO-210's daemon adds polling
//! and cross-sweep debouncing on top of it, calling it repeatedly, without
//! this crate needing to know anything about intervals.

use std::path::PathBuf;

use common::api::SnapshotSession;

use crate::registry::{is_live, read_entries};
use crate::status::map_status;

/// Environment variable naming the registry directories to sweep, as a
/// PATH-style list (`:`-delimited on Unix).
///
/// This is an explicit, permanent override, not scaffolding: automatic
/// discovery of registry directories from running Claude Code process
/// environments arrives in PRO-208, but this variable remains a real escape
/// hatch - and the seam integration tests use - even once that lands.
pub const REGISTRY_DIRS_ENV: &str = "CSM_WATCHER_REGISTRY_DIRS";

/// Read [`REGISTRY_DIRS_ENV`]. Returns an empty list if it is unset - a
/// safe default until PRO-208 adds automatic discovery.
///
/// Components that name no directory - empty or entirely whitespace - are
/// dropped. `split_paths` yields one empty `PathBuf` for an empty value
/// rather than yielding nothing, so without this filter
/// `CSM_WATCHER_REGISTRY_DIRS=""` would look like one configured directory,
/// slip past the caller's "no directories configured" refusal, resolve
/// `sessions/` relative to the process's working directory, find nothing,
/// and publish an empty snapshot that ends every session on the host. A
/// blank value is reachable from a launchd or systemd unit, from
/// `export VAR=""`, and from `VAR="$SOMETHING_UNSET"`, so it must land on
/// the same side as unset. Whitespace-only components wipe the host the
/// same way and no real directory is named by whitespace, so they are
/// refused too.
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

/// Sweep every directory in `registry_dirs` and return the live interactive
/// sessions found, translated into the watcher's publish shape.
///
/// Every per-directory/per-file concern (a missing directory, malformed
/// files, dead or reused pids, non-interactive entries) is handled inside
/// `registry::read_entries`/`registry::is_live`; this function only
/// aggregates their results and maps them to `SnapshotSession`.
///
/// `git_branch`, `git_remote`, `tmux_target`, and `model` have no
/// equivalent in the registry, so they are always `None` here; they remain
/// populated only via the (soon to be retired) hook-reporting path.
pub fn sweep(registry_dirs: &[PathBuf]) -> Vec<SnapshotSession> {
    registry_dirs
        .iter()
        .flat_map(|dir| match read_entries(dir) {
            Ok(entries) => entries,
            // PRO-211 owns the policy for a failed sweep ("a failed sweep
            // publishes nothing"); implementing that here is out of scope
            // for this slice. For now this preserves the prior behaviour
            // exactly - log and contribute no entries from this directory
            // - but via the explicit `ReadDirError` signal rather than a
            // silent empty `Vec`, so PRO-211 has something to match on and
            // propagate as a sweep-wide failure.
            Err(e) => {
                tracing::warn!(
                    dir = %e.dir.display(),
                    error = %e.source,
                    "failed to read registry directory, treating as empty"
                );
                Vec::new()
            }
        })
        .filter(is_live)
        .map(|entry| SnapshotSession {
            session_id: entry.session_id,
            cwd: entry.cwd,
            status: map_status(&entry.status, entry.waiting_for.as_deref()),
            name: entry.name,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
            model: None,
        })
        .collect()
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
}
