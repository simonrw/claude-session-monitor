//! Codex CLI session discovery from its writer-lock directory.
//!
//! Codex 0.147.0 and later holds an exclusive `flock` on
//! `$CODEX_HOME/thread-writer-locks/<thread_id>.lock` for each live thread.
//! These files and their locking behavior are undocumented Codex internals,
//! so every assumption about them is kept in this module.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::BufRead;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::api::SnapshotSession;
use common::session::Status;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::FromSql};

use crate::git::{GitCache, GitInfo};

const ACTIVITY_THRESHOLD_SECONDS: i64 = 30;
const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Integration-test override for Codex's home directory.
pub const HOME_ENV: &str = "CSM_WATCHER_CODEX_HOME";

fn default_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
}

fn homes() -> std::io::Result<Vec<PathBuf>> {
    let default = default_home();
    let override_home = std::env::var_os(HOME_ENV).map(PathBuf::from);
    if override_home.is_some() {
        return Ok(resolved_homes(override_home, default, Vec::new()));
    }
    let processes = imp::process_environments()?;
    Ok(resolved_homes(None, default, processes))
}

fn resolved_homes(
    override_home: Option<PathBuf>,
    default: Option<PathBuf>,
    processes: Vec<ProcessEnvironment>,
) -> Vec<PathBuf> {
    if let Some(home) = override_home {
        return vec![home];
    }

    let mut homes = HashSet::new();
    if let Some(home) = &default {
        homes.insert(home.clone());
    }
    for process in processes {
        let home = absolute_path(process.codex_home.as_deref())
            .or_else(|| absolute_path(process.home.as_deref()).map(|p| p.join(".codex")))
            .or_else(|| default.clone());
        if let Some(home) = home {
            homes.insert(home);
        }
    }
    let mut homes: Vec<_> = homes.into_iter().collect();
    homes.sort_unstable();
    homes
}

fn absolute_path(value: Option<&str>) -> Option<PathBuf> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Return the complete best-effort snapshot of live Codex CLI threads.
pub fn sweep(git_cache: &GitCache) -> std::io::Result<Vec<SnapshotSession>> {
    let homes = homes()?;
    if homes.is_empty() {
        tracing::debug!("Codex home is unavailable; publishing an empty Codex snapshot");
        return Ok(Vec::new());
    }

    Ok(sweep_homes(&homes, git_cache))
}

fn sweep_homes(homes: &[PathBuf], git_cache: &GitCache) -> Vec<SnapshotSession> {
    let mut missing_locks = Vec::new();
    let mut seen = HashSet::new();
    let sessions = homes
        .iter()
        .flat_map(|home| live_sessions(home, git_cache, &mut missing_locks))
        .filter(|session| seen.insert(session.session_id.clone()))
        .collect();
    if !missing_locks.is_empty() {
        tracing::debug!(
            homes = ?missing_locks,
            "Codex writer-lock directory is absent; publishing no sessions from those homes"
        );
    }
    sessions
}

fn live_sessions(
    home: &Path,
    git_cache: &GitCache,
    missing_locks: &mut Vec<PathBuf>,
) -> Vec<SnapshotSession> {
    let locks_dir = home.join("thread-writer-locks");
    let entries = match std::fs::read_dir(&locks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            missing_locks.push(home.to_owned());
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
        .filter_map(|entry| live_session(&entry.path(), home, state.as_ref(), git_cache))
        .collect()
}

#[derive(Debug)]
struct ProcessEnvironment {
    codex_home: Option<String>,
    home: Option<String>,
}

fn is_codex_command(tokens: &[String]) -> bool {
    fn is_codex_name(token: &str) -> bool {
        Path::new(token)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.eq_ignore_ascii_case("codex")
                    || stem.to_ascii_lowercase().starts_with("codex-")
            })
    }

    tokens.first().is_some_and(|token| is_codex_name(token))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    const PS_TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) fn process_environments() -> std::io::Result<Vec<ProcessEnvironment>> {
        let output = crate::command::run(
            "ps",
            &["-Eww", "-ax", "-o", "pid=,uid=,command="],
            None,
            PS_TIMEOUT,
        )
        .ok_or_else(|| std::io::Error::other("failed to enumerate processes with ps"))?;
        let parsed = crate::discovery::parse_ps_output(&output)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let current_uid = unsafe { libc::getuid() };
        let mut processes = Vec::new();
        for (pid, uid, command, env) in parsed {
            if !is_codex_command(&command) {
                continue;
            }
            if env.is_empty() {
                if uid == current_uid {
                    return Err(std::io::Error::other(format!(
                        "ps reported no environment for same-user Codex process {pid}"
                    )));
                }
                tracing::debug!(
                    pid,
                    "skipping foreign-user Codex process with unreadable environment"
                );
                continue;
            }
            processes.push(ProcessEnvironment {
                codex_home: env.get(CODEX_HOME_ENV).cloned(),
                home: env.get("HOME").cloned(),
            });
        }
        Ok(processes)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub(super) fn process_environments() -> std::io::Result<Vec<ProcessEnvironment>> {
        let proc_root = std::env::var_os("CSM_WATCHER_PROC_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/proc"));
        let mut processes = Vec::new();
        for entry in std::fs::read_dir(proc_root)? {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            let Ok(cmdline) = std::fs::read(dir.join("cmdline")) else {
                continue;
            };
            let command: Vec<String> = cmdline
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect();
            if !is_codex_command(&command) {
                continue;
            }
            let environ = match std::fs::read(dir.join("environ")) {
                Ok(environ) => environ,
                Err(error) => {
                    let owner_uid = std::fs::metadata(&dir).ok().map(|metadata| {
                        use std::os::unix::fs::MetadataExt;
                        metadata.uid()
                    });
                    let current_uid = unsafe { libc::getuid() };
                    if owner_uid.is_some_and(|uid| uid != current_uid) {
                        tracing::debug!(pid, %error, "skipping foreign-user Codex process with unreadable environment");
                        continue;
                    }
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!(
                            "failed to read environment for same-user Codex process {pid}: {error}"
                        ),
                    ));
                }
            };
            let mut codex_home = None;
            let mut home = None;
            for entry in environ.split(|byte| *byte == 0) {
                let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
                    continue;
                };
                let (key, value) = entry.split_at(separator);
                let value = &value[1..];
                let value = String::from_utf8_lossy(value).into_owned();
                if key == CODEX_HOME_ENV.as_bytes() {
                    codex_home = Some(value);
                } else if key == b"HOME" {
                    home = Some(value);
                }
            }
            processes.push(ProcessEnvironment { codex_home, home });
        }
        Ok(processes)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub(super) fn process_environments() -> std::io::Result<Vec<ProcessEnvironment>> {
        Ok(Vec::new())
    }
}

fn live_session(
    path: &Path,
    home: &Path,
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

    let mut enrichment = state.map_or_else(ThreadEnrichment::default, |connection| {
        thread_enrichment(connection, thread_id)
    });
    if enrichment.is_subagent {
        tracing::debug!(thread_id, "skipping Codex subagent writer lock");
        return None;
    }
    let rollout_path = enrichment
        .rollout_path
        .clone()
        .filter(|path| !path.is_empty())
        .or_else(|| find_rollout(home, thread_id));
    if let Some(rollout_path) = rollout_path.as_deref() {
        let rollout = rollout_enrichment(Path::new(rollout_path));
        if enrichment.cwd.is_none() {
            enrichment.cwd = rollout.cwd;
        }
        if enrichment.updated_at.is_none() {
            enrichment.updated_at = rollout.updated_at;
        }
    }
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

fn find_rollout(home: &Path, thread_id: &str) -> Option<String> {
    let sessions = home.join("sessions");
    let mut directories = vec![sessions];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(directory).ok()?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(thread_id))
            {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[derive(Default)]
struct ThreadEnrichment {
    is_subagent: bool,
    rollout_path: Option<String>,
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
    let rollout_path: Option<String> = read_column(
        connection,
        "SELECT rollout_path FROM threads WHERE id = ?1",
        thread_id,
    );
    ThreadEnrichment {
        is_subagent: read_column::<String>(
            connection,
            "SELECT source FROM threads WHERE id = ?1",
            thread_id,
        )
        .is_some_and(|source| source_is_subagent(&source)),
        rollout_path,
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

fn source_is_subagent(source: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(source)
        .ok()
        .is_some_and(|source| source.get("subagent").is_some())
}

#[derive(Default)]
struct RolloutEnrichment {
    cwd: Option<String>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn rollout_enrichment(path: &Path) -> RolloutEnrichment {
    if path.extension().and_then(|extension| extension.to_str()) == Some("zst") {
        return RolloutEnrichment::default();
    }

    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "failed to open Codex rollout");
            return RolloutEnrichment::default();
        }
    };
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(file);
    if reader.read_line(&mut line).is_err() {
        return RolloutEnrichment::default();
    }
    let record: serde_json::Value = match serde_json::from_str(&line) {
        Ok(record) => record,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "failed to parse Codex rollout header");
            return RolloutEnrichment::default();
        }
    };
    if record.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return RolloutEnrichment::default();
    }
    let payload = record.get("payload").unwrap_or(&record);
    let cwd = payload
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_owned);
    let updated_at = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from);
    RolloutEnrichment { cwd, updated_at }
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
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::sync::{Arc, Mutex};

    use chrono::Utc;

    use super::*;

    #[derive(Clone)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn explicit_test_home_bypasses_process_discovery_and_the_default() {
        let homes = resolved_homes(
            Some(PathBuf::from("/tmp/override")),
            Some(PathBuf::from("/tmp/default")),
            vec![ProcessEnvironment {
                codex_home: Some("/tmp/discovered".into()),
                home: Some("/tmp/process-home".into()),
            }],
        );

        assert_eq!(homes, vec![PathBuf::from("/tmp/override")]);
    }

    #[test]
    fn no_processes_falls_back_to_the_default_codex_home() {
        let homes = resolved_homes(None, Some(PathBuf::from("/tmp/home/.codex")), Vec::new());

        assert_eq!(homes, vec![PathBuf::from("/tmp/home/.codex")]);
    }

    #[test]
    fn process_home_is_used_when_codex_home_is_unset() {
        let homes = resolved_homes(
            None,
            Some(PathBuf::from("/tmp/default/.codex")),
            vec![ProcessEnvironment {
                codex_home: None,
                home: Some("/tmp/process-home".into()),
            }],
        );

        assert_eq!(
            homes,
            vec![
                PathBuf::from("/tmp/default/.codex"),
                PathBuf::from("/tmp/process-home/.codex"),
            ]
        );
    }

    #[test]
    fn missing_writer_lock_directories_emit_one_debug_note() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = LogBuffer(Arc::clone(&logs));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_target(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let git_cache = GitCache::new(crate::git::DEFAULT_TTL, crate::git::DEFAULT_COMMAND_TIMEOUT);

        let sessions = sweep_homes(
            &[first.path().to_owned(), second.path().to_owned()],
            &git_cache,
        );

        assert!(sessions.is_empty());
        let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert_eq!(
            logs.matches("Codex writer-lock directory is absent")
                .count(),
            1,
            "expected exactly one diagnostic, got {logs:?}"
        );
        assert!(logs.contains(first.path().to_str().unwrap()));
        assert!(logs.contains(second.path().to_str().unwrap()));
    }

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

    #[test]
    fn internal_writer_locks_are_not_published_as_sessions() {
        let home = tempfile::tempdir().unwrap();
        let locks = home.path().join("thread-writer-locks");
        std::fs::create_dir(&locks).unwrap();
        let user_thread_id = "019fe837-14f7-7162-a5cd-2241b18f8316";
        let internal_thread_id = "019fe837-1577-7172-90c2-f6e7b55419de";
        let lock = |thread_id: &str| {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(locks.join(format!("{thread_id}.lock")))
                .unwrap();
            // SAFETY: the descriptor belongs to `file` and remains open for
            // the duration of the sweep below.
            assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
            file
        };
        let _user_lock = lock(user_thread_id);
        let _internal_lock = lock(internal_thread_id);

        let connection = Connection::open(home.path().join("state_1.sqlite")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    rollout_path TEXT NOT NULL,
                    cwd TEXT NOT NULL,
                    title TEXT NOT NULL,
                    updated_at INTEGER NOT NULL,
                    source TEXT NOT NULL
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path, cwd, title, updated_at, source)
                 VALUES (?1, '', '/tmp/project', 'User thread', 0, 'cli')",
                [user_thread_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path, cwd, title, updated_at, source)
                 VALUES (?1, '', '/tmp/project', 'Guardian thread', 0,
                         '{\"subagent\":{\"other\":\"guardian\"}}')",
                [internal_thread_id],
            )
            .unwrap();
        drop(connection);

        let git_cache = GitCache::new(crate::git::DEFAULT_TTL, crate::git::DEFAULT_COMMAND_TIMEOUT);
        let sessions = sweep_homes(&[home.path().to_owned()], &git_cache);

        assert_eq!(
            sessions.len(),
            1,
            "internal lock created a duplicate session"
        );
        assert_eq!(sessions[0].session_id, user_thread_id);
    }

    #[test]
    fn writer_lock_remains_authoritative_when_state_is_unavailable() {
        let home = tempfile::tempdir().unwrap();
        let locks = home.path().join("thread-writer-locks");
        std::fs::create_dir(&locks).unwrap();
        let thread_id = "019fe837-14f7-7162-a5cd-2241b18f8316";
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(locks.join(format!("{thread_id}.lock")))
            .unwrap();
        // SAFETY: the descriptor belongs to `lock` and remains open for the
        // duration of the sweep below.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

        let git_cache = GitCache::new(crate::git::DEFAULT_TTL, crate::git::DEFAULT_COMMAND_TIMEOUT);
        let sessions = sweep_homes(&[home.path().to_owned()], &git_cache);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, thread_id);
    }

    #[test]
    fn source_classification_excludes_only_subagents() {
        assert!(source_is_subagent(r#"{"subagent":{"other":"guardian"}}"#));
        assert!(source_is_subagent(
            r#"{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}"#
        ));
        assert!(!source_is_subagent("cli"));
        assert!(!source_is_subagent("vscode"));
        assert!(!source_is_subagent("unknown"));
    }

    #[test]
    fn rollout_header_provides_cwd_and_mtime() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-08-08T12:00:00Z","type":"session_meta","payload":{"cwd":"/tmp/codex"}}
{"type":"event_msg"}
"#,
        )
        .unwrap();

        let enrichment = rollout_enrichment(&path);

        assert_eq!(enrichment.cwd.as_deref(), Some("/tmp/codex"));
        assert!(enrichment.updated_at.is_some());
    }

    #[test]
    fn rollout_lookup_finds_unindexed_thread() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("sessions/2026/08/08");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("rollout-019c2f61-4a77-78d9-a119-573c21704eb1.jsonl");
        std::fs::write(&path, "{}").unwrap();

        assert_eq!(
            find_rollout(home.path(), "019c2f61-4a77-78d9-a119-573c21704eb1"),
            Some(path.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn compressed_or_malformed_rollouts_are_empty() {
        let home = tempfile::tempdir().unwrap();
        let malformed = home.path().join("rollout.jsonl");
        std::fs::write(&malformed, "{\"type\":\"session_meta\"").unwrap();
        let compressed = home.path().join("rollout.jsonl.zst");
        std::fs::write(&compressed, b"not compressed").unwrap();

        assert!(rollout_enrichment(&malformed).cwd.is_none());
        assert!(rollout_enrichment(&malformed).updated_at.is_none());
        assert!(rollout_enrichment(&compressed).cwd.is_none());
        assert!(rollout_enrichment(&compressed).updated_at.is_none());
    }
}
