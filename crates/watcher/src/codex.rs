//! Codex CLI session discovery from its writer-lock directory.
//!
//! Codex 0.147.0 and later holds an exclusive `flock` on
//! `$CODEX_HOME/thread-writer-locks/<thread_id>.lock` for each live thread.
//! These files and their locking behavior are undocumented Codex internals,
//! so every assumption about them is kept in this module.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use common::api::SnapshotSession;
use common::session::Status;

/// Integration-test override for Codex's home directory.
pub const HOME_ENV: &str = "CSM_WATCHER_CODEX_HOME";

fn home() -> Option<PathBuf> {
    std::env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Return the complete minimal snapshot of live Codex CLI threads.
pub fn sweep() -> Vec<SnapshotSession> {
    let Some(home) = home() else {
        tracing::debug!("Codex home is unavailable; publishing an empty Codex snapshot");
        return Vec::new();
    };
    live_sessions(&home)
}

fn live_sessions(home: &Path) -> Vec<SnapshotSession> {
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

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| live_session(&entry.path()))
        .collect()
}

fn live_session(path: &Path) -> Option<SnapshotSession> {
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

    Some(SnapshotSession {
        session_id: thread_id.to_owned(),
        cwd: String::new(),
        status: Status::Idle,
        name: None,
        git_branch: None,
        git_remote: None,
        tmux_target: None,
        model: None,
    })
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
