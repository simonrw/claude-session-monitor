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

/// The resolved activation route for a session: local (same host) or remote.
enum Route {
    Local(TmuxTarget),
    Remote {
        hostname: String,
        target: TmuxTarget,
    },
}

/// Parse the session's tmux target and decide between local and remote
/// activation by comparing its hostname to `local_hostname`. Shared by
/// [`activate`] (GUI clients) and [`activate_in_tmux`] (terminal clients) so
/// the routing decision and its logging live in one place.
fn resolve_route(session: &SessionView, local_hostname: &str) -> Result<Route, ActivationError> {
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
        Ok(Route::Local(target))
    } else {
        let hostname = session.hostname.as_deref().ok_or_else(|| {
            tracing::warn!(session_id = %session.session_id, "activate: remote path taken but session has no hostname");
            ActivationError::TmuxFailed("session has no hostname".into())
        })?;
        Ok(Route::Remote {
            hostname: hostname.to_owned(),
            target,
        })
    }
}

/// Activate a session from a GUI client (the Mac apps).
///
/// Local sessions switch the current tmux client; remote sessions spawn a new
/// GUI terminal running SSH via `open`. Terminal clients running inside tmux
/// must use [`activate_in_tmux`] instead - the `open` path cannot foreground a
/// window from tmux's "Background" launchd session.
pub fn activate(session: &SessionView, local_hostname: &str) -> Result<(), ActivationError> {
    match resolve_route(session, local_hostname)? {
        Route::Local(target) => activate_local(&target),
        Route::Remote { hostname, target } => activate_remote(&hostname, &target),
    }
}

/// Activate a session from a terminal client running inside tmux (the TUI).
///
/// Mirrors [`activate`] but keeps everything inside tmux: local sessions use
/// `switch-client` (exactly as [`activate`] does) and remote sessions detach the
/// current client and hand its terminal to `ssh … tmux attach` rather than
/// spawning a GUI terminal. This sidesteps the macOS limitation where a process
/// living in the tmux server's "Background" launchd session launches a GUI
/// terminal but cannot bring its window to the foreground - so the jump appears
/// to do nothing - and avoids nesting the remote tmux inside the local one.
pub fn activate_in_tmux(
    session: &SessionView,
    local_hostname: &str,
) -> Result<(), ActivationError> {
    match resolve_route(session, local_hostname)? {
        Route::Local(target) => activate_local(&target),
        Route::Remote { hostname, target } => activate_remote_tmux(&hostname, &target),
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

/// The remote-side command that selects the stored window/pane and attaches.
/// The `&&` pipeline runs on the remote host, so ssh must hand the whole string
/// to the remote user's shell (and any local wrapper must keep it as one arg).
fn remote_attach_cmd(target: &TmuxTarget) -> String {
    format!(
        "tmux select-window -t {} && tmux select-pane -t {} && tmux attach -t {}",
        target.window_target(),
        target.pane_target(),
        target.session,
    )
}

/// Build the ssh argv for remote activation.
///
/// The trailing entry is the remote command string; ssh hands it to the remote
/// user's shell, which is required because the pipeline uses `&&`.
pub fn build_remote_ssh_argv(hostname: &str, target: &TmuxTarget) -> Vec<String> {
    vec![
        "ssh".to_owned(),
        hostname.to_owned(),
        "-t".to_owned(),
        remote_attach_cmd(target),
    ]
}

/// Build the command handed to `tmux detach-client -E` for remote activation
/// from inside tmux.
///
/// Detaching the local client first means the remote `tmux attach` runs in a
/// terminal that is no longer inside the local tmux, so the two servers never
/// nest. The remote attach pipeline is single-quoted so the layers unwrap
/// cleanly: tmux runs the whole string via `sh -c`, that shell hands the quoted
/// pipeline to ssh as a single argument, and ssh forwards it to the remote shell
/// where the `&&` chain runs. The trailing `; tmux attach` re-attaches the local
/// session once the remote session ends, returning the user where they started.
pub fn build_remote_tmux_command(hostname: &str, target: &TmuxTarget) -> String {
    format!(
        "ssh {hostname} -t '{}' ; tmux attach",
        remote_attach_cmd(target)
    )
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

/// Remote activation for terminal clients: detach the local tmux client and
/// replace it with the SSH attach in the same terminal.
///
/// Unlike [`activate_remote`] this never touches the GUI, so it works from
/// inside the tmux server's "Background" launchd session where launching a GUI
/// terminal cannot foreground its window. And unlike opening the attach in a new
/// local tmux window, detaching first keeps the remote tmux from nesting inside
/// the local one - the freed terminal is no longer a tmux client when ssh runs.
fn activate_remote_tmux(hostname: &str, target: &TmuxTarget) -> Result<(), ActivationError> {
    let client = resolve_most_recent_client().inspect_err(|e| {
        tracing::warn!(error = %e, "activate_remote_tmux: failed to resolve tmux client");
    })?;
    let cmd = build_remote_tmux_command(hostname, target);
    tracing::info!(
        client = %client,
        hostname,
        cmd = %cmd,
        "activate_remote_tmux: detaching client into remote ssh"
    );
    // detach-client names the target client with `-t` (switch-client uses `-c`).
    run_tmux(&["detach-client", "-t", &client, "-E", cmd.as_str()])
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
    fn build_remote_tmux_command_single_quotes_remote_pipeline_and_reattaches() {
        let t = TmuxTarget::parse("dev:1.0").unwrap();
        let cmd = build_remote_tmux_command("myhost", &t);
        assert_eq!(
            cmd,
            "ssh myhost -t 'tmux select-window -t dev:1 && tmux select-pane -t dev:1.0 && tmux attach -t dev' ; tmux attach"
        );
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
                "tmux select-window -t dev:1 && tmux select-pane -t dev:1.0 && tmux attach -t dev"
                    .to_string(),
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
            status: crate::session::Status::Busy { tool: None },
            agent_kind: crate::api::AgentKind::Claude,
            model: None,
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: None,
            git_remote: None,
            tmux_target: None,
            name: None,
        };
        let err = activate(&session, "myhost").unwrap_err();
        assert!(matches!(err, ActivationError::NoTmuxTarget));
    }

    #[test]
    fn activate_returns_invalid_target_for_bad_format() {
        let session = SessionView {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            status: crate::session::Status::Busy { tool: None },
            agent_kind: crate::api::AgentKind::Claude,
            model: None,
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: None,
            git_remote: None,
            tmux_target: Some("bad-format".into()),
            name: None,
        };
        let err = activate(&session, "myhost").unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget(_)));
    }

    #[test]
    fn activate_in_tmux_returns_no_tmux_target_when_none() {
        let session = SessionView {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            status: crate::session::Status::Busy { tool: None },
            agent_kind: crate::api::AgentKind::Claude,
            model: None,
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: None,
            git_remote: None,
            tmux_target: None,
            name: None,
        };
        let err = activate_in_tmux(&session, "myhost").unwrap_err();
        assert!(matches!(err, ActivationError::NoTmuxTarget));
    }

    #[test]
    fn activate_in_tmux_returns_invalid_target_for_bad_format() {
        let session = SessionView {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            status: crate::session::Status::Busy { tool: None },
            agent_kind: crate::api::AgentKind::Claude,
            model: None,
            updated_at: chrono::Utc::now(),
            hostname: Some("myhost".into()),
            git_branch: None,
            git_remote: None,
            tmux_target: Some("bad-format".into()),
            name: None,
        };
        let err = activate_in_tmux(&session, "myhost").unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget(_)));
    }
}
