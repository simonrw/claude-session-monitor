//! Codex CLI session discovery from its writer-lock directory.
//!
//! Codex 0.147.0 and later holds an exclusive `flock` on
//! `$CODEX_HOME/thread-writer-locks/<thread_id>.lock` for each live thread.
//! These files and their locking behavior are undocumented Codex internals,
//! so every assumption about them is kept in this module.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::api::SnapshotSession;
use common::session::Status;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::FromSql};

use crate::git::{GitCache, GitInfo};

const ACTIVITY_THRESHOLD_SECONDS: i64 = 30;

/// Integration-test override for Codex's home directory.
pub const HOME_ENV: &str = "CSM_WATCHER_CODEX_HOME";

fn home() -> Option<PathBuf> {
    std::env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Return the complete best-effort snapshot of live Codex CLI threads.
pub fn sweep(git_cache: &GitCache) -> Vec<SnapshotSession> {
    let Some(home) = home() else {
        tracing::debug!("Codex home is unavailable; publishing an empty Codex snapshot");
        return Vec::new();
    };
    live_sessions(&home, git_cache)
}

fn live_sessions(home: &Path, git_cache: &GitCache) -> Vec<SnapshotSession> {
    let locks_dir = home.join("thread-writer-locks");
    let entries = match std::fs::read_dir(&locks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                path = %locks_dir.display(),
                "Codex writer-lock directory is absent; publishing an empty Codex snapshot"
            );
            return Vec::new();
        }
        Err(error) => {
            tracing::debug!(
                path = %locks_dir.display(),
                %error,
                "failed to read Codex writer-lock directory; publishing an empty Codex snapshot"
            );
            return Vec::new();
        }
    };

    let state = open_state(home);
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| live_session(&entry.path(), state.as_ref(), git_cache))
        .collect()
}

fn live_session(
    path: &Path,
    state: Option<&Connection>,
    git_cache: &GitCache,
) -> Option<SnapshotSession> {
    let file_name = path.file_name()?.to_str()?;
    let thread_id = file_name.strip_suffix(".lock")?;
    if thread_id.is_empty() || thread_id == ".coordination" {
        return None;
    }

    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "skipping unreadable Codex writer lock");
            return None;
        }
    };

    if !is_locked_by_writer(&file, path) {
        return None;
    }

    let enrichment = state.map_or_else(ThreadEnrichment::default, |connection| {
        thread_enrichment(connection, thread_id)
    });
    let git_info = enrichment
        .cwd
        .as_deref()
        .map_or_else(GitInfo::default, |cwd| git_cache.get(cwd));

    Some(SnapshotSession {
        session_id: thread_id.to_owned(),
        cwd: enrichment.cwd.unwrap_or_default(),
        status: enrichment.updated_at.map_or(Status::Idle, |updated_at| {
            status_from_updated_at(updated_at, chrono::Utc::now())
        }),
        name: enrichment.title,
        git_branch: git_info.branch,
        git_remote: git_info.remote,
        tmux_target: None,
        model: None,
    })
}

#[derive(Default)]
struct ThreadEnrichment {
    cwd: Option<String>,
    title: Option<String>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn open_state(home: &Path) -> Option<Connection> {
    let path = newest_state_path(home)?;
    let connection = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "failed to open Codex state sqlite");
            return None;
        }
    };
    if let Err(error) = connection.busy_timeout(Duration::ZERO) {
        tracing::debug!(path = %path.display(), %error, "failed to configure Codex state sqlite");
        return None;
    }
    Some(connection)
}

fn newest_state_path(home: &Path) -> Option<PathBuf> {
    std::fs::read_dir(home)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((version, path))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

fn thread_enrichment(connection: &Connection, thread_id: &str) -> ThreadEnrichment {
    // Issue 157 includes rollout_path in the compatibility read even though
    // SnapshotSession has no field for it. Keeping this lookup independent
    // ensures drift in that column cannot suppress the fields we do publish.
    let _rollout_path: Option<String> = read_column(
        connection,
        "SELECT rollout_path FROM threads WHERE id = ?1",
        thread_id,
    );
    ThreadEnrichment {
        cwd: read_column(
            connection,
            "SELECT cwd FROM threads WHERE id = ?1",
            thread_id,
        ),
        title: read_column(
            connection,
            "SELECT title FROM threads WHERE id = ?1",
            thread_id,
        ),
        updated_at: read_column::<i64>(
            connection,
            "SELECT updated_at FROM threads WHERE id = ?1",
            thread_id,
        )
        .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
    }
}

fn read_column<T: FromSql>(connection: &Connection, sql: &str, thread_id: &str) -> Option<T> {
    match connection
        .query_row(sql, params![thread_id], |row| row.get(0))
        .optional()
    {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(thread_id, %error, "failed to read Codex thread enrichment field");
            None
        }
    }
}

fn is_locked_by_writer(file: &File, path: &Path) -> bool {
    // SAFETY: `flock` only reads the valid file descriptor and lock flags;
    // the `File` remains alive for the duration of the call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return false;
    }

    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }

    tracing::debug!(path = %path.display(), %error, "skipping unprobeable Codex writer lock");
    false
}

/// Map Codex's last heartbeat to the coarse activity vocabulary this project owns.
fn status_from_updated_at(
    updated_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Status {
    if now.signed_duration_since(updated_at) < chrono::Duration::seconds(ACTIVITY_THRESHOLD_SECONDS)
    {
        Status::Busy { tool: None }
    } else {
        Status::Idle
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn recent_heartbeat_is_busy() {
        let now = Utc::now();

        assert_eq!(
            status_from_updated_at(now - chrono::Duration::seconds(29), now),
            Status::Busy { tool: None }
        );
    }

    #[test]
    fn stale_heartbeat_is_idle_at_the_threshold() {
        let now = Utc::now();

        assert_eq!(
            status_from_updated_at(now - chrono::Duration::seconds(30), now),
            Status::Idle
        );
    }
}
