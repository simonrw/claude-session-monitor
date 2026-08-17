//! pi CLI session discovery from the registry maintained by the optional
//! `contrib/pi/csm-session-monitor.ts` extension.
//!
//! pi itself has no live-session registry. The extension writes the same
//! claim shape as Claude Code under `<agent-dir>/csm/sessions`; this source
//! discovers agent directories from live pi process environments and then
//! delegates parsing, pid/start-time liveness, status mapping, git, and tmux
//! enrichment to the shared registry sweep.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

use common::api::SnapshotSession;

use crate::git::GitCache;

const PI_AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";

/// Test/diagnostic override for pi registry roots. Values are PATH-style and
/// point at directories containing a `sessions` child (normally
/// `<pi-agent-dir>/csm`).
pub const REGISTRY_DIRS_ENV: &str = "CSM_WATCHER_PI_REGISTRY_DIRS";

#[derive(Debug)]
struct PiProcess {
    pid: i32,
    agent_dir: Option<String>,
    home: Option<String>,
    cwd: Option<PathBuf>,
    tmux_pane: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    #[error("failed to discover live pi processes: {0}")]
    Discovery(#[source] std::io::Error),
    #[error(transparent)]
    Registry(#[from] crate::sweep::SweepError),
}

/// Return one complete best-effort pi snapshot.
pub fn sweep(git_cache: &GitCache) -> Result<Vec<SnapshotSession>, SweepError> {
    let override_dirs = registry_dirs_from_env();
    let processes = match imp::processes() {
        Ok(processes) => processes,
        Err(error) if !override_dirs.is_empty() => {
            tracing::debug!(%error, "pi process discovery failed under explicit registry override; continuing without tmux enrichment");
            Vec::new()
        }
        Err(error) => return Err(SweepError::Discovery(error)),
    };

    let registry_dirs = if override_dirs.is_empty() {
        discovered_registry_dirs(&processes)
    } else {
        override_dirs
    };
    let live_pids = processes.iter().map(|process| process.pid).collect();
    let tmux_panes = processes
        .iter()
        .filter_map(|process| {
            process
                .tmux_pane
                .as_ref()
                .map(|pane| (process.pid, pane.clone()))
        })
        .collect();

    Ok(crate::sweep::sweep(
        &registry_dirs,
        &tmux_panes,
        git_cache,
        &live_pids,
        None,
    )?)
}

fn registry_dirs_from_env() -> Vec<PathBuf> {
    std::env::var_os(REGISTRY_DIRS_ENV)
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| path.to_str().is_none_or(|value| !value.trim().is_empty()))
                .collect()
        })
        .unwrap_or_default()
}

fn discovered_registry_dirs(processes: &[PiProcess]) -> Vec<PathBuf> {
    let watcher_home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = HashSet::new();
    if let Some(home) = &watcher_home {
        dirs.insert(home.join(".pi/agent/csm"));
    }
    for process in processes {
        let process_home = absolute_path(process.home.as_deref()).or_else(|| watcher_home.clone());
        let agent_dir = process
            .agent_dir
            .as_deref()
            .and_then(|value| {
                resolve_agent_dir(value, process_home.as_deref(), process.cwd.as_deref())
            })
            .or_else(|| process_home.map(|home| home.join(".pi/agent")));
        if let Some(agent_dir) = agent_dir {
            dirs.insert(agent_dir.join("csm"));
        }
    }
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_unstable();
    dirs
}

fn absolute_path(value: Option<&str>) -> Option<PathBuf> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn resolve_agent_dir(value: &str, home: Option<&Path>, cwd: Option<&Path>) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }

    let path = PathBuf::from(value);
    if path.is_absolute() {
        Some(path)
    } else {
        cwd.map(|cwd| cwd.join(path))
    }
}

fn is_pi_command(tokens: &[String]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    let first_name = Path::new(first)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if first_name == "pi" {
        return true;
    }

    matches!(first_name, "node" | "bun")
        && tokens.get(1).is_some_and(|script| {
            script.contains("pi-coding-agent")
                && matches!(
                    Path::new(script).file_name().and_then(|name| name.to_str()),
                    Some("cli.js" | "main.js" | "pi")
                )
        })
}

#[cfg(any(target_os = "macos", test))]
fn pi_processes_from_ps(
    parsed: Vec<(
        i32,
        u32,
        Vec<String>,
        std::collections::HashMap<String, String>,
    )>,
    current_uid: u32,
    mut process_cwd: impl FnMut(i32) -> Option<PathBuf>,
) -> Vec<PiProcess> {
    let mut processes = Vec::new();
    for (pid, uid, command, env) in parsed {
        if !is_pi_command(&command) {
            continue;
        }
        if env.is_empty() && uid != current_uid {
            continue;
        }
        // pi sets `process.title = "pi"` during startup. On macOS Node's
        // process-title rewrite overwrites the argv/environ memory that
        // `ps -E` reads, so a real same-user pi process normally appears
        // with no environment at all. Keep the pid in the discovery set and
        // fall back to the watcher's default agent directory; rejecting the
        // whole sweep here would make every normal pi invocation invisible.
        let agent_dir = env.get(PI_AGENT_DIR_ENV).cloned();
        let cwd = agent_dir
            .as_deref()
            .filter(|value| !value.starts_with('~') && !Path::new(value).is_absolute())
            .and_then(|_| process_cwd(pid));
        processes.push(PiProcess {
            pid,
            agent_dir,
            home: env.get("HOME").cloned(),
            cwd,
            tmux_pane: env.get("TMUX_PANE").cloned(),
        });
    }
    processes
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    const PS_TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) fn processes() -> std::io::Result<Vec<PiProcess>> {
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
        Ok(pi_processes_from_ps(parsed, current_uid, process_cwd))
    }

    fn process_cwd(pid: i32) -> Option<PathBuf> {
        let pid = pid.to_string();
        let output = crate::command::run(
            "lsof",
            &["-a", "-p", &pid, "-d", "cwd", "-Fn"],
            None,
            PS_TIMEOUT,
        )?;
        output
            .lines()
            .find_map(|line| line.strip_prefix('n'))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub(super) fn processes() -> std::io::Result<Vec<PiProcess>> {
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
                .and_then(|name| name.parse::<i32>().ok())
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
            if !is_pi_command(&command) {
                continue;
            }
            let environ = match std::fs::read(dir.join("environ")) {
                Ok(environ) => environ,
                Err(error) => {
                    let owner_uid = std::fs::metadata(&dir).ok().map(|metadata| {
                        use std::os::unix::fs::MetadataExt;
                        metadata.uid()
                    });
                    if owner_uid.is_some_and(|uid| uid != unsafe { libc::getuid() }) {
                        continue;
                    }
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!(
                            "failed to read environment for same-user pi process {pid}: {error}"
                        ),
                    ));
                }
            };
            let mut env = HashMap::new();
            for entry in environ.split(|byte| *byte == 0) {
                let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
                    continue;
                };
                env.insert(
                    String::from_utf8_lossy(&entry[..separator]).into_owned(),
                    String::from_utf8_lossy(&entry[separator + 1..]).into_owned(),
                );
            }
            processes.push(PiProcess {
                pid,
                agent_dir: env.remove(PI_AGENT_DIR_ENV),
                home: env.remove("HOME"),
                cwd: std::fs::read_link(dir.join("cwd")).ok(),
                tmux_pane: env.remove("TMUX_PANE"),
            });
        }
        Ok(processes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_matcher_is_conservative() {
        assert!(is_pi_command(&["pi".into()]));
        assert!(is_pi_command(&["/opt/homebrew/bin/pi".into()]));
        assert!(is_pi_command(&[
            "node".into(),
            "/opt/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js".into(),
        ]));
        assert!(!is_pi_command(&["pine".into()]));
        assert!(!is_pi_command(&["pilot".into()]));
        assert!(!is_pi_command(&["node".into(), "/tmp/pi.js".into()]));
    }

    #[test]
    fn same_user_pi_with_environment_hidden_by_process_title_still_discovers() {
        let parsed = crate::discovery::parse_ps_output("73387 501 pi\n").unwrap();
        let processes = pi_processes_from_ps(parsed, 501, |_| None);
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 73387);
    }

    #[test]
    fn agent_dir_override_expands_tilde_and_process_relative_paths() {
        let home = Path::new("/Users/pi");
        let cwd = Path::new("/work/project");
        assert_eq!(
            resolve_agent_dir("~/custom-agent", Some(home), Some(cwd)),
            Some(PathBuf::from("/Users/pi/custom-agent"))
        );
        assert_eq!(
            resolve_agent_dir(".pi-agent", Some(home), Some(cwd)),
            Some(PathBuf::from("/work/project/.pi-agent"))
        );
        assert_eq!(
            resolve_agent_dir("/var/lib/pi", Some(home), Some(cwd)),
            Some(PathBuf::from("/var/lib/pi"))
        );
    }
}
