//! Cross-sweep debounce for absent sessions (PRO-211): a session must be
//! missing from two consecutive *successful* sweeps before the watcher
//! stops publishing it, so a single sweep that legitimately (and honestly)
//! omits a session it saw last time - a registry file removed a beat before
//! the process itself actually exits, for instance - does not immediately
//! end a session that is, in practice, still there a moment later.
//!
//! This lives here, not in `sweep`: `sweep::sweep` is a stateless, one-shot
//! function (see its own module doc comment) that fails loudly rather than
//! ever guessing; debouncing is inherently cross-sweep state, which only the
//! daemon loop (`main.rs`'s `run_daemon`) can own across cycles - the same
//! pattern `git::GitCache` and `main.rs`'s own `Backoff` already follow.
//! `main.rs` constructs one [`Debounce`] and holds it for the life of the
//! daemon (a fresh, throwaway one for each `--once` invocation, where a
//! single sweep can never observe two consecutive anything).
//!
//! **Only a sweep that completes successfully ever reaches [`Debounce::apply`].**
//! A failed sweep (`sweep::SweepError`) is handled entirely upstream, in
//! `main.rs`'s `run_cycle`, which returns `CycleOutcome::SweepFailed`
//! without ever calling `apply` at all. So two consecutive failed sweeps do
//! not advance the debounce, and do not reset it either - they simply leave
//! it exactly where it was, as if that cycle had not happened. This is what
//! stops a run of sweep failures from reaping every tracked session two
//! cycles in: only a *successful* sweep that genuinely, honestly omits a
//! session counts as one "absence" toward the two-strike limit.
//!
//! **What a session in its one-sweep grace period publishes.** A session
//! missing from the latest successful sweep, but not yet missing from two in
//! a row, is republished exactly as it was last actually observed - frozen,
//! not re-derived. Its status, `cwd`, git branch/remote, tmux target, and
//! name all carry over unchanged from the last sweep that actually saw it;
//! nothing here tries to guess whether any of that is still accurate.
//! This is deliberate: inventing a fresher answer with no fresh observation
//! to justify it would be its own kind of dishonesty, and re-deriving
//! enrichment (git/tmux) for a session `sweep` did not see this cycle would
//! reach past what those modules can support - `sweep` itself never handed
//! this cycle a `cwd` for it to enrich. The alternative, silently dropping
//! it a sweep early, is exactly the hazard PRO-211 exists to close.

use std::collections::HashMap;

use common::api::SnapshotSession;

/// How many consecutive successful sweeps a session may be absent from
/// before [`Debounce::apply`] stops republishing it. A session missing from
/// exactly one successful sweep is still republished (frozen, see the
/// module doc comment); a second consecutive miss drops it.
const MAX_MISSING_STREAK: u32 = 2;

/// One tracked session's last known snapshot, plus how many consecutive
/// successful sweeps it has now been absent from.
struct Tracked {
    session: SnapshotSession,
    missing_streak: u32,
}

/// Cross-sweep debounce state, owned once by the daemon loop and threaded
/// through every cycle - see the module doc comment.
#[derive(Default)]
pub struct Debounce {
    tracked: HashMap<String, Tracked>,
}

impl Debounce {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one successful sweep's result, returning what should actually
    /// be published this cycle: every session in `fresh` (its missing streak
    /// reset to zero), plus any previously-tracked session now in its
    /// one-sweep grace period, republished frozen exactly as last observed.
    ///
    /// Must only ever be called with the result of a *successful* sweep -
    /// see the module doc comment for why a failed sweep must never reach
    /// here at all.
    pub fn apply(&mut self, fresh: Vec<SnapshotSession>) -> Vec<SnapshotSession> {
        let fresh_ids: std::collections::HashSet<String> =
            fresh.iter().map(|s| s.session_id.clone()).collect();

        for session in fresh {
            self.tracked.insert(
                session.session_id.clone(),
                Tracked {
                    session,
                    missing_streak: 0,
                },
            );
        }

        // Every session not seen this sweep ages by one; anything that has
        // now reached MAX_MISSING_STREAK is dropped for good.
        self.tracked.retain(|session_id, t| {
            if fresh_ids.contains(session_id) {
                return true;
            }
            t.missing_streak += 1;
            t.missing_streak < MAX_MISSING_STREAK
        });

        self.tracked.values().map(|t| t.session.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::session::{Status, WorkingStatus};

    fn session(id: &str) -> SnapshotSession {
        SnapshotSession {
            session_id: id.to_string(),
            cwd: format!("/tmp/{id}"),
            status: Status::Working(WorkingStatus { tool: None }),
            name: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
            model: None,
        }
    }

    fn ids(sessions: &[SnapshotSession]) -> std::collections::HashSet<String> {
        sessions.iter().map(|s| s.session_id.clone()).collect()
    }

    #[test]
    fn a_session_present_every_sweep_is_always_published() {
        let mut debounce = Debounce::new();
        for _ in 0..3 {
            let out = debounce.apply(vec![session("a")]);
            assert_eq!(ids(&out), std::collections::HashSet::from(["a".into()]));
        }
    }

    #[test]
    fn a_session_absent_from_exactly_one_sweep_is_still_published_frozen() {
        let mut debounce = Debounce::new();
        debounce.apply(vec![session("a")]);
        // Absent from this sweep: still republished, once.
        let out = debounce.apply(vec![]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "a");
        assert_eq!(
            out[0].cwd, "/tmp/a",
            "the frozen session's fields are unchanged from last observed"
        );
    }

    #[test]
    fn a_session_absent_from_two_consecutive_sweeps_is_dropped() {
        let mut debounce = Debounce::new();
        debounce.apply(vec![session("a")]);
        let out = debounce.apply(vec![]); // strike 1
        assert_eq!(out.len(), 1);
        let out = debounce.apply(vec![]); // strike 2
        assert!(
            out.is_empty(),
            "must be dropped after two consecutive absences"
        );
    }

    #[test]
    fn reappearing_during_the_grace_period_resets_the_streak() {
        let mut debounce = Debounce::new();
        debounce.apply(vec![session("a")]);
        debounce.apply(vec![]); // strike 1
        // Reappears - the streak must reset, not merely pause.
        debounce.apply(vec![session("a")]);
        let out = debounce.apply(vec![]); // this must be strike 1 again, not strike 2
        assert_eq!(
            out.len(),
            1,
            "a reappearance must reset the missing streak to zero"
        );
    }

    #[test]
    fn a_brand_new_session_with_no_history_is_published_immediately() {
        let mut debounce = Debounce::new();
        let out = debounce.apply(vec![session("a")]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn once_dropped_a_session_does_not_reappear_on_its_own() {
        let mut debounce = Debounce::new();
        debounce.apply(vec![session("a")]);
        debounce.apply(vec![]); // strike 1
        debounce.apply(vec![]); // strike 2, dropped
        let out = debounce.apply(vec![]);
        assert!(out.is_empty());
    }
}
