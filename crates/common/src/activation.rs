use std::process::Command;

use crate::api::SessionView;

/// Errors that can occur during session activation.
#[derive(Debug, thiserror::Error)]
pub enum ActivationError {
    #[error("session has no tmux target")]
    NoTmuxTarget,
    #[error("invalid tmux target format: {0}")]
    InvalidTarget(String),
    #[error("no tmux clients found")]
    NoTmuxClients,
    #[error("tmux command failed: {0}")]
    TmuxFailed(String),
    #[error("failed to launch terminal: {0}")]
    TerminalLaunchFailed(String),
}

/// Parsed components of a tmux target string (`session:window.pane`).
#[derive(Debug, Clone, PartialEq)]
pub struct TmuxTarget {
    pub session: String,
    pub window: String,
    pub pane: String,
}

impl TmuxTarget {
    pub fn parse(target: &str) -> Result<Self, ActivationError> {
        let (session, rest) = target
            .split_once(':')
            .ok_or_else(|| ActivationError::InvalidTarget(target.to_owned()))?;
        let (window, pane) = rest
            .split_once('.')
            .ok_or_else(|| ActivationError::InvalidTarget(target.to_owned()))?;
        Ok(Self {
            session: session.to_owned(),
            window: window.to_owned(),
            pane: pane.to_owned(),
        })
    }

    /// Full window target: `session:window`
    pub fn window_target(&self) -> String {
        format!("{}:{}", self.session, self.window)
    }

    /// Full pane target: `session:window.pane`
    pub fn pane_target(&self) -> String {
        format!("{}:{}.{}", self.session, self.window, self.pane)
    }
}

/// Activate a session by switching to its tmux pane.
///
/// Compares the session's hostname to `local_hostname` to decide between
/// local activation (tmux switch-client) and remote activation (new terminal
/// with SSH).
pub fn activate(session: &SessionView, local_hostname: &str) -> Result<(), ActivationError> {
    tracing::info!(
        session_id = %session.session_id,
        hostname = ?session.hostname,
        tmux_target = ?session.tmux_target,
        local_hostname,
        "activate: request received"
    );

    let target_str = session.tmux_target.as_deref().ok_or_else(|| {
        tracing::warn!(session_id = %session.session_id, "activate: session has no tmux_target");
        ActivationError::NoTmuxTarget
    })?;
    let target = TmuxTarget::parse(target_str).inspect_err(|e| {
        tracing::warn!(target_str, error = %e, "activate: failed to parse tmux target");
    })?;

    let is_local = session
        .hostname
        .as_deref()
        .is_some_and(|h| h == local_hostname);

    tracing::info!(
        session_id = %session.session_id,
        is_local,
        target = ?target,
        "activate: routing"
    );

    if is_local {
        activate_local(&target)
    } else {
        let hostname = session.hostname.as_deref().ok_or_else(|| {
            tracing::warn!(session_id = %session.session_id, "activate: remote path taken but session has no hostname");
            ActivationError::TmuxFailed("session has no hostname".into())
        })?;
        activate_remote(hostname, &target)
    }
}

/// Resolve the most recently active tmux client name.
fn resolve_most_recent_client() -> Result<String, ActivationError> {
    let output = Command::new("tmux")
        .args(["list-clients", "-F", "#{client_activity} #{client_name}"])
        .output()
        .map_err(|e| {
            ActivationError::TmuxFailed(format!("failed to run tmux list-clients: {e}"))
        })?;

    if !output.status.success() {
        return Err(ActivationError::NoTmuxClients);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Lines are "timestamp client_name", pick the one with the highest timestamp.
    stdout
        .lines()
        .filter_map(|line| {
            let (ts, name) = line.split_once(' ')?;
            let ts: u64 = ts.parse().ok()?;
            Some((ts, name.to_owned()))
        })
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, name)| name)
        .ok_or(ActivationError::NoTmuxClients)
}

fn run_tmux(args: &[&str]) -> Result<(), ActivationError> {
    tracing::debug!(args = ?args, "run_tmux: invoking tmux");
    let output = Command::new("tmux").args(args).output().map_err(|e| {
        tracing::error!(args = ?args, error = %e, "run_tmux: failed to spawn tmux");
        ActivationError::TmuxFailed(format!("failed to run tmux: {e}"))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            args = ?args,
            status = ?output.status,
            stderr = %stderr.trim(),
            "run_tmux: tmux returned non-zero"
        );
        return Err(ActivationError::TmuxFailed(stderr.trim().to_owned()));
    }
    Ok(())
}

fn activate_local(target: &TmuxTarget) -> Result<(), ActivationError> {
    let client = resolve_most_recent_client().inspect_err(|e| {
        tracing::warn!(error = %e, "activate_local: failed to resolve tmux client");
    })?;
    tracing::info!(client = %client, target = ?target, "activate_local: switching tmux client");

    // `switch-client -t session:window.pane` does session + window + pane
    // selection in one atomic call. If the stored window/pane indexes are
    // stale (the user renumbered or closed a pane since the last report),
    // fall back to just switching the session so the UI at least takes
    // the user somewhere useful.
    let pane_target = target.pane_target();
    match run_tmux(&["switch-client", "-c", &client, "-t", &pane_target]) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!(
                target = %pane_target,
                error = %e,
                "activate_local: pane target failed, falling back to session"
            );
            run_tmux(&["switch-client", "-c", &client, "-t", &target.session])
        }
    }
}

/// Build the ssh argv for remote activation.
///
/// The trailing entry is the remote command string; ssh hands it to the remote
/// user's shell, which is required because the pipeline uses `&&`.
pub fn build_remote_ssh_argv(hostname: &str, target: &TmuxTarget) -> Vec<String> {
    let remote_cmd = format!(
        "tmux select-window -t {} && tmux select-pane -t {} && tmux attach -t {}",
        target.window_target(),
        target.pane_target(),
        target.session,
    );
    vec![
        "ssh".to_owned(),
        hostname.to_owned(),
        "-t".to_owned(),
        remote_cmd,
    ]
}

/// Build the full terminal launch command for remote activation.
/// Returns (program, args) tuple.
pub fn build_remote_launch_command(hostname: &str, target: &TmuxTarget) -> (String, Vec<String>) {
    let mut ssh_argv = build_remote_ssh_argv(hostname, target);

    #[cfg(target_os = "macos")]
    {
        // `-n` forces a new Ghostty instance; without it `open` just activates
        // the existing app and discards `--args`. Ghostty's `-e` works like
        // xterm's: the remaining argv is program + args, not a shell string.
        let mut args = vec![
            "-n".to_owned(),
            "-a".to_owned(),
            "Ghostty".to_owned(),
            "--args".to_owned(),
            "-e".to_owned(),
        ];
        args.append(&mut ssh_argv);
        ("open".to_owned(), args)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let mut args = vec!["-e".to_owned()];
        args.append(&mut ssh_argv);
        ("ghostty".to_owned(), args)
    }
}

fn activate_remote(hostname: &str, target: &TmuxTarget) -> Result<(), ActivationError> {
    let (program, args) = build_remote_launch_command(hostname, target);

    tracing::info!(
        hostname,
        program = %program,
        args = ?args,
        "activate_remote: spawning terminal"
    );

    match Command::new(&program).args(&args).spawn() {
        Ok(child) => {
            tracing::info!(
                hostname,
                program = %program,
                pid = child.id(),
                "activate_remote: spawn succeeded"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                hostname,
                program = %program,
                args = ?args,
                error = %e,
                "activate_remote: spawn failed"
            );
            Err(ActivationError::TerminalLaunchFailed(format!(
                "failed to launch {program}: {e}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_target() {
        let t = TmuxTarget::parse("main:2.1").unwrap();
        assert_eq!(t.session, "main");
        assert_eq!(t.window, "2");
        assert_eq!(t.pane, "1");
    }

    #[test]
    fn parse_target_with_complex_session_name() {
        let t = TmuxTarget::parse("my-project:0.3").unwrap();
        assert_eq!(t.session, "my-project");
        assert_eq!(t.window, "0");
        assert_eq!(t.pane, "3");
    }

    #[test]
    fn parse_invalid_target_no_colon() {
        assert!(TmuxTarget::parse("invalid").is_err());
    }

    #[test]
    fn parse_invalid_target_no_dot() {
        assert!(TmuxTarget::parse("main:2").is_err());
    }

    #[test]
    fn window_target_format() {
        let t = TmuxTarget::parse("main:2.1").unwrap();
        assert_eq!(t.window_target(), "main:2");
    }

    #[test]
    fn pane_target_format() {
        let t = TmuxTarget::parse("main:2.1").unwrap();
        assert_eq!(t.pane_target(), "main:2.1");
    }

    #[test]
    fn build_remote_ssh_argv_splits_args() {
        let t = TmuxTarget::parse("dev:1.0").unwrap();
        let argv = build_remote_ssh_argv("myhost", &t);
        assert_eq!(
            argv,
            vec![
                "ssh".to_string(),
                "myhost".to_string(),
                "-t".to_string(),
                "tmux select-window -t dev:1 && tmux select-pane -t dev:1.0 && tmux attach -t dev".to_string(),
            ]
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn build_remote_launch_command_macos_uses_open_n() {
        let t = TmuxTarget::parse("main:0.1").unwrap();
        let (program, args) = build_remote_launch_command("server1", &t);
        assert_eq!(program, "open");
        assert_eq!(&args[..5], &["-n", "-a", "Ghostty", "--args", "-e"]);
        assert_eq!(args[5], "ssh");
        assert_eq!(args[6], "server1");
        assert_eq!(args[7], "-t");
        assert!(args[8].contains("tmux attach -t main"));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn build_remote_launch_command_linux_uses_ghostty_direct() {
        let t = TmuxTarget::parse("main:0.1").unwrap();
        let (program, args) = build_remote_launch_command("server1", &t);
        assert_eq!(program, "ghostty");
        assert_eq!(args[0], "-e");
        assert_eq!(args[1], "ssh");
        assert_eq!(args[2], "server1");
        assert_eq!(args[3], "-t");
        assert!(args[4].contains("tmux attach -t main"));
    }

    #[test]
    fn activate_returns_no_tmux_target_when_none() {
        let session = SessionView {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            status: crate::session::Status::Working(crate::session::WorkingStatus { tool: None }),
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };
        let err = activate(&session, "myhost").unwrap_err();
        assert!(matches!(err, ActivationError::NoTmuxTarget));
    }

    #[test]
    fn activate_returns_invalid_target_for_bad_format() {
        let session = SessionView {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            status: crate::session::Status::Working(crate::session::WorkingStatus { tool: None }),
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: None,
            git_remote: None,
            tmux_target: Some("bad-format".into()),
        };
        let err = activate(&session, "myhost").unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget(_)));
    }
}
