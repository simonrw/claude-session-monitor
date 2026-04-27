# Claude Session Monitor

A dashboard for monitoring active [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and Codex sessions across machines. A hook-based reporter sends session status to a central server, which streams updates to a native desktop GUI via SSE.

## Architecture

```
Claude Code / Codex hook events
        |
        v
   [csm-reporter]  --HTTP POST-->  [csm-server]  --SSE-->  [csm-gui]
                               (SQLite)
```

- **csm-reporter** -- Claude Code and Codex hook binary. Reads hook event JSON from stdin, enriches it with hostname and git info, and POSTs to the server.
- **csm-codex** -- Codex wrapper. Launches the real Codex CLI and marks wrapped Codex sessions ended when the Codex process exits.
- **csm-server** -- Axum HTTP server with SQLite storage. Accepts session reports, broadcasts changes to connected clients via SSE.
- **csm-gui** -- eframe/egui native desktop app. Connects to the server's SSE endpoint and displays sessions in two sections: waiting (needs attention) and working.
- **common** -- Shared types, API definitions, and SSE client used by the other crates.

## Prerequisites

- Rust toolchain (edition 2024)
- Linux desktop environment (for the GUI; the server and reporter work headless)

## Building

```sh
cargo build --release
```

Binaries are produced for `csm-reporter`, `csm-codex`, `csm-server`, and `csm-gui`.

## Setup

### 1. Start the server

```sh
./csm-server
```

The server binds to `0.0.0.0:7685` by default and creates a SQLite database at `~/claude-session-monitor.db`.

```
csm-server [OPTIONS]

  --db <path>     SQLite database path [env: CLAUDE_MONITOR_DB] [default: ~/claude-session-monitor.db]
  --host <addr>   Bind address [default: 0.0.0.0]
  --port <port>   Listen port [default: 7685]
```

### 2. Install the reporter hook for Claude Code

Add the reporter as a Claude Code hook in `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ],
    "SessionStart": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ],
    "SessionEnd": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ],
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "/path/to/csm-reporter" }]
      }
    ]
  }
}
```

Replace `/path/to/csm-reporter` with the actual path to the built `csm-reporter` binary.

The reporter reads hook event JSON from stdin (provided by Claude Code), derives the session status, and POSTs it to the server. It logs to `~/.local/share/claude-session-monitor/reporter.log` with daily rotation.

### 3. Use the Codex wrapper

Codex does not currently expose a process-exit hook. Its `Stop` hook is turn-scoped and means Codex is waiting for more input. To end sessions reliably when Codex exits, launch Codex through `csm-codex`:

```sh
alias codex="/path/to/csm-codex"
```

`csm-codex` finds the real `codex` executable on `PATH`, passes through arguments and stdio, and sends an end event when the wrapped Codex process exits. If the real Codex binary is not discoverable after aliasing, set it explicitly:

```sh
export CSM_CODEX_BIN="/path/to/real/codex"
```

Wrapper options must appear before `--`; arguments after `--` are passed to Codex:

```sh
csm-codex --codex-bin /path/to/real/codex -- --help
```

### 4. Install the reporter hook for Codex

Codex support uses the same `csm-reporter` binary, but the hook command must pass `--agent codex`. Add the hook feature flag and lifecycle hooks to `~/.codex/config.toml`:

```toml
[features]
codex_hooks = true

[[hooks.SessionStart]]
matcher = "startup|resume|clear"

[[hooks.SessionStart.hooks]]
type = "command"
command = "sh -c '/path/to/csm-reporter --agent codex || true'"
timeout = 5

[[hooks.UserPromptSubmit]]

[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "sh -c '/path/to/csm-reporter --agent codex || true'"
timeout = 5

[[hooks.PreToolUse]]
matcher = "*"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "sh -c '/path/to/csm-reporter --agent codex || true'"
timeout = 5

[[hooks.PermissionRequest]]
matcher = "*"

[[hooks.PermissionRequest.hooks]]
type = "command"
command = "sh -c '/path/to/csm-reporter --agent codex || true'"
timeout = 5

[[hooks.PostToolUse]]
matcher = "*"

[[hooks.PostToolUse.hooks]]
type = "command"
command = "sh -c '/path/to/csm-reporter --agent codex || true'"
timeout = 5

[[hooks.Stop]]

[[hooks.Stop.hooks]]
type = "command"
command = "sh -c '/path/to/csm-reporter --agent codex || true'"
timeout = 5
```

Replace `/path/to/csm-reporter` with the actual path to the built `csm-reporter` binary.

The short Codex hook timeout keeps monitoring from delaying agent work if the reporter hangs. The `|| true` soft-fails the command so Codex workflows continue when reporting fails; the reporter also logs parse and post failures instead of blocking the agent.

The Codex parser relies on documented hook input fields: `session_id`, `cwd`, `hook_event_name`, `model`, tool metadata such as `tool_name`, `tool_use_id`, and `tool_input`, and the permission prompt detail at `tool_input.description` when Codex provides it.

```
csm-reporter [OPTIONS]

  --server-url <url>   Server URL [env: CLAUDE_MONITOR_URL] [default: http://localhost:7685]
  --agent <agent>      Hook payload format: claude or codex [default: claude]
```

### 5. Launch the GUI

```sh
./csm-gui
```

```
csm-gui [OPTIONS]

  --server-url <url>   Server URL [env: CLAUDE_MONITOR_URL] [default: http://localhost:7685]
```

The GUI connects to the server's SSE endpoint and displays active sessions. Sessions are grouped into two sections:

- **Waiting** (top) -- sessions needing attention, color-coded:
  - Red: waiting for permission approval
  - Yellow: waiting for user input
- **Working** (bottom) -- sessions actively processing, shown in green

Sessions inactive for 30+ minutes fade to indicate staleness. Each session shows the working directory, hostname, git branch, remote repository, and time since last update. Sessions can be deleted via the close button.

### macOS native app (CsmMac)

For macOS users, a dedicated AppKit menu-bar app is available as an alternative to the cross-platform `csm-gui`. It runs as an accessory (no dock icon), shows live session counts in the menu-bar icon, and exposes a sectioned session list in a popover.

Download the latest `Claude-Session-Monitor-Mac.dmg` from [GitHub Releases](https://github.com/simonrw/claude-session-monitor/releases).

**The build is unsigned.** On first launch macOS Gatekeeper will refuse to run it. To bypass:

1. Drag `CsmMac.app` from the DMG into `/Applications`.
2. In Finder, *right-click* (or Control-click) on `CsmMac.app` → **Open**.
3. Confirm **Open** in the Gatekeeper dialog.

macOS remembers this choice; subsequent launches from Spotlight or Launchpad work normally. Signing + notarization will land once a Developer ID is available.

Server URL is configured from Preferences (gear icon in the popover) or via the `CSM_SERVER_URL` environment variable.

## Configuration

| Variable | Used by | Default | Description |
|---|---|---|---|
| `CLAUDE_MONITOR_URL` | csm-reporter, csm-gui | `http://localhost:7685` | Server URL |
| `CLAUDE_MONITOR_DB` | csm-server | `~/claude-session-monitor.db` | SQLite database file path |
| `CSM_CODEX_BIN` | csm-codex | unset | Path to the real Codex CLI when it cannot be found on `PATH` |
| `RUST_LOG` | csm-reporter | `csm_reporter=debug` | Log level filter (standard `tracing` env filter) |

## API

| Method | Endpoint | Description |
|---|---|---|
| `POST` | `/api/sessions` | Upsert a session (used by reporter) |
| `POST` | `/api/sessions/{id}/end` | Mark a session ended (used by `csm-codex`) |
| `DELETE` | `/api/sessions/{id}` | Delete a session |
| `GET` | `/api/events` | SSE stream of active sessions |
| `GET` | `/api/health` | Health check (`{"status": "ok"}`) |

## Session Statuses

| Status | Trigger hooks | Description |
|---|---|---|
| Working | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | Agent is actively processing (optionally shows current tool; cleared on `PostToolUse`) |
| Waiting (permission) | Claude `Notification` (type `permission_prompt`), Codex `PermissionRequest` | Blocked on permission approval |
| Waiting (input) | Claude `Notification` (other), `Stop`; Codex `Stop` | Waiting for user input |
| Ended | Claude `SessionEnd`; `csm-codex` process exit | Session has finished (excluded from active list) |

## Troubleshooting

**Server won't start** -- Check that port 7685 is not already in use. The server binds to `0.0.0.0:7685`.

**Reporter not sending updates** -- Check the reporter log at `~/.local/share/claude-session-monitor/reporter.log`. Verify `CLAUDE_MONITOR_URL` points to the running server. Ensure the hook is configured in `~/.claude/settings.json` for Claude Code or `~/.codex/config.toml` for Codex.

**GUI shows no sessions** -- Verify the server is running and reachable. Check that `CLAUDE_MONITOR_URL` is set correctly if the server is not on `localhost:7685`.

**GUI shows stale sessions** -- Sessions fade after 30 minutes of inactivity. Use the close button to remove sessions that are no longer relevant. Ended sessions are automatically excluded.

**Database errors** -- The server runs SQLite migrations automatically on startup. If the database is corrupted, delete the file at `~/claude-session-monitor.db` (or the path set via `CLAUDE_MONITOR_DB`) and restart the server.
