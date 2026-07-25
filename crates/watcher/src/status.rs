//! Temporary translation from the Claude session registry's `status` field
//! to this project's existing `common::session::Status`.
//!
//! **Temporary**, see PRO-214: this squeezes the registry's four-way status
//! (`busy`, `shell`, `idle`, `waiting`) into the existing two-state
//! `Status`/`WaitingReason` model because introducing new session-state
//! vocabulary is out of scope for the watcher's introduction - PRO-214
//! replaces this mapping once the hook-based reporting path (which this
//! `Status` type was designed for) is retired. Do not grow this file with
//! new `Status`/`WaitingReason` variants; that decision belongs to PRO-214.

use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};

/// Map a registry `status` string, plus the registry's free-form
/// `waitingFor`, onto `common::session::Status`.
///
/// `busy` and `shell` (actively running a shell command) both present as
/// `Working`. `idle` and `waiting` - and any value this format might
/// introduce in the future, since it is undocumented and owned by Claude
/// Code - present as `Waiting`, always with reason `Input`: the registry
/// carries no structured signal distinguishing a permission prompt from any
/// other kind of pause (unlike the hook path's dedicated
/// `PermissionRequest` event), and `waitingFor` is a free-form string, not
/// a closed enum, so guessing `Permission` from its contents would be
/// unfounded. `waitingFor` is carried through as the waiting detail
/// regardless of which of the two source statuses produced it.
pub(crate) fn map_status(status: &str, waiting_for: Option<&str>) -> Status {
    match status {
        "busy" | "shell" => Status::Working(WorkingStatus { tool: None }),
        _ => Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: waiting_for.map(str::to_string),
        }),
    }
}

// `busy`/`shell`/`idle`/`waiting` mapping, and `waitingFor` carried through
// as detail, are all covered end to end by
// `watcher_maps_registry_statuses_to_expected_session_states` in
// `crates/server/tests/reconciliation.rs`; duplicating them here as unit
// tests would just pin the same behaviour twice. What's kept below is the
// one case that integration test cannot express, since it feeds only
// currently-known status strings through the real registry format: the
// unknown-status fallback.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_status_degrades_to_waiting_input() {
        assert_eq!(
            map_status("some-future-status", None),
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Input,
                detail: None,
            })
        );
    }
}
