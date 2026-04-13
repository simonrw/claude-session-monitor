use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StatusRowError {
    #[error("unknown status kind: {0}")]
    UnknownKind(String),
    #[error("missing waiting_reason for waiting status")]
    MissingWaitingReason,
    #[error("unknown waiting reason: {0}")]
    UnknownWaitingReason(String),
}

/// Flattened representation of Status for SQL column storage.
pub struct StatusRow {
    pub status: String,
    pub status_tool: Option<String>,
    pub waiting_reason: Option<String>,
    pub waiting_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingStatus {
    pub tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    Permission,
    Input,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitingStatus {
    pub reason: WaitingReason,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Status {
    Working(WorkingStatus),
    Waiting(WaitingStatus),
    Ended,
}

impl Status {
    pub fn to_row(&self) -> StatusRow {
        match self {
            Status::Working(w) => StatusRow {
                status: "working".into(),
                status_tool: w.tool.clone(),
                waiting_reason: None,
                waiting_detail: None,
            },
            Status::Waiting(w) => StatusRow {
                status: "waiting".into(),
                status_tool: None,
                waiting_reason: Some(match w.reason {
                    WaitingReason::Permission => "permission".into(),
                    WaitingReason::Input => "input".into(),
                }),
                waiting_detail: w.detail.clone(),
            },
            Status::Ended => StatusRow {
                status: "ended".into(),
                status_tool: None,
                waiting_reason: None,
                waiting_detail: None,
            },
        }
    }

    pub fn from_row(row: &StatusRow) -> Result<Status, StatusRowError> {
        match row.status.as_str() {
            "working" => Ok(Status::Working(WorkingStatus { tool: row.status_tool.clone() })),
            "waiting" => {
                let reason = match row
                    .waiting_reason
                    .as_deref()
                    .ok_or(StatusRowError::MissingWaitingReason)?
                {
                    "permission" => WaitingReason::Permission,
                    "input" => WaitingReason::Input,
                    other => return Err(StatusRowError::UnknownWaitingReason(other.into())),
                };
                Ok(Status::Waiting(WaitingStatus { reason, detail: row.waiting_detail.clone() }))
            }
            "ended" => Ok(Status::Ended),
            other => Err(StatusRowError::UnknownKind(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_with_tool_round_trips_json() {
        let status = Status::Working(WorkingStatus { tool: Some("Bash".into()) });
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Working(WorkingStatus { tool: Some("Bash".into()) }));
    }

    #[test]
    fn working_without_tool_round_trips_json() {
        let status = Status::Working(WorkingStatus { tool: None });
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn waiting_permission_round_trips_json() {
        let status = Status::Waiting(WaitingStatus {
            reason: WaitingReason::Permission,
            detail: None,
        });
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored,
            Status::Waiting(WaitingStatus { reason: WaitingReason::Permission, detail: None })
        );
    }

    #[test]
    fn working_with_tool_round_trips_sqlite() {
        let status = Status::Working(WorkingStatus { tool: Some("Bash".into()) });
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn waiting_permission_round_trips_sqlite() {
        let status =
            Status::Waiting(WaitingStatus { reason: WaitingReason::Permission, detail: None });
        let row = status.to_row();
        let restored = Status::from_row(&row).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn waiting_input_with_detail_round_trips_sqlite() {
        let status = Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: Some("Shall I continue?".into()),
        });
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
    fn ended_round_trips_json() {
        let status = Status::Ended;
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Status::Ended);
    }

    #[test]
    fn waiting_input_with_detail_round_trips_json() {
        let status = Status::Waiting(WaitingStatus {
            reason: WaitingReason::Input,
            detail: Some("Shall I continue?".into()),
        });
        let json = serde_json::to_string(&status).unwrap();
        let restored: Status = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Input,
                detail: Some("Shall I continue?".into()),
            })
        );
    }
}
