# Claude Session Monitor

A dashboard for monitoring active [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and Codex sessions across machines. A watcher daemon polls Claude Code's own session registry and publishes full snapshots of live sessions; Codex sessions are still reported via lifecycle hooks, a deprecated, maintenance-only mechanism kept only because Codex has no equivalent registry (see "Use the Codex wrapper" below). The server streams updates to a native desktop GUI via SSE.

## Architecture

```
Claude Code session registry        Codex hook events
        |                                   |
        v                                   v
  [csm-watcher]                      [csm-reporter]
   (poll + diff)                            |
        |                                   |
        +--------------HTTP POST------------+
                        |
                        v
                  [csm-server]  --SSE-->  [csm-gui]
                    (SQLite)
```

- **csm-watcher** -- Polls Claude Code's session registry (`<CLAUDE_CONFIG_DIR>/sessions/*.json`), discovers every live Claude process on the host, verifies liveness, enriches with git and tmux info, and publishes a full snapshot to the server. No hooks, no plugin, nothing to install into Claude Code itself.
- **csm-reporter** -- Hook binary used for Codex sessions only, and deprecated/maintenance-only along with the rest of the Codex path (see "Use the Codex wrapper" below). Reads hook event JSON from stdin, enriches it with hostname and git/tmux info via its own Codex-only copy of that logic (kept separate from, and not shared with, `csm-watcher`'s enrichment), and POSTs to the server. Claude Code sessions are tracked by `csm-watcher` instead: `csm-reporter --agent claude`, and a bare invocation with no `--agent` flag at all (what a stale Claude Code hook does), both exit non-zero naming `csm-watcher` rather than parsing anything - see "Upgrading from the hook-based setup" below.
- **csm-codex** -- Codex wrapper. Launches the real Codex CLI and marks wrapped Codex sessions ended when the Codex process exits.
- **csm-server** -- Axum HTTP server with SQLite storage. Accepts session reports, broadcasts changes to connected clients via SSE.
- **csm-gui** -- eframe/egui native desktop app. Connects to the server's SSE endpoint and displays sessions in two sections: waiting (blocked on you) and everything else.
- **common** -- Shared types, API definitions, and SSE client used by the other crates.

## Prerequisites

- Rust toolchain (edition 2024)
- Linux desktop environment (for the GUI; the server, watcher, and reporter work headless)

## Building

```sh
cargo build --release
```

Binaries are produced for `csm-watcher`, `csm-reporter`, `csm-codex`, `csm-server`, and `csm-gui`.

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

### 2. Install and run the watcher for Claude Code

`csm-watcher` polls Claude Code's own session registry (`<CLAUDE_CONFIG_DIR>/sessions/*.json`, defaulting to `~/.claude/sessions`) every couple of seconds, discovers every live Claude process on the host, and publishes a full snapshot of live sessions to the server. There is no hook to register, no plugin to install, and nothing to add to `~/.claude/settings.json` - Claude Code already writes everything the watcher needs.

Install it:

```sh
make install-watcher
```

which runs `cargo install --path crates/watcher --locked` and installs the `csm-watcher` binary.

`cargo install` puts the binary in the cargo bin directory (`$CARGO_HOME/bin`, `~/.cargo/bin` by default), and prints a warning right after installing if that directory isn't already on `PATH`. Put it on `PATH` before running `csm-watcher` for the first time, for example:

```sh
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
```

Run it once to see what it would publish without leaving anything running:

```sh
csm-watcher --once
```

Summary of the flags that matter (run `csm-watcher --help` for the exact, current `clap`-generated text):

  --server-url <url>   Server URL, e.g. http://localhost:7685. Not resolved from CLAUDE_MONITOR_URL by clap itself - see the resolution order below
  --interval <dur>     Poll period between sweeps while running continuously, e.g. `2s` or `500ms`; minimum 100ms, ignored with --once [default: 2s]
  --once                Perform a single sweep and exit, instead of running continuously

The server URL is resolved in this order: `--server-url`, then `CLAUDE_MONITOR_URL`, then the `[server] url` key in the config file (`~/.config/claude-session-monitor/config.toml` on Linux, `~/Library/Application Support/claude-session-monitor/config.toml` on macOS - auto-created on first run), then the compiled-in default `http://localhost:7685`. This resolution happens in `csm-watcher`'s own code after `clap` parses `--server-url`, which is why `--help` itself shows no `[env: ...]` annotation on that flag.

It logs to `~/.local/share/claude-session-monitor/watcher.log.<date>`, rotating daily and keeping the most recent 14 files, the same way `csm-reporter` logs to `reporter.log`.

For continuous tracking it needs to run as a long-lived process. Install it as a service so it starts with your session and restarts if it crashes.

Both render commands below use `command -v csm-watcher` to fill in the binary's path, so they need the cargo bin directory on `PATH` too - see the `PATH` export above if you haven't already run it in this shell.

Both `sed` commands below also use the `contrib/...` unit file as a relative path, so run them from the repository root.

**The rendered binary path must sit on a volume that's mounted and available at login**, not merely mounted right now while you're running the render command. A non-default `CARGO_HOME` on an external drive or a network mount is the common way to get this wrong: `command -v csm-watcher` above will happily resolve to something like `/Volumes/External/cargo-home/bin/csm-watcher`, and the render, `plutil -lint`, and the "binary path is executable" check further down all pass, because the volume is mounted while you're doing all of this by hand. The failure only shows up once the service starts on its own at login, before that volume is remounted: the process hangs forever in dyld trying to read the binary off a volume that isn't there yet, rather than failing fast. On macOS this is silent in a way the standard health check does not catch: `launchctl print ... | grep state` keeps reporting `state = running` for a process wedged in dyld, and the watcher's own log file is never created (nor is stdout/stderr populated), because the process never gets far enough to log anything. The only check that actually distinguishes this from a genuinely running watcher is confirming the log file exists and has a recent `starting watcher daemon` line - see "Check it's running" below. If your `CARGO_HOME` lives on a volume that isn't guaranteed to be mounted before login (external or network storage), install the binary somewhere on the volume your home directory lives on instead, for example `cargo install --path crates/watcher --locked --root ~/.local`, and point the render at that binary instead of `command -v csm-watcher`.

**macOS (launchd, per-user LaunchAgent):**

```sh
mkdir -p ~/Library/LaunchAgents
sed -e "s#__CSM_WATCHER_BIN__#$(command -v csm-watcher)#" \
    -e "s#__HOME__#$HOME#g" \
    contrib/launchd/com.claude-session-monitor.watcher.plist \
    > ~/Library/LaunchAgents/com.claude-session-monitor.watcher.plist

# Verify the render actually filled in a real, existing binary path.
# `plutil -lint` alone is not enough here: if `command -v csm-watcher`
# above printed nothing (cargo bin dir not on PATH), sed substitutes the
# placeholder with an empty string - no placeholder is left behind, and
# the plist still passes `plutil -lint` with an empty ProgramArguments
# entry, which launchd will happily load and then fail to run.
resolved_bin="$(plutil -extract ProgramArguments.0 raw ~/Library/LaunchAgents/com.claude-session-monitor.watcher.plist)"
if [ -z "$resolved_bin" ] || [ ! -x "$resolved_bin" ]; then
    echo "ERROR: rendered plist's binary path is empty or not executable: '$resolved_bin'" >&2
else
    echo "OK: rendered plist points at $resolved_bin"
fi

launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.claude-session-monitor.watcher.plist
```

Check it's running: `launchctl print gui/$(id -u)/com.claude-session-monitor.watcher | grep state` tells you launchd thinks the job is running, but that alone does not prove the watcher itself is doing anything - see the volume-availability caveat above for a case where it says `state = running` forever with the process wedged in dyld. The honest check is that the log file exists and has a recent `starting watcher daemon` line: `tail -n5 ~/.local/share/claude-session-monitor/watcher.log.$(date +%Y-%m-%d)` (yesterday's date if you just crossed midnight).

Stop and unload it: `launchctl bootout gui/$(id -u)/com.claude-session-monitor.watcher`

**Linux (systemd, per-user unit):**

```sh
mkdir -p ~/.config/systemd/user
sed "s#__CSM_WATCHER_BIN__#$(command -v csm-watcher)#" \
    contrib/systemd/csm-watcher.service \
    > ~/.config/systemd/user/csm-watcher.service

# Verify the render actually filled in a real, existing binary path.
# Checking for a leftover placeholder is not enough here: if
# `command -v csm-watcher` above printed nothing (cargo bin dir not on
# PATH), sed substitutes the placeholder with an empty string - no
# placeholder is left behind, and you get a unit with an empty
# ExecStart=, which systemd rejects only once you try to start it, not
# at daemon-reload time.
resolved_bin="$(sed -n 's/^ExecStart=//p' ~/.config/systemd/user/csm-watcher.service)"
if [ -z "$resolved_bin" ] || [ ! -x "$resolved_bin" ]; then
    echo "ERROR: rendered unit's ExecStart binary path is empty or not executable: '$resolved_bin'" >&2
else
    echo "OK: rendered unit points at $resolved_bin"
fi

systemctl --user daemon-reload
systemctl --user enable --now csm-watcher.service
```

Check it's running: `systemctl --user status csm-watcher.service` tells you systemd thinks the job is running, but the same volume-availability caveat above applies - the honest check is that the log file exists and has a recent `starting watcher daemon` line: `tail -n5 ~/.local/share/claude-session-monitor/watcher.log.$(date +%Y-%m-%d)` (yesterday's date if you just crossed midnight).

To have it start even when you're not logged in (e.g. a headless server), enable lingering once: `loginctl enable-linger $USER`.

Stop and disable it: `systemctl --user disable --now csm-watcher.service`

Both unit files (see [`contrib/launchd`](contrib/launchd) and [`contrib/systemd`](contrib/systemd)) set an explicit 15-second stop timeout (`ExitTimeOut`/`TimeoutStopSec`), tightened from launchd's and systemd's own defaults (20s and 90s respectively) - still enough headroom above the watcher's own worst-case clean-shutdown time of roughly 9 to 10 seconds (discovery's own `PS_TIMEOUT` of 5s, plus up to `PUBLISH_TIMEOUT`'s 5s if a publish was already in flight when the signal arrived; a measured fully-degraded sweep with `git` and `tmux` both hung came in at about 3.8s, below the 5s bound - see `run_daemon`'s doc comment in `crates/watcher/src/main.rs`), so a normal stop is never escalated to a kill.

Both also set `HOME` explicitly. That matters for two reasons, neither of which is "the" fallback for a Claude process's own config directory: it is where the watcher writes its own log (above), and it seeds `union_discovery`'s unconditional default-profile sweep (`$HOME/.claude`) alongside whatever profiles are discovered from other processes' environments. A *discovered Claude process's* own missing `CLAUDE_CONFIG_DIR` is resolved against **that process's own `HOME`** first (see `default_config_dir_for` in `crates/watcher/src/discovery.rs`), not the watcher's - the watcher's `HOME` is only a last resort, used if that process's own `HOME` cannot be read either. A service running with the wrong or no `HOME` still silently logs, and seeds its default-profile sweep, in the wrong place.

Both also set `PATH` explicitly, and this one is easy to miss because it fails silently: launchd's default agent `PATH` is just `/usr/bin:/bin:/usr/sbin:/sbin`, and systemd `--user`'s default is similarly minimal. `crates/watcher/src/tmux.rs` and `crates/watcher/src/git.rs` both invoke `tmux` and `git` bare (PATH lookup, not an absolute path), and both are designed to degrade quietly on a missing binary - by design, so one host's `tmux`-less setup can't fail every sweep - which means a `PATH` that's missing one or both of them produces a watcher that runs, exits 0, and publishes sessions with no git branch/remote and/or no `tmux_target`, with nothing in the log calling that out as wrong. On macOS specifically, that bare `/usr/bin:/bin:/usr/sbin:/sbin` already resolves `git` (`/usr/bin/git`, the Xcode Command Line Tools stub, present whenever the CLT are installed - the common case), so a bare launchd `PATH` on its own only loses `tmux_target`, not git enrichment; `tmux` itself is almost never under `/usr/bin` (Homebrew installs it under `/opt/homebrew/bin` or `/usr/local/bin`). Linux distros vary in what ships under `/usr/bin`, so there `git`, `tmux`, or both can be missing depending on the host - hence "and/or" above. If your `git` or `tmux` live somewhere other than the paths already listed in the unit files, add that directory to `PATH` there too.

**Do not run the watcher as root**, and do not install it as a system-wide launchd LaunchDaemon or a system systemd unit. The watcher reads other Claude processes' environments to discover their config directories, and it uses a same-uid check to tell "belongs to another user, expected to be unreadable" apart from "should have been readable and wasn't" - a distinction that stops meaning anything once the watcher itself is root, since root can read everyone's environment. It's also the same `HOME` problem as above: root's `HOME` is not yours. Use a per-user LaunchAgent or a systemd `--user` unit, as shown above.

One watcher process covers every `CLAUDE_CONFIG_DIR` profile on its host automatically - it discovers them from the environment of every running Claude process, so a work/personal split needs no extra configuration. Run one watcher per host; each publishes only its own host's sessions.

`CSM_WATCHER_REGISTRY_DIRS` (a `:`-separated list of directories, like `PATH`) is a supported, permanent escape hatch that bypasses automatic discovery and sweeps exactly the directories you name instead - useful if discovery ever needs to be pinned or worked around. A blank or whitespace-only value is treated the same as unset (falls back to discovery), not as "sweep nothing".

At the default 2-second interval, the watcher costs roughly 7.7% of one CPU core, measured on a host with about 880 running processes - most of that is enumerating every process's environment each sweep, not the watcher's own work. That's unlikely to be noticeable on a plugged-in machine, but if you're on battery and want to trade responsiveness for lower average CPU, pass a longer `--interval` (e.g. `--interval 5s`; edit `ExecStart`/`ProgramArguments` in the service file to add it).

#### Upgrading from the hook-based setup

If you previously followed this README's old instructions and have `csm-reporter` registered as Claude Code hooks in `~/.claude/settings.json`, or installed the `session-monitor` plugin, remove both now that `csm-watcher` covers Claude Code:

1. Delete the seven `csm-reporter` hook entries (`PreToolUse`, `PostToolUse`, `Notification`, `Stop`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`) from `~/.claude/settings.json`.
2. If you installed the plugin, uninstall it: `/plugin uninstall session-monitor@claude-session-monitor`. The plugin and its marketplace entry have been removed from this repository (PRO-213), so `/plugin marketplace add simonrw/claude-session-monitor` no longer finds anything to (re)install - uninstalling is the only step left for anyone who already has it.

This is not just tidiness: `csm-reporter` no longer parses Claude Code hook payloads at all (PRO-213). If a leftover hook still calls `csm-reporter` - whether with `--agent claude` explicitly, or bare with no `--agent` flag at all, which is what the old plugin and hand-edited `settings.json` entries both do - the reporter exits non-zero without contacting the server, and prints a message naming `csm-watcher` as the replacement, for example:

```
csm-reporter: --agent claude is no longer supported. Claude Code sessions are tracked by csm-watcher (a polling daemon), not by hooks. Remove the old csm-reporter hooks / session-monitor plugin from Claude Code (see README.md's "Upgrading from the hook-based setup") and run csm-watcher instead. Codex sessions are unaffected: keep using `csm-reporter --agent codex`.
```

Claude Code discards the hook's stderr and continues past a nonzero-exit hook without blocking you, so this shows up as an error in `~/.local/share/claude-session-monitor/reporter.log`, not as a dialog - checking that log is how you notice a hook was left behind. Either way, a skipped removal step is now a loud, harmless no-op instead of a silent double-report: split-brain state between the leftover hooks and `csm-watcher` is no longer possible.

Codex is unaffected - its hooks in `~/.codex/config.toml` and `csm-reporter --agent codex` are still the supported (if deprecated - see the next section) path (step 4 below).

### 3. Use the Codex wrapper

**Codex support is deprecated, maintenance-only, and slated for removal.** It is the last surviving piece of the old hook-driven design that `csm-watcher` replaced for Claude Code (PRO-204/PRO-213), kept only because Codex has no equivalent session registry for a watcher to poll. It is frozen on purpose: no new hook events are added, and its known problems are not being fixed. Concretely, that means:

- **No liveness backstop.** Unlike Claude Code sessions, a Codex session killed with `kill -9`, lost to an OOM, a closed terminal, an SSH drop, or a reboot has nothing to notice it is gone. It stays `Busy` (or whatever it last reported) until something explicitly ends it - normally `csm-codex` on the wrapped process's exit, or a manual `DELETE`/`.../end` call.
- **Never touched by Claude reconciliation.** `csm-watcher`'s snapshots are scoped to the Claude agent kind specifically (see the publish contract in the API table below), so it is structurally incapable of ending, or otherwise altering, a Codex session even if it wanted to. A dead Codex session is only ever cleaned up by the Codex path itself.
- **The non-atomic run-state writes underlying `csm-codex`'s "end recorded sessions on exit" behavior are not fixed.** They are unlikely to lose data in ordinary use but are not held to the same standard as the Claude path.

If you can avoid depending on Codex tracking, do. If you still need it, everything below continues to work exactly as it did before PRO-213 - only Claude Code's tracking mechanism changed.

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

Codex support uses the `csm-reporter` binary. Install it:

```sh
make install-reporter
```

The hook command must pass `--agent codex`. Add the hook feature flag and lifecycle hooks to `~/.codex/config.toml`:

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

Only `--agent codex` is actually accepted. `claude` remains the *default* value deliberately, not because Claude is supported: a stale Claude Code hook was never told to pass `--agent` at all, so it always calls `csm-reporter` bare. Keeping the default at `claude` means that bare invocation resolves to the rejected path and fails loudly, instead of the reporter silently guessing `codex` and mis-parsing a Claude Code hook payload as if it were one. Both `--agent claude` and a bare invocation exit non-zero immediately, before touching the network, with a message naming `csm-watcher`.

### 5. Launch the GUI

```sh
./csm-gui
```

```
csm-gui [OPTIONS]

  --server-url <url>   Server URL [env: CLAUDE_MONITOR_URL] [default: http://localhost:7685]
```

The GUI connects to the server's SSE endpoint and displays active sessions. Sessions are grouped into two sections:

- **Waiting** (top) -- sessions blocked on you, in red, with the detail saying what they are blocked on
- **Everything else** (bottom), most recently updated first, colour-coded by state:
  - Green: busy, thinking or running a tool
  - Teal: running a foreground shell command
  - Blue-grey: idle, at the prompt with its turn finished
  - Grey: ended

The bottom section is deliberately not called "working": it also holds idle and ended sessions. The GUI makes one cut, "does this need me right now", and the colour carries the finer distinction.

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
| `CLAUDE_MONITOR_URL` | csm-watcher, csm-reporter, csm-gui | `http://localhost:7685` | Server URL. For csm-watcher this only wins over the config file, not `--server-url` - see "Install and run the watcher" above |
| `CLAUDE_MONITOR_DB` | csm-server | `~/claude-session-monitor.db` | SQLite database file path |
| `CSM_WATCHER_REGISTRY_DIRS` | csm-watcher | unset | `:`-separated list of registry directories to sweep, bypassing automatic discovery. Permanent supported escape hatch, not just a test seam; blank or whitespace-only is treated as unset |
| `CSM_CODEX_BIN` | csm-codex | unset | Path to the real Codex CLI when it cannot be found on `PATH` |
| `RUST_LOG` | csm-watcher, csm-reporter | `csm_watcher=info,watcher=info` (csm-watcher), `csm_reporter=debug` (csm-reporter) | Log level filter (standard `tracing` env filter) |

## API

| Method | Endpoint | Description |
|---|---|---|
| `POST` | `/api/sessions` | Upsert a session (used by `csm-reporter`, i.e. Codex hooks) |
| `POST` | `/api/sessions/{id}/end` | Mark a session ended (used by `csm-codex`) |
| `DELETE` | `/api/sessions/{id}` | Delete a session |
| `POST` | `/api/hosts/{hostname}/sessions` | Publish a full snapshot of one host's live sessions for one agent kind (used by `csm-watcher`); reconciles the server's view of that host+agent to match the snapshot exactly - upserting everything present, ending everything absent - without touching rows for other hosts or agent kinds |
| `GET` | `/api/hosts` | Last-accepted-snapshot time for every host and agent kind that has ever published one, most recently seen first - lets a client distinguish "this host genuinely has no live sessions" from "this host's watcher has stopped reporting" |
| `GET` | `/api/events` | SSE stream of active sessions |
| `GET` | `/api/health` | Health check (`{"status": "ok"}`) |

## Session Statuses

Session state uses Claude Code's own vocabulary rather than translating it. For Claude sessions `csm-watcher` passes the registry's `status` and `waitingFor` fields straight through. For Codex sessions, which are still hook-reported, the hooks map onto the same states.

| Status | Claude Code (`csm-watcher`) | Codex (`csm-reporter` hooks) | Description |
|---|---|---|---|
| Busy | registry `status` is `busy` | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | Thinking or running a tool. Carries the tool name for Codex; the registry has no current-tool field, so it is absent for Claude |
| Shell | registry `status` is `shell` | not produced | A foreground shell command is running, so you can tell why the session is busy |
| Idle | registry `status` is `idle`, and any value this project does not yet recognise | `Stop` | The turn finished and the session is at the prompt |
| Waiting | registry `status` is `waiting`, with `waitingFor` carried through as the detail | `Notification`, `PermissionRequest` | Blocked on you, with the detail saying what it is blocked on |
| Ended | the session's registry file disappears, or its process is no longer live | `csm-codex` process exit | Session has finished (excluded from the active list) |

Busy and Shell both count as working in the menu-bar count: a foreground shell command is the agent working, just visibly rather than invisibly. Idle counts as neither working nor waiting.

An unrecognised registry `status` falls back to Idle and is logged at warn level, so a Claude Code release that renames a state degrades visibly rather than silently.

## Troubleshooting

**Server won't start** -- Check that port 7685 is not already in use. The server binds to `0.0.0.0:7685`.

**Watcher not reporting Claude sessions** -- Check the watcher log at `~/.local/share/claude-session-monitor/watcher.log.<date>` (today's date; check the previous day's file too if you just crossed midnight) for a recent `starting watcher daemon` line - this, not `launchctl print`/`systemctl status` reporting the job as "running", is the real proof the watcher is alive. `launchctl print gui/$(id -u)/com.claude-session-monitor.watcher | grep state` on macOS or `systemctl --user status csm-watcher.service` on Linux can both keep reporting the job as running even when the watcher process is wedged and has never logged anything - the most common cause is the service binary living on a volume that wasn't yet mounted at login (see the volume-availability caveat under "Install and run the watcher" above), which a non-default `CARGO_HOME` on an external or network volume makes easy to hit by accident. If the log has no recent entry, check where the service's binary path actually points and whether that volume was mounted before login. If the log does have a recent `starting watcher daemon` line, verify the server URL it resolved points to the running server. If it was installed as a service, double check `HOME` in the plist/unit file matches your real home directory - a wrong `HOME` moves both the log and the watcher's own default-profile seed to the wrong place. Also double check `PATH` in the plist/unit file covers wherever `git` and `tmux` are actually installed - a `PATH` that's missing one publishes sessions successfully but silently drops git branch/remote info and/or `tmux_target`, with no error in the log.

**Reporter not sending updates (Codex)** -- Check the reporter log at `~/.local/share/claude-session-monitor/reporter.log`. Verify `CLAUDE_MONITOR_URL` points to the running server. Ensure the hook is configured in `~/.codex/config.toml`.

**GUI shows no sessions** -- Verify the server is running and reachable. Check that `CLAUDE_MONITOR_URL` is set correctly if the server is not on `localhost:7685`.

**GUI shows stale sessions** -- Sessions fade after 30 minutes of inactivity. Use the close button to remove sessions that are no longer relevant. Ended sessions are automatically excluded.

**Database errors** -- The server runs SQLite migrations automatically on startup. If the database is corrupted, delete the file at `~/claude-session-monitor.db` (or the path set via `CLAUDE_MONITOR_DB`) and restart the server.
