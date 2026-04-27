use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use common::api::resolve_server_url;
use csm_reporter::codex_run_state;

struct Args {
    codex_bin: Option<PathBuf>,
    server_url: Option<String>,
    codex_args: Vec<OsString>,
}

fn main() -> ExitCode {
    let _sentry = common::sentry::init("reporter");
    let _guard = setup_tracing();

    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            tracing::error!(error = %e, "csm-codex failed");
            eprintln!("csm-codex: {e}");
            ExitCode::from(1)
        }
    }
}

fn run() -> io::Result<u8> {
    let args = parse_args(std::env::args_os().skip(1))?;
    let codex_bin = resolve_codex_bin(args.codex_bin.as_deref())?;
    let server_url = resolve_monitor_url(args.server_url.as_deref());
    let run_id = new_run_id();

    tracing::info!(
        codex_bin = %codex_bin.display(),
        run_id,
        "starting Codex through wrapper"
    );

    let mut child = Command::new(&codex_bin)
        .args(&args.codex_args)
        .env(codex_run_state::RUN_ID_ENV, &run_id)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    let _signal_forwarder = install_signal_forwarding(child.id());
    let status = child.wait()?;
    let exit_code = process_exit_code(status);

    end_recorded_sessions(&server_url, &run_id);

    Ok(exit_code)
}

fn setup_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join(".local/share/claude-session-monitor");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "codex-wrapper.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("csm_reporter=debug")),
        )
        .init();

    guard
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
    let mut codex_bin = std::env::var_os("CSM_CODEX_BIN").map(PathBuf::from);
    let mut server_url = None;
    let mut codex_args = Vec::new();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if arg == "--" {
            codex_args.extend(iter);
            break;
        }

        if arg == "--codex-bin" {
            let value = iter.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--codex-bin requires a value")
            })?;
            codex_bin = Some(PathBuf::from(value));
            continue;
        }

        if arg == "--server-url" {
            let value = iter.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--server-url requires a value")
            })?;
            server_url = Some(value.to_string_lossy().into_owned());
            continue;
        }

        if let Some(value) = split_option(&arg, "--codex-bin=") {
            codex_bin = Some(PathBuf::from(value));
            continue;
        }

        if let Some(value) = split_option(&arg, "--server-url=") {
            server_url = Some(value.to_string_lossy().into_owned());
            continue;
        }

        codex_args.push(arg);
    }

    Ok(Args {
        codex_bin,
        server_url,
        codex_args,
    })
}

fn split_option(arg: &OsString, prefix: &str) -> Option<OsString> {
    let arg = arg.to_string_lossy();
    arg.strip_prefix(prefix).map(OsString::from)
}

fn resolve_monitor_url(cli_arg: Option<&str>) -> String {
    let file_url = common::config::load().ok().map(|c| c.server.url);
    resolve_server_url(cli_arg, file_url.as_deref())
}

fn resolve_codex_bin(explicit: Option<&Path>) -> io::Result<PathBuf> {
    if let Some(path) = explicit {
        reject_self(path)?;
        return Ok(path.to_path_buf());
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        for candidate in codex_candidates(&dir) {
            if candidate.is_file() && !is_self(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not find a real codex executable on PATH; set CSM_CODEX_BIN or pass --codex-bin",
    ))
}

fn codex_candidates(dir: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![
            dir.join("codex.exe"),
            dir.join("codex.cmd"),
            dir.join("codex.bat"),
        ]
    }

    #[cfg(not(windows))]
    {
        vec![dir.join("codex")]
    }
}

fn reject_self(path: &Path) -> io::Result<()> {
    if is_self(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to launch wrapper as Codex binary: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn is_self(path: &Path) -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    same_file(&current, path)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn new_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn end_recorded_sessions(server_url: &str, run_id: &str) {
    let sessions = match codex_run_state::read_sessions(run_id) {
        Ok(sessions) => sessions,
        Err(e) => {
            tracing::warn!(error = %e, run_id, "failed to read Codex run sessions");
            return;
        }
    };

    if sessions.is_empty() {
        tracing::debug!(run_id, "no Codex sessions recorded for run");
        let _ = codex_run_state::remove_run(run_id);
        return;
    }

    let client = reqwest::blocking::Client::new();
    for session_id in sessions {
        let url = format!("{server_url}/api/sessions/{session_id}/end");
        let mut ended = false;
        for attempt in 1..=3 {
            match client.post(&url).send() {
                Ok(resp)
                    if resp.status().is_success()
                        || resp.status() == reqwest::StatusCode::NOT_FOUND =>
                {
                    tracing::debug!(session_id, status = %resp.status(), "ended Codex session");
                    ended = true;
                    break;
                }
                Ok(resp) => {
                    tracing::warn!(
                        session_id,
                        attempt,
                        status = %resp.status(),
                        "server rejected Codex session end"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        session_id,
                        attempt,
                        error = %e,
                        "failed to end Codex session"
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        if !ended {
            tracing::error!(session_id, "failed to end Codex session after retries");
        }
    }

    if let Err(e) = codex_run_state::remove_run(run_id) {
        tracing::warn!(error = %e, run_id, "failed to remove Codex run state");
    }
}

#[cfg(unix)]
fn install_signal_forwarding(child_pid: u32) -> io::Result<std::thread::JoinHandle<()>> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let handle = std::thread::spawn(move || {
        for signal in signals.forever() {
            unsafe {
                libc::kill(child_pid as libc::pid_t, signal);
            }
        }
    });
    Ok(handle)
}

#[cfg(not(unix))]
fn install_signal_forwarding(_child_pid: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn process_exit_code(status: std::process::ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt;

    if let Some(code) = status.code() {
        return code.min(255) as u8;
    }
    if let Some(signal) = status.signal() {
        return (128 + signal).min(255) as u8;
    }
    1
}

#[cfg(not(unix))]
fn process_exit_code(status: std::process::ExitStatus) -> u8 {
    status.code().unwrap_or(1).min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_consumes_wrapper_flags_and_preserves_codex_args() {
        let args = parse_args([
            OsString::from("--codex-bin"),
            OsString::from("/usr/bin/codex"),
            OsString::from("--server-url=http://localhost:9999"),
            OsString::from("-m"),
            OsString::from("gpt-5.5"),
            OsString::from("--"),
            OsString::from("--codex-bin"),
        ])
        .unwrap();

        assert_eq!(args.codex_bin, Some(PathBuf::from("/usr/bin/codex")));
        assert_eq!(args.server_url.as_deref(), Some("http://localhost:9999"));
        assert_eq!(
            args.codex_args,
            vec![
                OsString::from("-m"),
                OsString::from("gpt-5.5"),
                OsString::from("--codex-bin")
            ]
        );
    }
}
