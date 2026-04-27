use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const RUN_ID_ENV: &str = "CSM_CODEX_RUN_ID";
const STATE_DIR_ENV: &str = "CSM_CODEX_RUN_STATE_DIR";

pub fn record_session(run_id: &str, session_id: &str) -> io::Result<()> {
    let mut sessions: BTreeSet<String> = read_sessions(run_id)?.into_iter().collect();
    if sessions.insert(session_id.to_owned()) {
        write_sessions(run_id, sessions.iter().map(String::as_str))?;
    }
    Ok(())
}

pub fn read_sessions(run_id: &str) -> io::Result<Vec<String>> {
    let path = path_for_run(run_id);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)?;
    let sessions = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok(sessions)
}

pub fn remove_run(run_id: &str) -> io::Result<()> {
    let path = path_for_run(run_id);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn write_sessions<'a>(
    run_id: &str,
    session_ids: impl IntoIterator<Item = &'a str>,
) -> io::Result<()> {
    let root = state_root();
    fs::create_dir_all(&root)?;
    let path = root.join(file_name_for_run(run_id));
    let mut content = String::new();
    for session_id in session_ids {
        content.push_str(session_id);
        content.push('\n');
    }
    fs::write(path, content)
}

fn path_for_run(run_id: &str) -> PathBuf {
    state_root().join(file_name_for_run(run_id))
}

fn state_root() -> PathBuf {
    if let Ok(path) = std::env::var(STATE_DIR_ENV) {
        return PathBuf::from(path);
    }

    data_dir().join("codex-runs")
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("share")
            .join("claude-session-monitor");
    }

    std::env::temp_dir().join("claude-session-monitor")
}

fn file_name_for_run(run_id: &str) -> String {
    let sanitized: String = run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "unknown".into()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_state_dir(test: impl FnOnce(&Path)) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("csm-codex-run-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var(STATE_DIR_ENV, &dir) };
        test(&dir);
        unsafe { std::env::remove_var(STATE_DIR_ENV) };
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_unique_sessions_for_run() {
        with_temp_state_dir(|_| {
            record_session("run-1", "session-a").unwrap();
            record_session("run-1", "session-a").unwrap();
            record_session("run-1", "session-b").unwrap();

            let sessions = read_sessions("run-1").unwrap();
            assert_eq!(sessions, vec!["session-a", "session-b"]);
        });
    }

    #[test]
    fn run_id_cannot_escape_state_dir() {
        with_temp_state_dir(|dir| {
            record_session("../run", "session-a").unwrap();

            let sessions = read_sessions("../run").unwrap();
            assert_eq!(sessions, vec!["session-a"]);
            assert!(dir.join(".._run").exists());
        });
    }
}
