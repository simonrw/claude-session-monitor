use std::collections::HashSet;

use chrono::Utc;
use common::api::{AgentKind, HostStatus, ReportPayload, SessionView, SnapshotSession};
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

    /// Reconcile the server's view of one host's sessions for one agent kind
    /// against a complete snapshot: upsert every session in `sessions`, and
    /// end every non-ended session scoped to `hostname` and `agent_kind`
    /// that is absent from it.
    ///
    /// Rows with a null hostname, rows belonging to another host, and rows
    /// of another agent kind are never touched. Returns `true` if the
    /// snapshot changed at least one row (an upsert whose values genuinely
    /// differed from what was stored, or a session that was actually
    /// ended); returns `false` if the snapshot was a no-op republish, so
    /// callers know whether to broadcast.
    fn apply_snapshot(
        &self,
        hostname: &str,
        agent_kind: AgentKind,
        sessions: &[SnapshotSession],
    ) -> Result<bool>;

    /// Record that a snapshot was just accepted from `hostname` for
    /// `agent_kind`, regardless of whether it changed anything or contained
    /// any sessions at all (see [`HostStatus`]'s doc comment for why the
    /// "contained no sessions" case matters here). Upserts `last_seen_at` to
    /// now.
    ///
    /// Deliberately separate from `apply_snapshot`: this is PRO-211's
    /// addition, and `apply_snapshot`'s own reconciliation logic is
    /// untouched by it (PRO-211 requires the server's snapshot handling stay
    /// unchanged and purely idempotent).
    fn record_host_seen(&self, hostname: &str, agent_kind: AgentKind) -> Result<()>;

    /// The last-seen time for every host and agent kind that has ever
    /// published a snapshot, most recently seen first.
    fn list_host_status(&self) -> Result<Vec<HostStatus>>;
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

    fn apply_snapshot(
        &self,
        hostname: &str,
        agent_kind: AgentKind,
        sessions: &[SnapshotSession],
    ) -> Result<bool> {
        let agent_kind_str = match agent_kind {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        };

        // The upserts and the reaping update below must land atomically: if
        // an error occurred partway through without a transaction, the
        // snapshot would be half-applied while the `changed` flag we'd
        // return is simply discarded, so no broadcast fires for the rows
        // that did land until the next poll. `unchecked_transaction` (rather
        // than `transaction`, which needs `&mut self`) matches the `&self`
        // shape the `SessionStore` trait requires.
        let tx = self.unchecked_transaction()?;

        let mut changed = false;
        for session in sessions {
            if upsert_snapshot_session(&tx, hostname, agent_kind_str, session)? {
                changed = true;
            }
        }

        let snapshot_ids: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();

        let active_ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT session_id FROM sessions
                 WHERE hostname = ?1 AND agent_kind = ?2 AND status != 'ended'",
            )?;
            stmt.query_map(params![hostname, agent_kind_str], |row| row.get(0))?
                .collect::<Result<Vec<String>>>()?
        };

        for id in active_ids {
            if !snapshot_ids.contains(id.as_str()) && tx.end_session(&id)? {
                changed = true;
            }
        }

        tx.commit()?;

        tracing::debug!(
            hostname,
            agent_kind = agent_kind_str,
            session_count = sessions.len(),
            changed,
            "applied snapshot"
        );
        Ok(changed)
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

    fn record_host_seen(&self, hostname: &str, agent_kind: AgentKind) -> Result<()> {
        let agent_kind_str = match agent_kind {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        };
        let last_seen_at = Utc::now().to_rfc3339();
        self.execute(
            "INSERT INTO host_status (hostname, agent_kind, last_seen_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(hostname, agent_kind) DO UPDATE SET
               last_seen_at = excluded.last_seen_at",
            params![hostname, agent_kind_str, last_seen_at],
        )?;
        Ok(())
    }

    fn list_host_status(&self) -> Result<Vec<HostStatus>> {
        let mut stmt = self.prepare(
            "SELECT hostname, agent_kind, last_seen_at
             FROM host_status
             ORDER BY last_seen_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let hostname: String = row.get(0)?;
            let agent_kind_str: String = row.get(1)?;
            let last_seen_at_str: String = row.get(2)?;
            let agent_kind = match agent_kind_str.as_str() {
                "codex" => AgentKind::Codex,
                _ => AgentKind::Claude,
            };
            let last_seen_at = chrono::DateTime::parse_from_rfc3339(&last_seen_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            Ok(HostStatus {
                hostname,
                agent_kind,
                last_seen_at,
            })
        })?;
        rows.collect()
    }
}

/// Existing column values compared against an incoming `SnapshotSession` to
/// decide whether a republish is a genuine change.
struct ExistingSnapshotRow {
    cwd: String,
    status: String,
    status_tool: Option<String>,
    waiting_reason: Option<String>,
    waiting_detail: Option<String>,
    hostname: Option<String>,
    git_branch: Option<String>,
    git_remote: Option<String>,
    tmux_target: Option<String>,
    agent_kind: String,
    model: Option<String>,
    name: Option<String>,
}

// Looked up by `session_id` alone, not scoped to `hostname`/`agent_kind`.
// `session_id` is a UUID generated by the watcher and is globally unique, so
// a snapshot from one host can never legitimately contain a session id
// already owned by another host (or by a null-hostname row): if it somehow
// did, re-homing that row to the new host is the intentional behaviour, not
// a bug, and in practice it is unreachable because UUIDs don't collide.
fn fetch_existing_row(conn: &Connection, session_id: &str) -> Result<Option<ExistingSnapshotRow>> {
    let result = conn.query_row(
        "SELECT cwd, status, status_tool, waiting_reason, waiting_detail, hostname, git_branch, git_remote, tmux_target, agent_kind, model, name
         FROM sessions WHERE session_id = ?1",
        params![session_id],
        |row| {
            Ok(ExistingSnapshotRow {
                cwd: row.get(0)?,
                status: row.get(1)?,
                status_tool: row.get(2)?,
                waiting_reason: row.get(3)?,
                waiting_detail: row.get(4)?,
                hostname: row.get(5)?,
                git_branch: row.get(6)?,
                git_remote: row.get(7)?,
                tmux_target: row.get(8)?,
                agent_kind: row.get(9)?,
                model: row.get(10)?,
                name: row.get(11)?,
            })
        },
    );
    match result {
        Ok(row) => Ok(Some(row)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Upsert one session from a snapshot. Skips the write entirely (leaving
/// `updated_at` untouched) when every stored column already matches, so a
/// republish of unchanged data never bumps `updated_at` and never counts as
/// a change for broadcast purposes. Returns whether a write happened.
fn upsert_snapshot_session(
    conn: &Connection,
    hostname: &str,
    agent_kind: &str,
    session: &SnapshotSession,
) -> Result<bool> {
    let row = session.status.to_row();
    let existing = fetch_existing_row(conn, &session.session_id)?;

    let unchanged = existing.as_ref().is_some_and(|e| {
        e.cwd == session.cwd
            && e.status == row.status
            && e.status_tool == row.status_tool
            && e.waiting_reason == row.waiting_reason
            && e.waiting_detail == row.waiting_detail
            && e.hostname.as_deref() == Some(hostname)
            && e.git_branch == session.git_branch
            && e.git_remote == session.git_remote
            && e.tmux_target == session.tmux_target
            && e.agent_kind == agent_kind
            && e.model == session.model
            && e.name == session.name
    });

    if unchanged {
        tracing::debug!(
            session_id = session.session_id,
            "snapshot session unchanged, skipping write"
        );
        return Ok(false);
    }

    let updated_at = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sessions (session_id, cwd, status, status_tool, waiting_reason, waiting_detail, updated_at, hostname, git_branch, git_remote, tmux_target, agent_kind, model, name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
           model = excluded.model,
           name = excluded.name",
        params![
            session.session_id,
            session.cwd,
            row.status,
            row.status_tool,
            row.waiting_reason,
            row.waiting_detail,
            updated_at,
            hostname,
            session.git_branch,
            session.git_remote,
            session.tmux_target,
            agent_kind,
            session.model,
            session.name,
        ],
    )?;
    Ok(true)
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

    // `apply_snapshot` behaviour (upserting, reaping absent sessions, host /
    // agent-kind scoping, no-op republish detection, and not churning
    // already-ended sessions) is covered end-to-end by the integration tests
    // in `crates/server/tests/reconciliation.rs`, which assert through
    // `SseClient` per this crate's testing conventions. There is nothing
    // about `apply_snapshot` those tests do not already exercise, so no
    // trait-level duplicate tests are kept here.

    #[test]
    fn record_host_seen_then_listed() {
        let conn = make_conn();
        conn.record_host_seen("host-a", AgentKind::Claude).unwrap();
        let statuses = conn.list_host_status().unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].hostname, "host-a");
        assert_eq!(statuses[0].agent_kind, AgentKind::Claude);
    }

    #[test]
    fn record_host_seen_twice_updates_last_seen_at_rather_than_duplicating() {
        let conn = make_conn();
        conn.record_host_seen("host-a", AgentKind::Claude).unwrap();
        let first = conn.list_host_status().unwrap();
        let first_seen = first[0].last_seen_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        conn.record_host_seen("host-a", AgentKind::Claude).unwrap();

        let statuses = conn.list_host_status().unwrap();
        assert_eq!(statuses.len(), 1, "must upsert, not duplicate rows");
        assert!(
            statuses[0].last_seen_at > first_seen,
            "last_seen_at must advance on a repeat call"
        );
    }

    #[test]
    fn record_host_seen_scopes_by_hostname_and_agent_kind_independently() {
        let conn = make_conn();
        conn.record_host_seen("host-a", AgentKind::Claude).unwrap();
        conn.record_host_seen("host-a", AgentKind::Codex).unwrap();
        conn.record_host_seen("host-b", AgentKind::Claude).unwrap();

        let statuses = conn.list_host_status().unwrap();
        assert_eq!(statuses.len(), 3);
    }

    #[test]
    fn list_host_status_empty_when_no_host_has_ever_reported() {
        let conn = make_conn();
        assert!(conn.list_host_status().unwrap().is_empty());
    }
}
