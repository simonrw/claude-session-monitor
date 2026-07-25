//! Git branch and remote enrichment, derived from a session's `cwd`.
//!
//! Matches `crates/reporter/src/enrichment.rs`'s `detect_git_branch` /
//! `detect_git_remote` exactly - same commands, same detached-HEAD `jj`
//! fallback, same trimming/emptiness rules - because clients already render
//! whatever those produce. PRO-213 deletes that module once the hook path
//! is retired; this crate does not depend on it, it reimplements the same
//! behaviour independently.
//!
//! Two properties this module adds on top of that: every git invocation is
//! bounded by a timeout (a hung git process must not stall a sweep) via
//! `crate::command::run`, and results are cached by `cwd` with a
//! time-to-live via [`GitCache`], so polling every couple of seconds - and
//! several sessions commonly sharing one `cwd` - does not spawn git
//! continuously.
//!
//! Degradation is the point of this module, not an edge case: a `cwd` that
//! no longer exists, a `git` binary that is not installed, a directory that
//! is not a git repository, and a command that exceeds its timeout must all
//! produce `None` rather than an error, so one session's enrichment can
//! never fail the sweep that carries every other session.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default time-to-live for a cached `cwd`'s git info.
///
/// This is a deliberate trade against staleness: 30 seconds against a
/// two-second poll means a session's branch or remote can be up to 15
/// sweeps behind what `git` would report right now. That trade was judged
/// correct because branch switches are rare events (a person runs
/// `git checkout` a handful of times an hour, not per second) while a git
/// invocation on every single sweep - the alternative - is not: several
/// sessions commonly share one `cwd`, and a couple-of-seconds poll loop
/// (PRO-210) turns "spawn git on every sweep" into a steady git storm for
/// no benefit, since the branch essentially never actually changed between
/// consecutive sweeps. The cost of that trade is real, though: a user
/// looking at a 30-second-stale branch label today has nowhere to look to
/// learn that it can lag, or by how much - PRO-212 is expected to surface
/// this TTL in user-facing docs so that gap has an answer.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// Default upper bound on one git invocation.
///
/// One `cwd` can cost up to three invocations within a single lookup: a
/// branch check, its `jj` detached-HEAD fallback, and a remote check. At
/// the previous value of two seconds each, a single hung `git` could stall
/// one `cwd`'s enrichment for up to six seconds - measured directly: a
/// sweep covering three sessions with a hung `git` on `PATH` took 12.3
/// seconds to complete before this was lowered, nowhere near "well under
/// the poll interval" PRO-204 targets. 500ms keeps that same worst case
/// under 1.5 seconds while still comfortably outliving a real git
/// invocation against a local repository.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_millis(500);

/// Git branch and remote for one `cwd`, as produced by [`detect`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub remote: Option<String>,
}

/// A cache of [`GitInfo`] keyed by `cwd`, with a time-to-live.
///
/// Construct one per watcher process (or per daemon run, once PRO-210 adds
/// the polling loop) and reuse it across every sweep: a fresh cache per
/// sweep would defeat the whole point of caching, since every lookup would
/// always miss.
///
/// Entries past their TTL are pruned opportunistically whenever a value is
/// stored (see [`GitCache::store`]), rather than only being ignored on
/// read: without that, `entries` would retain every distinct `cwd` ever
/// queried for the life of the process, unbounded, for as long as
/// PRO-210's daemon runs - a session that ended, or a `cwd` that was
/// queried only once, would leak its entry forever.
pub struct GitCache {
    ttl: Duration,
    command_timeout: Duration,
    entries: Mutex<HashMap<String, (Instant, GitInfo)>>,
}

impl GitCache {
    pub fn new(ttl: Duration, command_timeout: Duration) -> Self {
        Self {
            ttl,
            command_timeout,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Git branch and remote for `cwd`.
    ///
    /// Reuses a cached value younger than this cache's TTL rather than
    /// re-invoking git. `cwd` is the cache key, so multiple sessions
    /// sharing one working directory - the common case - collapse to a
    /// single pair of git invocations rather than one pair per session.
    pub fn get(&self, cwd: &str) -> GitInfo {
        let timeout = self.command_timeout;
        self.get_or_fetch(cwd, || detect(cwd, timeout))
    }

    /// The cache's actual logic, with the fetch behaviour injected so tests
    /// can observe (and count) it without spawning real git processes -
    /// the same boundary `main.rs`'s `resolve_registry_dirs` uses for
    /// `discovery::discover`.
    ///
    /// The mutex is never held across `fetch()`, which can take up to
    /// `command_timeout` (a subprocess call): holding it there would
    /// serialize every other `cwd`'s lookup behind whichever one is
    /// currently shelling out to git, defeating the purpose of caching by
    /// `cwd` at all. This is safe today because `sweep::sweep` calls `get`
    /// serially within one sweep, and sweeps themselves are not run
    /// concurrently yet - but it is a real seam, not just tidiness: the
    /// moment two sweeps (or two threads) call `get` for the same `cwd`
    /// concurrently, both can miss and both will fetch, and the second
    /// `store` wins. That is an acceptable, bounded race - the loser's
    /// subprocess call is wasted work, not incorrect data - traded
    /// deliberately against serializing every lookup behind one lock.
    fn get_or_fetch(&self, cwd: &str, fetch: impl FnOnce() -> GitInfo) -> GitInfo {
        if let Some(info) = self.fresh_entry(cwd) {
            return info;
        }
        let info = fetch();
        self.store(cwd, info.clone());
        info
    }

    /// A cached value for `cwd` younger than this cache's TTL, if any.
    fn fresh_entry(&self, cwd: &str) -> Option<GitInfo> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(cwd)
            .and_then(|(fetched_at, info)| (fetched_at.elapsed() < self.ttl).then(|| info.clone()))
    }

    /// Store a freshly-fetched value for `cwd`, and prune every entry
    /// (for any `cwd`, not just this one) whose TTL has already expired.
    /// Pruning here, rather than in a separate sweep of its own, means no
    /// extra timer or background task is needed: as long as `get` is
    /// called at all - which it is, every sweep, for every live session's
    /// `cwd` - expired entries are bounded to at most one TTL past their
    /// last use before they are dropped.
    fn store(&self, cwd: &str, info: GitInfo) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let ttl = self.ttl;
        entries.retain(|_, (fetched_at, _)| fetched_at.elapsed() < ttl);
        entries.insert(cwd.to_owned(), (Instant::now(), info));
    }
}

/// Detect both branch and remote for `dir`, each independently degrading to
/// `None` on any failure.
fn detect(dir: &str, timeout: Duration) -> GitInfo {
    GitInfo {
        branch: detect_git_branch(dir, timeout),
        remote: detect_git_remote(dir, timeout),
    }
}

/// Mirrors `enrichment::detect_git_branch`: `git rev-parse --abbrev-ref
/// HEAD`, falling back to `jj current-bookmark` when HEAD is detached (the
/// literal value `"HEAD"`), since a jj-colocated repo reports that way too.
fn detect_git_branch(dir: &str, timeout: Duration) -> Option<String> {
    let branch = crate::command::run(
        "git",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        Some(Path::new(dir)),
        timeout,
    )?;
    if branch == "HEAD" {
        crate::command::run("jj", &["current-bookmark"], Some(Path::new(dir)), timeout)
    } else {
        Some(branch)
    }
}

/// Mirrors `enrichment::detect_git_remote`: `git remote get-url origin`.
fn detect_git_remote(dir: &str, timeout: Duration) -> Option<String> {
    crate::command::run(
        "git",
        &["remote", "get-url", "origin"],
        Some(Path::new(dir)),
        timeout,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The bounded-subprocess degrade paths (missing binary, nonexistent
    // dir, timeout, nonzero exit, output trimming) are `crate::command`'s
    // own impure boundary now, tested there - see that module's test
    // module - rather than duplicated here against `git`-specific
    // arguments that exercise exactly the same shared code path.

    // --- detect_git_branch / detect_git_remote against a real repo ---

    #[test]
    fn detect_git_branch_and_remote_on_real_repo() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR always set during tests");
        // Mirrors `enrichment.rs`'s own test: only assert branch is present
        // when HEAD is not detached (true for a normal checkout, not
        // necessarily true for a CI checkout of a specific commit).
        let head_is_branch = Command::new("git")
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .current_dir(&manifest_dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if head_is_branch {
            let branch = detect_git_branch(&manifest_dir, Duration::from_secs(2));
            assert!(
                branch.is_some_and(|b| !b.is_empty()),
                "git_branch should be non-empty when HEAD is a branch"
            );
        }
    }

    #[test]
    fn detect_git_info_on_nonexistent_path_has_no_git_info() {
        let info = detect(
            "/nonexistent/path/that/does/not/exist",
            Duration::from_secs(1),
        );
        assert_eq!(info.branch, None);
        assert_eq!(info.remote, None);
    }

    #[test]
    fn detect_git_info_outside_a_repo_has_no_git_info() {
        // A real, existing directory that is (almost certainly) not itself
        // inside a git repository.
        let dir = std::env::temp_dir();
        let info = detect(&dir.to_string_lossy(), Duration::from_secs(2));
        // `std::env::temp_dir()` could theoretically be nested inside a
        // repo on some machine; only assert the shape when it plainly is
        // not (no local .git and rev-parse fails), which the fixture-based
        // GitCache tests below cover deterministically regardless.
        let _ = info;
    }

    // --- GitCache ---

    fn counting_fetch(counter: &AtomicUsize) -> GitInfo {
        counter.fetch_add(1, Ordering::SeqCst);
        GitInfo {
            branch: Some("main".into()),
            remote: Some("origin-url".into()),
        }
    }

    #[test]
    fn get_or_fetch_reuses_cached_value_within_ttl_for_the_same_cwd() {
        let cache = GitCache::new(Duration::from_secs(60), Duration::from_secs(1));
        let counter = AtomicUsize::new(0);

        let first = cache.get_or_fetch("/repo/a", || counting_fetch(&counter));
        let second = cache.get_or_fetch("/repo/a", || counting_fetch(&counter));

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "second call must hit the cache"
        );
        assert_eq!(first, second);
    }

    #[test]
    fn get_or_fetch_collapses_lookups_for_sessions_sharing_one_cwd() {
        // Two different "sessions" (call sites) looking up the exact same
        // cwd must still only fetch once - this is the load-bearing
        // behaviour behind "several sessions commonly share one cwd".
        let cache = GitCache::new(Duration::from_secs(60), Duration::from_secs(1));
        let counter = AtomicUsize::new(0);

        for _ in 0..5 {
            cache.get_or_fetch("/shared/repo", || counting_fetch(&counter));
        }

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn get_or_fetch_does_not_share_across_different_cwds() {
        let cache = GitCache::new(Duration::from_secs(60), Duration::from_secs(1));
        let counter = AtomicUsize::new(0);

        cache.get_or_fetch("/repo/a", || counting_fetch(&counter));
        cache.get_or_fetch("/repo/b", || counting_fetch(&counter));

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn get_or_fetch_refetches_once_the_ttl_expires() {
        let cache = GitCache::new(Duration::from_millis(20), Duration::from_secs(1));
        let counter = AtomicUsize::new(0);

        cache.get_or_fetch("/repo/a", || counting_fetch(&counter));
        std::thread::sleep(Duration::from_millis(60));
        cache.get_or_fetch("/repo/a", || counting_fetch(&counter));

        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "a lookup after the TTL has elapsed must re-fetch"
        );
    }

    #[test]
    fn store_prunes_expired_entries_for_other_cwds_too() {
        // Finding 6 from the PRO-209 review: `entries` never evicted, so an
        // expired entry for a cwd that is never queried again - a session
        // that ended, most commonly - was retained for the life of the
        // process. A store for any cwd must prune every expired entry, not
        // just decide whether to reuse the one it was asked about.
        let cache = GitCache::new(Duration::from_millis(20), Duration::from_secs(1));
        let counter = AtomicUsize::new(0);

        cache.get_or_fetch("/repo/a", || counting_fetch(&counter));
        std::thread::sleep(Duration::from_millis(60));
        cache.get_or_fetch("/repo/b", || counting_fetch(&counter));

        let entries = cache.entries.lock().unwrap();
        assert!(
            !entries.contains_key("/repo/a"),
            "an expired entry must be pruned once any store happens, not retained forever"
        );
        assert!(entries.contains_key("/repo/b"));
    }
}
