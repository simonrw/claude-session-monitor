use chrono::Utc;
use common::api::{AgentKind, ReportPayload, SessionView};
use common::session::Status;
use refinery::embed_migrations;
use rusqlite::{Connection, Result, params};

embed_migrations!("migrations");

pub fn open_db(path: &str) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    migrations::runner()
        .run(&mut conn)
        .expect("migration failed");
    tracing::info!(path, "database opened, migrations applied");
    Ok(conn)
}

pub trait SessionStore {
    fn upsert_session(&self, payload: &ReportPayload) -> Result<()>;
    fn list_active_sessions(&self) -> Result<Vec<SessionView>>;
    fn delete_session(&self, session_id: &str) -> Result<bool>;
    fn end_session(&self, session_id: &str) -> Result<bool>;
}

impl SessionStore for Connection {
    fn upsert_session(&self, payload: &ReportPayload) -> Result<()> {
        tracing::debug!(session_id = payload.session_id, status = ?payload.status, "upserting session");
        let row = payload.status.to_row();
        let updated_at = Utc::now().to_rfc3339();
        let agent_kind = match payload.agent_kind {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        };
        self.execute(
            "INSERT INTO sessions (session_id, cwd, status, status_tool, waiting_reason, waiting_detail, updated_at, hostname, git_branch, git_remote, tmux_target, agent_kind, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(session_id) DO UPDATE SET
               cwd = excluded.cwd,
               status = excluded.status,
               status_tool = excluded.status_tool,
               waiting_reason = excluded.waiting_reason,
               waiting_detail = excluded.waiting_detail,
               updated_at = excluded.updated_at,
               hostname = excluded.hostname,
               git_branch = excluded.git_branch,
               git_remote = excluded.git_remote,
               tmux_target = excluded.tmux_target,
               agent_kind = excluded.agent_kind,
               model = excluded.model",
            params![
                payload.session_id,
                payload.cwd,
                row.status,
                row.status_tool,
                row.waiting_reason,
                row.waiting_detail,
                updated_at,
                payload.hostname,
                payload.git_branch,
                payload.git_remote,
                payload.tmux_target,
                agent_kind,
                payload.model,
            ],
        )?;
        Ok(())
    }

    fn delete_session(&self, session_id: &str) -> Result<bool> {
        let rows = self.execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        tracing::debug!(session_id, found = rows > 0, "deleted session");
        Ok(rows > 0)
    }

    fn end_session(&self, session_id: &str) -> Result<bool> {
        let updated_at = Utc::now().to_rfc3339();
        let rows = self.execute(
            "UPDATE sessions
             SET status = 'ended',
                 status_tool = NULL,
                 waiting_reason = NULL,
                 waiting_detail = NULL,
                 updated_at = ?2
             WHERE session_id = ?1",
            params![session_id, updated_at],
        )?;
        tracing::debug!(session_id, found = rows > 0, "ended session");
        Ok(rows > 0)
    }

    fn list_active_sessions(&self) -> Result<Vec<SessionView>> {
        let mut stmt = self.prepare(
            "SELECT session_id, cwd, status, status_tool, waiting_reason, waiting_detail, updated_at, hostname, git_branch, git_remote, tmux_target, agent_kind, model
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
            let hostname: Option<String> = row.get(7)?;
            let git_branch: Option<String> = row.get(8)?;
            let git_remote: Option<String> = row.get(9)?;
            let tmux_target: Option<String> = row.get(10)?;
            let agent_kind: String = row.get(11)?;
            let model: Option<String> = row.get(12)?;
            let agent_kind = match agent_kind.as_str() {
                "codex" => AgentKind::Codex,
                _ => AgentKind::Claude,
            };

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

            Ok(SessionView {
                session_id,
                cwd,
                status,
                agent_kind,
                model,
                updated_at,
                hostname,
                git_branch,
                git_remote,
                tmux_target,
            })
        })?;

        let sessions: Result<Vec<SessionView>> = rows.collect();
        if let Ok(ref s) = sessions {
            tracing::debug!(count = s.len(), "listed active sessions");
        }
        sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::api::{AgentKind, ReportPayload};
    use common::session::{Status, WaitingReason, WaitingStatus, WorkingStatus};

    fn make_conn() -> Connection {
        open_db(":memory:").unwrap()
    }

    fn working_payload(id: &str, cwd: &str) -> ReportPayload {
        ReportPayload {
            session_id: id.into(),
            cwd: cwd.into(),
            status: Status::Working(WorkingStatus { tool: None }),
            agent_kind: AgentKind::Claude,
            model: None,
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        }
    }

    #[test]
    fn delete_session_missing_returns_false() {
        let conn = make_conn();
        let deleted = conn.delete_session("nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn delete_session_removes_it() {
        let conn = make_conn();
        conn.upsert_session(&working_payload("s1", "/tmp/project"))
            .unwrap();
        let deleted = conn.delete_session("s1").unwrap();
        assert!(deleted);
        let sessions = conn.list_active_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn end_session_marks_it_inactive() {
        let conn = make_conn();
        conn.upsert_session(&working_payload("s1", "/tmp/project"))
            .unwrap();
        let ended = conn.end_session("s1").unwrap();
        assert!(ended);
        let sessions = conn.list_active_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn end_session_missing_returns_false() {
        let conn = make_conn();
        let ended = conn.end_session("missing").unwrap();
        assert!(!ended);
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
        assert_eq!(
            sessions[0].status,
            Status::Working(WorkingStatus { tool: None })
        );
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
            agent_kind: AgentKind::Claude,
            model: None,
            hook_event_name: "PreToolUse".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };
        conn.upsert_session(&p2).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd, "/tmp/second");
        assert_eq!(
            sessions[0].status,
            Status::Waiting(WaitingStatus {
                reason: WaitingReason::Permission,
                detail: None
            })
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
            agent_kind: AgentKind::Claude,
            model: None,
            hook_event_name: "Stop".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };
        conn.upsert_session(&ended).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s1");
    }

    #[test]
    fn list_multiple_sessions() {
        let conn = make_conn();
        conn.upsert_session(&working_payload("s1", "/tmp/one"))
            .unwrap();
        conn.upsert_session(&working_payload("s2", "/tmp/two"))
            .unwrap();
        conn.upsert_session(&working_payload("s3", "/tmp/three"))
            .unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn enrichment_fields_round_trip() {
        let conn = make_conn();
        let payload = ReportPayload {
            session_id: "enriched".into(),
            cwd: "/tmp/project".into(),
            status: Status::Working(WorkingStatus { tool: None }),
            agent_kind: AgentKind::Claude,
            model: None,
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: Some("myhost".into()),
            git_branch: Some("main".into()),
            git_remote: Some("https://github.com/user/repo.git".into()),
            tmux_target: Some("main:2.1".into()),
        };
        conn.upsert_session(&payload).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].hostname, Some("myhost".into()));
        assert_eq!(sessions[0].git_branch, Some("main".into()));
        assert_eq!(
            sessions[0].git_remote,
            Some("https://github.com/user/repo.git".into())
        );
        assert_eq!(sessions[0].tmux_target, Some("main:2.1".into()));
    }

    #[test]
    fn agent_metadata_round_trip() {
        let conn = make_conn();
        let payload = ReportPayload {
            session_id: "codex-session".into(),
            cwd: "/tmp/project".into(),
            status: Status::Working(WorkingStatus { tool: None }),
            agent_kind: AgentKind::Codex,
            model: Some("gpt-5.1-codex".into()),
            hook_event_name: "SessionStart".into(),
            tool_name: None,
            tool_input: None,
            notification_type: None,
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };
        conn.upsert_session(&payload).unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_kind, AgentKind::Codex);
        assert_eq!(sessions[0].model, Some("gpt-5.1-codex".into()));
    }

    #[test]
    fn existing_rows_migrate_as_claude_sessions() {
        let conn = make_conn();
        conn.execute(
            "INSERT INTO sessions (session_id, cwd, status, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                "legacy",
                "/tmp/project",
                "working",
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .unwrap();

        let sessions = conn.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_kind, AgentKind::Claude);
        assert_eq!(sessions[0].model, None);
    }
}
