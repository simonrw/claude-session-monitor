# session-monitor (Claude Code plugin)

Auto-registers the [Claude Session Monitor](https://github.com/simonrw/claude-session-monitor)
lifecycle hooks for Claude Code. Installing this plugin replaces the manual
`~/.claude/settings.json` hook editing described in the project README.

## What it does

The plugin registers `csm-report.sh` for all seven Claude Code hook events
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Notification`,
`Stop`, `SessionEnd`). On each event the wrapper forwards the hook JSON to the
`csm-reporter` binary, which enriches it and POSTs the session status to the
`csm-server`.

## Prerequisites

The plugin does not ship or download a binary. Install `csm-reporter` once:

```sh
cargo install --path crates/reporter --locked
```

The wrapper resolves the binary in this order:

1. `$CSM_REPORTER_BIN` (if set and executable)
2. `csm-reporter` on `PATH`
3. `$CARGO_HOME/bin/csm-reporter` (or `~/.cargo/bin/csm-reporter`)

If none is found, the wrapper soft-fails (exits 0) so it never blocks the agent,
and notes it in `~/.local/share/claude-session-monitor/reporter.log`.

Point the reporter at a non-default server with `CLAUDE_MONITOR_URL`
(default `http://localhost:7685`). You still need to run `csm-server` and,
optionally, `csm-gui` separately - see the project README.

## Install

```
/plugin marketplace add simonrw/claude-session-monitor
/plugin install session-monitor@claude-session-monitor
```

## Codex

This plugin covers **Claude Code only**. Claude plugins cannot manage Codex
configuration, so Codex hooks in `~/.codex/config.toml` remain a manual step -
see the "Install the reporter hook for Codex" section of the project README.
