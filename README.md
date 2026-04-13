# Claude Session Monitor

A dashboard for monitoring active [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions across machines. A hook-based reporter sends session status to a central server, which streams updates to a native desktop GUI via SSE.

## Architecture

```
Claude Code hook events
        |
        v
   [reporter]  --HTTP POST-->  [server]  --SSE-->  [gui]
                               (SQLite)
```

- **reporter** -- Claude Code hook binary. Reads hook event JSON from stdin, enriches it with hostname and git info, and POSTs to the server.
- **server** -- Axum HTTP server with SQLite storage. Accepts session reports, broadcasts changes to connected clients via SSE.
- **gui** -- eframe/egui native desktop app. Connects to the server's SSE endpoint and displays sessions in two sections: waiting (needs attention) and working.
- **common** -- Shared types, API definitions, and SSE client used by the other crates.

## Prerequisites

- Rust toolchain (edition 2024)
- Linux desktop environment (for the GUI; the server and reporter work headless)

## Building

```sh
cargo build --release
```

Binaries are produced for `reporter`, `server`, and `gui`.

## Setup

### 1. Start the server

```sh
./server
```

The server binds to `0.0.0.0:7685` by default and creates a SQLite database at `~/claude-session-monitor.db`.

```
server [OPTIONS]

  --db <path>     SQLite database path [env: CLAUDE_MONITOR_DB] [default: ~/claude-session-monitor.db]
  --host <addr>   Bind address [default: 0.0.0.0]
  --port <port>   Listen port [default: 7685]
```

### 2. Install the reporter hook

Add the reporter as a Claude Code hook in `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "/path/to/reporter"
      }
    ],
    "Notification": [
      {
        "type": "command",
        "command": "/path/to/reporter"
      }
    ],
    "Stop": [
      {
        "type": "command",
        "command": "/path/to/reporter"
      }
    ],
    "SessionStart": [
      {
        "type": "command",
        "command": "/path/to/reporter"
      }
    ],
    "SessionEnd": [
      {
        "type": "command",
        "command": "/path/to/reporter"
      }
    ],
    "UserPromptSubmit": [
      {
        "type": "command",
        "command": "/path/to/reporter"
      }
    ]
  }
}
```

Replace `/path/to/reporter` with the actual path to the built `reporter` binary.

The reporter reads hook event JSON from stdin (provided by Claude Code), derives the session status, and POSTs it to the server. It logs to `~/.local/share/claude-session-monitor/reporter.log` with daily rotation.

```
reporter [OPTIONS]

  --server-url <url>   Server URL [env: CLAUDE_MONITOR_URL] [default: http://localhost:7685]
```

### 3. Launch the GUI

```sh
./gui
```

```
gui [OPTIONS]

  --server-url <url>   Server URL [env: CLAUDE_MONITOR_URL] [default: http://localhost:7685]
```

The GUI connects to the server's SSE endpoint and displays active sessions. Sessions are grouped into two sections:

- **Waiting** (top) -- sessions needing attention, color-coded:
  - Red: waiting for permission approval
  - Yellow: waiting for user input
- **Working** (bottom) -- sessions actively processing, shown in green

Sessions inactive for 30+ minutes fade to indicate staleness. Each session shows the working directory, hostname, git branch, remote repository, and time since last update. Sessions can be deleted via the close button.

## Configuration

| Variable | Used by | Default | Description |
|---|---|---|---|
| `CLAUDE_MONITOR_URL` | reporter, gui | `http://localhost:7685` | Server URL |
| `CLAUDE_MONITOR_DB` | server | `~/claude-session-monitor.db` | SQLite database file path |
| `RUST_LOG` | reporter | `reporter=debug` | Log level filter (standard `tracing` env filter) |

## API

| Method | Endpoint | Description |
|---|---|---|
| `POST` | `/api/sessions` | Upsert a session (used by reporter) |
| `DELETE` | `/api/sessions/{id}` | Delete a session |
| `GET` | `/api/events` | SSE stream of active sessions |
| `GET` | `/api/health` | Health check (`{"status": "ok"}`) |

## Session Statuses

| Status | Trigger hooks | Description |
|---|---|---|
| Working | `SessionStart`, `UserPromptSubmit`, `PreToolUse` | Claude is actively processing (optionally shows current tool) |
| Waiting (permission) | `Notification` (type `permission_prompt`) | Blocked on permission approval |
| Waiting (input) | `Notification` (other), `Stop` | Waiting for user input |
| Ended | `SessionEnd` | Session has finished (excluded from active list) |

## Troubleshooting

**Server won't start** -- Check that port 7685 is not already in use. The server binds to `0.0.0.0:7685`.

**Reporter not sending updates** -- Check the reporter log at `~/.local/share/claude-session-monitor/reporter.log`. Verify `CLAUDE_MONITOR_URL` points to the running server. Ensure the hook is configured in `~/.claude/settings.json`.

**GUI shows no sessions** -- Verify the server is running and reachable. Check that `CLAUDE_MONITOR_URL` is set correctly if the server is not on `localhost:7685`.

**GUI shows stale sessions** -- Sessions fade after 30 minutes of inactivity. Use the close button to remove sessions that are no longer relevant. Ended sessions are automatically excluded.

**Database errors** -- The server runs SQLite migrations automatically on startup. If the database is corrupted, delete the file at `~/claude-session-monitor.db` (or the path set via `CLAUDE_MONITOR_DB`) and restart the server.
