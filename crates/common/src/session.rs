use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StatusRowError {
    #[error("unknown status kind: {0}")]
    UnknownKind(String),
}

/// Flattened representation of Status for SQL column storage.
pub struct StatusRow {
    pub status: String,
    pub status_tool: Option<String>,
    pub waiting_detail: Option<String>,
}

/// A session's state, in the Claude Code session registry's own vocabulary
/// (see PRO-214): `Busy`/`Shell`/`Idle`/`Waiting` mirror the registry's
/// `busy`/`shell`/`idle`/`waiting` `status` field directly, rather than the
/// old two-state Working/Waiting model this replaced. `Ended` remains a
/// purely local concept - the registry has no equivalent, since Claude Code
/// simply deletes a session's registry file when it exits.
///
/// - `Busy`: thinking, or actively running a tool. `tool` is populated only
///   by the Codex hook path (which names the tool it's about to/just ran);
///   the registry carries no per-session tool-name signal, so a
///   registry-derived `Busy` is always `Busy { tool: None }` - see
///   [`Status::from_registry`].
/// - `Shell`: a foreground shell command is running. The registry
///   distinguishes this from `Busy` (thinking/tool-use); nothing before
///   PRO-214 could express it at all.
/// - `Idle`: the turn has finished and the session is sitting at the
///   prompt - not blocked on anything, just not currently doing work. This
///   is what distinguishes "done, nothing needed" from `Waiting`, which is
///   "blocked on you specifically".
/// - `Waiting`: blocked on the user. `detail` carries the registry's
///   free-form `waitingFor` (or, on the Codex hook path, the permission
///   request's tool-input description) - not a closed reason enum. The
///   previous model's `WaitingReason::{Permission, Input}` split is gone:
///   the registry has no structured signal distinguishing a permission
///   prompt from any other kind of pause, so that distinction was never
///   more than a guess dressed up as a type.
/// - `Ended`: the session is gone. Set locally when a session drops out of
///   a watcher snapshot or a hook reports session end - never derived from
///   the registry, which simply stops having a file for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Status {
    Busy { tool: Option<String> },
    Shell,
    Idle,
    Waiting { detail: Option<String> },
    Ended,
}

impl Status {
    /// Construct a `Status` directly from the Claude Code session
    /// registry's own fields: its bare `status` string and free-form
    /// `waitingFor`. This is a straight pass-through, not a translation -
    /// see PRO-214, which replaced the old watcher::status::map_status
    /// squeeze that forced the registry's four-way status into the
    /// previous two-state model. `tool` is always `None` here: see this
    /// type's doc comment for why.
    ///
    /// An unrecognized `status` string - the registry's format is
    /// undocumented and owned by Claude Code, not this project, so it can
    /// change without warning - degrades to `Idle` rather than `Waiting` or
    /// `Busy`: `Idle` makes the weakest possible claim (nothing needs your
    /// attention, nothing is actively running), instead of risking a false
    /// "needs you" alert or a false "actually busy" count from a status
    /// string this project has never seen.
    pub fn from_registry(status: &str, waiting_for: Option<&str>) -> Status {
        match status {
            "busy" => Status::Busy { tool: None },
            "shell" => Status::Shell,
            "waiting" => Status::Waiting {
                detail: waiting_for.map(str::to_string),
            },
            // "idle", and anything this project doesn't yet recognize.
            _ => Status::Idle,
        }
    }

    pub fn to_row(&self) -> StatusRow {
        match self {
            Status::Busy { tool } => StatusRow {
                status: "busy".into(),
                status_tool: tool.clone(),
                waiting_detail: None,
            },
            Status::Shell => StatusRow {
                status: "shell".into(),
                status_tool: None,
                waiting_detail: None,
            },
            Status::Idle => StatusRow {
                status: "idle".into(),
                status_tool: None,
                waiting_detail: None,
            },
            Status::Waiting { detail } => StatusRow {
                status: "waiting".into(),
                status_tool: None,
                waiting_detail: detail.clone(),
            },
            Status::Ended => StatusRow {
                status: "ended".into(),
                status_tool: None,
                waiting_detail: None,
            },
        }
    }

    pub fn from_row(row: &StatusRow) -> Result<Status, StatusRowError> {
        match row.status.as_str() {
            "busy" => Ok(Status::Busy {
                tool: row.status_tool.clone(),
            }),
            "shell" => Ok(Status::Shell),
            "idle" => Ok(Status::Idle),
            "waiting" => Ok(Status::Waiting {
                detail: row.waiting_detail.clone(),
            }),
            "ended" => Ok(Status::Ended),
            other => Err(StatusRowError::UnknownKind(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_with_tool_round_trips_json() {
        let status = Status::Busy {
            tool: Some("Bash".into()),
        };
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored,
            Status::Busy {
                tool: Some("Bash".into())
            }
        );
    }

    #[test]
    fn busy_without_tool_round_trips_json() {
        let status = Status::Busy { tool: None };
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Busy { tool: None });
    }

    #[test]
    fn shell_round_trips_json() {
        let status = Status::Shell;
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Shell);
    }

    #[test]
    fn idle_round_trips_json() {
        let status = Status::Idle;
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Idle);
    }

    #[test]
    fn waiting_with_detail_round_trips_json() {
        let status = Status::Waiting {
            detail: Some("Shall I continue?".into()),
        };
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored,
            Status::Waiting {
                detail: Some("Shall I continue?".into()),
            }
        );
    }

    #[test]
    fn waiting_without_detail_round_trips_json() {
        let status = Status::Waiting { detail: None };
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Waiting { detail: None });
    }

    #[test]
    fn ended_round_trips_json() {
        let status = Status::Ended;
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Ended);
    }

    #[test]
    fn busy_with_tool_round_trips_sqlite() {
        let status = Status::Busy {
            tool: Some("Bash".into()),
        };
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn shell_round_trips_sqlite() {
        let status = Status::Shell;
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn idle_round_trips_sqlite() {
        let status = Status::Idle;
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn waiting_with_detail_round_trips_sqlite() {
        let status = Status::Waiting {
            detail: Some("Shall I continue?".into()),
        };
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn ended_round_trips_sqlite() {
        let status = Status::Ended;
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn from_row_rejects_unknown_kind() {
        let row = StatusRow {
            status: "bogus".into(),
            status_tool: None,
            waiting_detail: None,
        };
        assert!(matches!(
            Status::from_row(&row),
            Err(StatusRowError::UnknownKind(k)) if k == "bogus"
        ));
    }

    // --- from_registry: the watcher's pass-through constructor ---

    #[test]
    fn from_registry_busy_ignores_waiting_for() {
        assert_eq!(
            Status::from_registry("busy", Some("ignored")),
            Status::Busy { tool: None }
        );
    }

    #[test]
    fn from_registry_shell() {
        assert_eq!(Status::from_registry("shell", None), Status::Shell);
    }

    #[test]
    fn from_registry_idle() {
        assert_eq!(Status::from_registry("idle", None), Status::Idle);
    }

    #[test]
    fn from_registry_waiting_carries_waiting_for_as_detail() {
        assert_eq!(
            Status::from_registry("waiting", Some("Allow Bash to run cargo test?")),
            Status::Waiting {
                detail: Some("Allow Bash to run cargo test?".into())
            }
        );
    }

    #[test]
    fn from_registry_waiting_with_no_waiting_for() {
        assert_eq!(
            Status::from_registry("waiting", None),
            Status::Waiting { detail: None }
        );
    }

    #[test]
    fn from_registry_unknown_status_degrades_to_idle() {
        assert_eq!(
            Status::from_registry("some-future-status", Some("detail")),
            Status::Idle
        );
    }
}
