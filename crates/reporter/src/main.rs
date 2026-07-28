mod codex_enrichment;
mod hook;

use clap::{Parser, ValueEnum};
use common::api::{AgentKind as ReportAgentKind, ReportPayload, resolve_server_url};
use csm_reporter::codex_run_state;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum AgentKind {
    Claude,
    Codex,
}

#[derive(Parser, Debug)]
#[command(
    name = "claude-session-monitor-reporter",
    about = "Claude session monitor reporter (Codex hooks only - Claude Code sessions are tracked by csm-watcher)"
)]
#[command(version, about = "Claude Session Monitor Reporter")]
struct Args {
    /// Server URL (e.g. http://localhost:7685)
    #[arg(long)]
    server_url: Option<String>,

    /// Agent hook payload format. Only `codex` is accepted; `claude` (the
    /// default, so a bare invocation with no --agent flag - i.e. a stale
    /// Claude Code hook - is rejected too) exits non-zero pointing at
    /// csm-watcher.
    #[arg(long, value_enum, default_value_t = AgentKind::Claude)]
    agent: AgentKind,
}

/// Message printed (and logged) when `--agent claude` is used, or implied by
/// a bare invocation with no `--agent` flag at all - which is exactly what a
/// stale Claude Code hook does, since it was never told to pass `--agent`.
///
/// Claude Code sessions are tracked by `csm-watcher` polling the session
/// registry now (PRO-204/PRO-213), not by hooks. Accepting Claude events
/// here would let two mechanisms race to report the same session, so this
/// rejects outright instead of silently doing nothing or double-reporting.
const CLAUDE_REJECTION_MESSAGE: &str = "csm-reporter: --agent claude is no longer supported. \
Claude Code sessions are tracked by csm-watcher (a polling daemon), not by hooks. \
Remove the old csm-reporter hooks / session-monitor plugin from Claude Code \
(see README.md's \"Upgrading from the hook-based setup\") and run csm-watcher instead. \
Codex sessions are unaffected: keep using `csm-reporter --agent codex`.";

fn setup_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join(".local/share/claude-session-monitor");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "reporter.log");
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

fn main() {
    // Install sentry's panic hook before tracing_subscriber so the chain is:
    // sentry hook -> previous (default) hook. tracing's init won't clobber it.
    let _sentry = common::sentry::init("reporter");

    let args = Args::parse();
    let _guard = setup_tracing();

    if args.agent == AgentKind::Claude {
        tracing::error!("{CLAUDE_REJECTION_MESSAGE}");
        eprintln!("{CLAUDE_REJECTION_MESSAGE}");
        // Drop the tracing guard explicitly before exiting: `process::exit`
        // skips `Drop`, and the non-blocking appender's guard is what
        // flushes buffered log lines to disk. Without this, this rejection
        // - the whole point of which is to be visible in reporter.log - can
        // race the buffer and never make it to the file.
        drop(_guard);
        std::process::exit(1);
    }

    let config = match common::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let input = match read_stdin() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to read stdin");
            return;
        }
    };

    let event = match hook::parse_hook_event(&input) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "failed to parse hook event JSON");
            return;
        }
    };

    let span = tracing::info_span!(
        "report",
        session_id = %event.session_id,
        cwd = %event.cwd,
        hook_event_name = %event.hook_event_name,
    );
    let _enter = span.enter();

    tracing::debug!("processing hook event");

    let enrichment = codex_enrichment::gather(&event.cwd);
    let payload = build_report_payload(event, enrichment);
    tracing::debug!(status = ?payload.status, "derived status");
    record_codex_run_session(&payload);

    let url = format!(
        "{}/api/sessions",
        resolve_server_url(args.server_url.as_deref(), Some(&config.server.url))
    );
    tracing::debug!(url = %url, "posting to server");
    let result = reqwest::blocking::Client::new()
        .post(&url)
        .json(&payload)
        .send();
    match result {
        Ok(resp) => tracing::debug!(status = %resp.status(), "server responded"),
        Err(e) => report_post_failure(&e),
    }
}

fn record_codex_run_session(payload: &ReportPayload) {
    if payload.agent_kind != ReportAgentKind::Codex {
        return;
    }

    let Ok(run_id) = std::env::var(codex_run_state::RUN_ID_ENV) else {
        return;
    };

    if let Err(e) = codex_run_state::record_session(&run_id, &payload.session_id) {
        tracing::warn!(
            error = %e,
            session_id = %payload.session_id,
            "failed to record Codex wrapper run session"
        );
    }
}

/// Log a POST failure and forward it to Sentry (no-op without the feature).
///
/// Kept as a single funnel so every HTTP failure path captures consistently.
/// Returning cleanly (no panic) preserves existing graceful-continue behaviour.
fn report_post_failure(err: &reqwest::Error) {
    tracing::error!(error = %err, "failed to post to server");
    common::sentry::capture_error(err);
}

/// Build the payload posted for a Codex hook event.
///
/// Enrichment (hostname, git branch/remote, tmux target) is gathered by
/// `codex_enrichment`, a copy kept solely for this frozen, deprecated Codex
/// path - it is not shared with `csm-watcher`'s own enrichment, which is the
/// supported mechanism for Claude Code going forward and goes away
/// entirely once Codex support is removed.
fn build_report_payload(
    event: hook::NormalizedHookEvent,
    enrichment: codex_enrichment::Enrichment,
) -> ReportPayload {
    let status = hook::derive_status(&event);

    ReportPayload {
        session_id: event.session_id,
        cwd: enrichment.cwd,
        status,
        agent_kind: event.agent_kind,
        model: event.model,
        hook_event_name: event.hook_event_name,
        tool_name: event.tool_name,
        tool_input: event.tool_input,
        // `NormalizedHookEvent` no longer carries `notification_type` (see
        // `hook.rs`): Codex, the only agent this reporter still parses,
        // never populated it, and the one thing it drove -
        // `Notification`'s permission_prompt branch in `derive_status` -
        // was removed with it (see PRO-214). `ReportPayload`'s own field
        // stays only for wire compatibility with the legacy hook-report
        // shape.
        notification_type: None,
        hostname: enrichment.hostname,
        git_branch: enrichment.git_branch,
        git_remote: enrichment.git_remote,
        tmux_target: enrichment.tmux_target,
    }
}

fn read_stdin() -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_server_url_arg() {
        let args = Args::parse_from(["csm-reporter", "--server-url", "http://custom:1234"]);
        assert_eq!(args.server_url, Some("http://custom:1234".into()));
    }

    #[test]
    fn defaults_server_url_to_none_and_agent_to_claude() {
        let args = Args::parse_from(["csm-reporter"]);
        assert_eq!(args.server_url, None);
        assert_eq!(args.agent, AgentKind::Claude);
    }

    #[test]
    fn parse_codex_agent_arg() {
        let args = Args::parse_from(["csm-reporter", "--agent", "codex"]);
        assert_eq!(args.agent, AgentKind::Codex);
    }

    #[test]
    fn parse_explicit_claude_agent_arg() {
        let args = Args::parse_from(["csm-reporter", "--agent", "claude"]);
        assert_eq!(args.agent, AgentKind::Claude);
    }

    #[test]
    fn codex_payload_includes_agent_kind_and_model() {
        let event = hook::parse_hook_event(
            r#"{
                "session_id": "codex-session",
                "cwd": "/work/project",
                "hook_event_name": "SessionStart",
                "model": "gpt-5.1-codex"
            }"#,
        )
        .unwrap();
        let enrichment = codex_enrichment::Enrichment {
            cwd: "/work/project".into(),
            hostname: None,
            git_branch: None,
            git_remote: None,
            tmux_target: None,
        };

        let payload = build_report_payload(event, enrichment);

        assert_eq!(payload.agent_kind, ReportAgentKind::Codex);
        assert_eq!(payload.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(payload.cwd, "/work/project");
        assert_eq!(payload.git_branch, None);
        assert_eq!(payload.git_remote, None);
        assert_eq!(payload.tmux_target, None);
    }
}
