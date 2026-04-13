use chrono::Utc;
use common::api::{ReportPayload, SessionView};
use common::session::Status;
use rusqlite::{Connection, Result, params};
use refinery::embed_migrations;

embed_migrations!("migrations");

pub fn open_db(path: &str) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    migrations::runner().run(&mut conn).expect("migration failed");
    Ok(conn)
}

pub trait SessionStore {
    fn upsert_session(&self, payload: &ReportPayload) -> Result<()>;
    fn list_active_sessions(&self) -> Result<Vec<SessionView>>;
}

impl SessionStore for Connection {
    fn upsert_session(&self, payload: &ReportPayload) -> Result<()> {
        let row = payload.status.to_row();
        let updated_at = Utc::now().to_rfc3339();
        self.execute(
            "INSERT INTO sessions (session_id, cwd, status, status_tool, waiting_reason, waiting_detail, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id) DO UPDATE SET
               cwd = excluded.cwd,
               status = excluded.status,
               status_tool = excluded.status_tool,
               waiting_reason = excluded.waiting_reason,
               waiting_detail = excluded.waiting_detail,
               updated_at = excluded.updated_at",
            params![
                payload.session_id,
                payload.cwd,
                row.status,
                row.status_tool,
                row.waiting_reason,
                row.waiting_detail,
                updated_at,
            ],
        )?;
        Ok(())
    }

    fn list_active_sessions(&self) -> Result<Vec<SessionView>> {
        let mut stmt = self.prepare(
            "SELECT session_id, cwd, status, status_tool, waiting_reason, waiting_detail, updated_at
             FROM sessions
             WHERE status != 'ended'
             ORDER BY updated_at DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let session_id: String = row.get(0)?;
            let cwd: String = row.get(1)?;
            let status_str: String = row.get(2)?;
            let status_tool: Option<String> = row.get(3)?;
            let waiting_reason: Option<String> = row.get(4)?;
            let waiting_detail: Option<String> = row.get(5)?;
            let updated_at_str: String = row.get(6)?;

            let status_row = common::session::StatusRow {
                status: status_str,
                status_tool,
                waiting_reason,
                waiting_detail,
            };
            let status = Status::from_row(&status_row).unwrap_or(Status::Ended);
            let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            Ok(SessionView { session_id, cwd, status, updated_at })
        })?;

        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::api::ReportPayload;
    use common::session::{Status, WorkingStatus, WaitingStatus, WaitingReason};

    fn make_conn() -> Connection {
        open_db(":memory:").unwrap()
    }

    fn working_payload(id: &str, cwd: &str) -> ReportPayload {
        ReportPayload {
            session_id: id.into(),
            cwd: cwd.into(),
            status: Status::Working(WorkingStatus { tool: None }),
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
        }
    }

    #[test]
    fn upsert_and_read_back() {
        let conn = make_conn();
        let payload = working_payload("s1", "/tmp/project");
        conn.upsert_session(&payload).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(sessions[0].cwd, "/tmp/project");
        assert_eq!(sessions[0].status, Status::Working(WorkingStatus { tool: None }));
    }

    #[test]
    fn upsert_same_id_last_write_wins() {
        let conn = make_conn();
        let p1 = working_payload("s1", "/tmp/first");
        conn.upsert_session(&p1).unwrap();

        // Small delay to ensure updated_at changes
        std::thread::sleep(std::time::Duration::from_millis(10));

        let p2 = ReportPayload {
            session_id: "s1".into(),
            cwd: "/tmp/second".into(),
            status: Status::Waiting(WaitingStatus {
                reason: WaitingReason::Permission,
                detail: None,
            }),
            hook_event_name: "PreToolUse".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
        };
        conn.upsert_session(&p2).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd, "/tmp/second");
        assert_eq!(
            sessions[0].status,
            Status::Waiting(WaitingStatus { reason: WaitingReason::Permission, detail: None })
        );
    }

    #[test]
    fn ended_sessions_excluded_from_list() {
        let conn = make_conn();
        let active = working_payload("s1", "/tmp/active");
        conn.upsert_session(&active).unwrap();

        let ended = ReportPayload {
            session_id: "s2".into(),
            cwd: "/tmp/ended".into(),
            status: Status::Ended,
            hook_event_name: "Stop".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
        };
        conn.upsert_session(&ended).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s1");
    }

    #[test]
    fn list_multiple_sessions() {
        let conn = make_conn();
        conn.upsert_session(&working_payload("s1", "/tmp/one")).unwrap();
        conn.upsert_session(&working_payload("s2", "/tmp/two")).unwrap();
        conn.upsert_session(&working_payload("s3", "/tmp/three")).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 3);
    }
}
