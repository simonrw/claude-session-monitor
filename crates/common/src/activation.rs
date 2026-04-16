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
    let target_str = session
        .tmux_target
        .as_deref()
        .ok_or(ActivationError::NoTmuxTarget)?;
    let target = TmuxTarget::parse(target_str)?;

    let is_local = session
        .hostname
        .as_deref()
        .is_some_and(|h| h == local_hostname);

    if is_local {
        activate_local(&target)
    } else {
        let hostname = session
            .hostname
            .as_deref()
            .ok_or_else(|| ActivationError::TmuxFailed("session has no hostname".into()))?;
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
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| ActivationError::TmuxFailed(format!("failed to run tmux: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ActivationError::TmuxFailed(stderr.trim().to_owned()));
    }
    Ok(())
}

fn activate_local(target: &TmuxTarget) -> Result<(), ActivationError> {
    let client = resolve_most_recent_client()?;

    run_tmux(&["switch-client", "-c", &client, "-t", &target.session])?;
    run_tmux(&["select-window", "-t", &target.window_target()])?;
    run_tmux(&["select-pane", "-t", &target.pane_target()])?;

    Ok(())
}

/// Build the SSH command string for remote activation.
pub fn build_remote_ssh_command(hostname: &str, target: &TmuxTarget) -> String {
    format!(
        "ssh {} -t \"tmux select-window -t {} && tmux select-pane -t {} && tmux attach -t {}\"",
        hostname,
        target.window_target(),
        target.pane_target(),
        target.session,
    )
}

/// Build the full terminal launch command for remote activation.
/// Returns (program, args) tuple.
pub fn build_remote_launch_command(hostname: &str, target: &TmuxTarget) -> (String, Vec<String>) {
    let ssh_cmd = build_remote_ssh_command(hostname, target);

    #[cfg(target_os = "macos")]
    {
        (
            "open".to_owned(),
            vec![
                "-a".to_owned(),
                "Ghostty".to_owned(),
                "--args".to_owned(),
                "-e".to_owned(),
                ssh_cmd,
            ],
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        ("ghostty".to_owned(), vec!["-e".to_owned(), ssh_cmd])
    }
}

fn activate_remote(hostname: &str, target: &TmuxTarget) -> Result<(), ActivationError> {
    let (program, args) = build_remote_launch_command(hostname, target);

    Command::new(&program).args(&args).spawn().map_err(|e| {
        ActivationError::TerminalLaunchFailed(format!("failed to launch {program}: {e}"))
    })?;

    Ok(())
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
    fn build_remote_ssh_command_format() {
        let t = TmuxTarget::parse("dev:1.0").unwrap();
        let cmd = build_remote_ssh_command("myhost", &t);
        assert_eq!(
            cmd,
            "ssh myhost -t \"tmux select-window -t dev:1 && tmux select-pane -t dev:1.0 && tmux attach -t dev\""
        );
    }

    #[test]
    fn build_remote_launch_command_linux() {
        let t = TmuxTarget::parse("main:0.1").unwrap();
        let (program, args) = build_remote_launch_command("server1", &t);

        // On Linux (the test environment), this should use ghostty directly
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(program, "ghostty");
            assert_eq!(args[0], "-e");
            assert!(args[1].starts_with("ssh server1"));
        }

        // On macOS, this should use `open -a Ghostty`
        #[cfg(target_os = "macos")]
        {
            assert_eq!(program, "open");
            assert_eq!(args[0], "-a");
            assert_eq!(args[1], "Ghostty");
            assert_eq!(args[2], "--args");
            assert_eq!(args[3], "-e");
            assert!(args[4].starts_with("ssh server1"));
        }
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
