# Session tracking equivalents in the Codex CLI and the pi CLI

Date: 2026-08-04

Question: our Claude Code architecture watches a sessions directory to find live processes (the `~/.claude/sessions/<pid>.json` registry, hooks for low-latency state, transcript/pid reconciliation - see `2026-07-24-claude-session-tracking.md`). Does an equivalent exist for OpenAI's Codex CLI and for the pi CLI (badlogic/pi-mono)?

Versions inspected:

- **Codex**: source at openai/codex HEAD `e9a692d53ba55d981c353ced88650dd1595c2b5f` (2026-08-04). Local machine has a populated `~/.codex` written by CLI versions up to `0.144.x` plus the Codex desktop app. Docs live at `developers.openai.com/codex/*`, which currently 308-redirects to `learn.chatgpt.com/docs/*`.
- **pi**: source at badlogic/pi-mono HEAD `79cc1ef00ae0643a747f399c267dc8c6c39b5d01` (2026-08-04). Local install is `pi 0.80.6` at `/opt/homebrew/bin/pi` with a populated `~/.pi/agent`.

Source citations are file:line in those two checkouts. Local observations are called out as such.

---

## TL;DR

**Neither CLI has a direct equivalent of Claude Code's `~/.claude/sessions/<pid>.json` per-process registry. Both have enough primitives to build the same monitor shape (registry-ish spine + hook push signal + transcript/mtime reconciliation), but the spine differs per tool:**

- **Codex: yes, close.** The authoritative liveness signal is `~/.codex/thread-writer-locks/<thread_id>.lock` - one file per open session, held under an exclusive advisory `flock` for the whole session lifetime and deleted on drop. A non-blocking flock probe tells you "this thread is live right now" with kernel-level truth (survives SIGKILL: the lock dies with the process). It carries no pid, but Codex now also ships a Claude-Code-style hooks system (`SessionStart`/`SessionEnd`/`Stop`/`PreToolUse`/... with `session_id`, `transcript_path`, `cwd` payloads), so a hook script can record `$PPID` itself. Rollout JSONLs are flushed per event (mtime is live), and a sqlite `threads` table heartbeats `updated_at_ms` every <=5s during a turn. All of it is undocumented internals except the hooks and `notify`.
- **pi: no registry at all, but a first-class extension API.** Sessions land in `~/.pi/agent/sessions/--escaped-cwd--/<ts>_<uuid>.jsonl` - structurally the same per-project layout as Claude Code's `~/.claude/projects` - and the format is *publicly documented* as a stable interface. There is no pid file, lock file, or heartbeat anywhere. The intended extensibility point is in-process TypeScript extensions with `session_start`, `session_shutdown`, `turn_start`/`turn_end`, and `tool_execution_update` events; a ~30-line extension dropped in `~/.pi/agent/extensions/` can maintain exactly the pid registry Claude Code maintains natively. Passive mtime-watching alone is notably worse than on Claude Code: the session file does not exist until the first assistant message completes, and nothing is written during a long tool call.

So the monitor architecture ports to both, with per-tool spines: **Codex = flock probe + hooks; pi = extension-maintained registry + mtime fallback**.

---

## 1. Codex CLI

### 1.1 Storage layout (local, confirmed against source)

Under `~/.codex` (overridable via `CODEX_HOME`):

- `sessions/YYYY/MM/DD/rollout-<YYYY-MM-DDTHH-MM-SS>-<thread_id>.jsonl` - the transcript ("rollout"). Path construction: `codex-rs/rollout/src/recorder.rs:1540-1578`; `SESSIONS_SUBDIR = "sessions"` at `codex-rs/rollout/src/lib.rs:25`. The filename UUID is the **ThreadId, not a pid**.
- First line is a `session_meta` record: `session_id`, `id`, `timestamp`, `cwd`, `originator` (e.g. `"Codex Desktop"`), `cli_version`, `source` (`cli`/`vscode`/`exec`/`subagent`), `model_provider`, git info, base instructions. Struct: `codex-rs/protocol/src/protocol.rs:3078-3135`. **No pid, hostname, or tty field.** Observed locally matching this shape.
- `archived_sessions/` - `codex archive` renames the rollout here and flips `threads.archived` in sqlite (`codex-rs/thread-store/src/local/archive_thread.rs:91-100`).
- `state_5.sqlite` - the `threads` table: `id, rollout_path, created_at, updated_at(_ms), source, model_provider, cwd, title, git_sha, git_branch, git_origin_url, archived, recency_at_ms, ...` (schema confirmed locally; migrations at `codex-rs/state/migrations/0001_threads.sql` and later). 419 rows locally.
- `session_index.jsonl` - append-only `{id, thread_name, updated_at}` written **only when a user names a thread** (`codex-rs/rollout/src/session_index.rs:19-67`). Despite the name, it is a label index, not a session registry - useless for liveness.
- `history.jsonl` - `{session_id, ts, text}` per user prompt (cross-session prompt history).
- `logs_2.sqlite` - log capture DB, see 1.3.
- `thread-writer-locks/` - see 1.2. (Not present locally yet because the local CLI predates it; it is in current source.)
- `process_manager/chat_processes.json` - observed locally: an array of `{command, conversationId, cwd, itemId, osPid, processId, turnId, startedAtMs, updatedAtMs}`. These are **background exec subprocesses spawned by turns** (test runs, `npm run` etc.), not Codex sessions. Grep of the OSS repo at this SHA finds no writer for this file - it is written by the ChatGPT/Codex **desktop app**, not the open-source CLI. Do not build on it.
- `app-server-control/` (socket + startup lock) and `app-server-daemon/app-server.pid` - daemon-scoped, one shared process, not per-session (`codex-rs/app-server-transport/src/transport/mod.rs:53-71`, `codex-rs/app-server-daemon/src/lib.rs:31-35`).

Rollout writes are **appended and flushed per event batch**: the writer task calls `state.flush_if_materialized()` after every `AddItems` and ends with `file.flush()` (`codex-rs/rollout/src/recorder.rs:1737, 1766-1796`). So mtime tracks activity at event granularity. One caveat: for a brand-new session the file is **deferred** until the first rollout item (`recorder.rs:788-791, 862-869`), so a session thinking on its very first prompt briefly has no file. Resumed sessions open the file immediately. Old rollouts get compressed in place, and re-materialization explicitly refreshes mtime (`recorder.rs:1581-1597`) - a false activity signal to filter.

### 1.2 Liveness: the thread writer lock (the real equivalent)

`codex-rs/thread-store/src/local/writer_lock.rs:17-18, 49-78, 165-186`:

- `~/.codex/thread-writer-locks/<thread_id>.lock`, one per open session.
- Acquired with an exclusive advisory `flock` (`file.try_lock()`) at session create and resume, and the guard lives alongside the recorder for the session's whole lifetime (`codex-rs/thread-store/src/local/live_writer.rs:33, 46`).
- Deleted on `Drop`; stale (unlocked) files are swept by the next Codex process (`writer_lock.rs:117-162`).

For an external monitor: enumerate `*.lock`, try a non-blocking flock; `EWOULDBLOCK` means that thread is live *right now*. Because flocks are released by the kernel on process death, this is robust against SIGKILL/crash in a way Claude Code's `updatedAt` staleness heuristics are not. The lock file body is empty - **no pid inside** - so pid mapping needs 1.3 or a hook.

This is the closest Codex thing to `~/.claude/sessions/<pid>.json`: existence+lock-state = live session, keyed by thread id instead of pid.

### 1.3 Pid mapping (indirect only)

Codex never writes its own pid into any session artifact. The one pid<->thread link is incidental: log capture tags every row in `~/.codex/logs_2.sqlite` with `process_uuid = "pid:<pid>:<uuid>"` alongside `thread_id` (`codex-rs/state/src/log_db.rs:387-393`, insert at `codex-rs/state/src/runtime/logs.rs:18`). `SELECT DISTINCT thread_id, process_uuid FROM logs` then `kill -0` works, but rows are size-pruned and the format is an internal string - treat as best-effort. The robust route is a `SessionStart` hook recording `$PPID` (1.4).

### 1.4 Hooks and notify (the push signal - documented)

Codex now has a near-clone of Claude Code's hooks system, documented at https://learn.chatgpt.com/docs/hooks and implemented in `codex-rs/hooks/`:

- Events (`codex-rs/hooks/src/schema.rs:99-121` plus `SessionEnd`): `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PermissionRequest`, `PreCompact`, `PostCompact`, `SubagentStart`, `SubagentStop`, `Stop`.
- `SessionStart` payload (`schema.rs:486-497`): `session_id`, `transcript_path` (the rollout jsonl path), `cwd`, `hook_event_name`, `model`, `permission_mode`, `source`. `SessionEnd` adds `reason`. Same correlation surface as Claude Code hook payloads.
- Configured via `~/.codex/hooks.json` / `[hooks]` in `config.toml`, user or project level; non-managed hooks are trust-gated via `/hooks` (docs).
- `SessionEnd` fires from graceful session shutdown only (`codex-rs/core/src/hook_runtime.rs:369-398`) - same "not delivered on SIGKILL" caveat as Claude Code, hence reconcile against the flock.
- Docs note `SessionEnd` also fires after 30 minutes idle and on archive/delete - so `SessionEnd` != process exit; still reconcile with the lock.

The older `notify` config option (`codex-rs/core/src/config/mod.rs:715-732`) spawns a program with a JSON argv payload but supports exactly **one** event, `agent-turn-complete` (`{type, thread-id, turn-id, cwd, client, input-messages, last-assistant-message}` - `codex-rs/hooks/src/legacy_notify.rs:14-93`). Turn-end only; superseded by hooks for our purposes.

### 1.5 Heartbeat: the threads table

`threads.updated_at_ms` is touched **during** a turn, debounced to 5s (`THREAD_UPDATED_AT_TOUCH_INTERVAL`, `codex-rs/thread-store/src/thread_metadata_sync.rs:27, 159-191`); `recency_at_ms` advances on `TurnStarted` only. Useful as a poll-friendly activity feed (one sqlite query covers all sessions, with `cwd` and `rollout_path` in the row), but it cannot distinguish "crashed 10s ago" from "alive and idle" - pair it with the flock.

### 1.6 First-party subscription surfaces

- `codex app-server` (JSON-RPC over stdio/websocket/unix socket) emits `thread/started`, `thread/closed`, `thread/status/changed`, `turn/started`, `turn/completed`, and supports `thread/list` and `thread/loaded/list` (docs: https://learn.chatgpt.com/docs/app-server; notification names in `codex-rs/app-server-protocol/src/protocol/common.rs:1687-1704`). Limitation: it only sees sessions running *inside the server you spawned or its daemon* - it is not a global observer of TUI sessions the user started by hand.
- OTEL: `otel.exporter` can be pointed at a local OTLP collector; events include `codex.conversation_starts`, `codex.turn_ttft`, `codex.user_prompt` (`codex-rs/otel/src/events/session_telemetry.rs`). Per-process config opt-in, no pid attribute - niche.

### 1.7 Stability

Hooks, `notify`, `app-server`, and the CLI commands are documented. Everything else - rollout paths, `session_meta`, `state_5.sqlite`, `session_index.jsonl`, writer locks - is undocumented internals; the version-suffixed DB filenames (`state_5`, `logs_2`) advertise that they get replaced without notice. Same trust posture as our Claude Code registry dependency: build on it, but keep it behind a module boundary and reconcile from multiple signals.

---

## 2. pi CLI (badlogic/pi-mono)

### 2.1 Storage layout (local, confirmed against source)

Under `~/.pi/agent` (overridable via `PI_CODING_AGENT_DIR`):

- `sessions/--<escaped-cwd>--/<ISO-ts-with-:-and-.-as-->_<uuid>.jsonl` - one dir per project cwd, one file per session. Escaping: strip leading slash, replace `/ \ :` with `-`, wrap in `--...--` (`packages/coding-agent/src/core/session-manager.ts:476-489`); naming at `session-manager.ts:935-953`. This is the same per-project shape as Claude Code's `~/.claude/projects/<escaped>/<session-id>.jsonl`. Escaping is lossy (literal `-` vs separator), so recover cwd from the header, not the dir name.
- First line: `{"type":"session","version":3,"id":"<uuid>","timestamp":"...","cwd":"..."}` (observed locally; `session-manager.ts:938-945`). Subsequent lines: `message`, `model_change`, `thinking_level_change`, `compaction`, `custom`, `custom_message`, `session_info`, `label`, `branch_summary` (`session-manager.ts:1058-1245`).
- Custom session dirs change the layout: with `--session-dir` / `$PI_CODING_AGENT_SESSION_DIR` / `sessionDir` in settings.json, files are stored **flat** and filtered by header `cwd` (`packages/coding-agent/src/main.ts:632-635`, `session-manager.ts:1638-1641`). A monitor must not assume the `--escaped--` layout.
- Other files in `~/.pi/agent`: `settings.json`, `auth.json`, `trust.json`, `extensions/`, `skills/` - no state db, no registry.

Write behavior (`session-manager.ts:1014-1047`): synchronous `appendFileSync` per entry, **but the file is not created until the first assistant message completes** - a brand-new session mid-first-turn has no file at all. After that, writes happen at message granularity (assistant message end, tool result), so **nothing is written during a long tool call** - mtime can be minutes stale on a very-alive session. Pi's own session picker distrusts mtime and reads the last entry's timestamp instead (`session-manager.ts:743-748`) - a monitor should do the same (stat for change detection, tail the last line for truth).

### 2.2 Liveness: no registry

**Nothing exists.** No pid file, lock file, heartbeat, or socket registry anywhere in the repo; `proper-lockfile` is used only to serialize config writes (`settings-manager.ts:208`, `auth-storage.ts`, `trust-manager.ts`). The `locked` concept in `packages/server/src/sessions.ts` is in-memory state of the hosted `pi server`, not local CLI. Passive detection is therefore: recent-mtime candidates + `ps` scan for `pi` processes + cwd matching - strictly weaker than what we have for Claude Code or Codex.

### 2.3 The equivalent push mechanism: extensions

Pi's answer to Claude Code hooks is in-process TypeScript extensions - full Node capability, auto-loaded from `~/.pi/agent/extensions/*.ts` (global, zero per-project setup) or project `.pi/extensions/` (trust-gated). Documented in `packages/coding-agent/docs/extensions.md` (event catalogue, lifecycle diagram at :275-348).

Events relevant to a monitor (`packages/coding-agent/src/core/extensions/types.ts:1034-1059, 1200-1239`):

- `session_start` (`reason: startup|reload|new|resume|fork`), `session_shutdown` (`reason: quit|reload|new|resume|fork`) - shutdown is wired to SIGTERM/SIGHUP handlers in both interactive and print modes (`src/modes/interactive/interactive-mode.ts:3743-3760`, `src/modes/print-mode.ts:51-60`), but cannot fire on SIGKILL/crash.
- `agent_start`/`agent_end`, `turn_start`/`turn_end`, `message_start`/`_update`/`_end`.
- `tool_execution_start`/`_update`/`_end` - `_update` fires *during* tool execution, closing the long-tool-call mtime blind spot.
- `ctx.sessionManager.getSessionFile()` gives the handler the JSONL path for correlation (`docs/extensions.md:972-985`).

A ~30-line extension writing `{pid, sessionId, sessionFile, cwd, status, updatedAt}` to a registry dir on these events reproduces Claude Code's `~/.claude/sessions/<pid>.json` almost exactly (example shapes already in-repo: `examples/extensions/notify.ts`, `auto-commit-on-exit.ts`). Same reconciliation rule as everywhere: registry entries are claims; validate pid liveness and sweep stale files.

### 2.4 Supervisor modes (only for sessions we spawn)

`pi --mode json` streams `agent_start`/`turn_*`/`message_*`/`tool_execution_*` events as JSONL on stdout (`docs/json.md`); `pi --mode rpc` is bidirectional with `get_state` returning `{isStreaming, sessionFile, sessionId, ...}` (`docs/rpc.md:162-192`). Useful if the monitor ever launches agents itself; irrelevant for discovering user-started sessions.

### 2.5 Stability

Unusually good: the session format has a 438-line public spec (`packages/coding-agent/docs/session-format.md`) with a version field (`version: 3`), version history, and explicit statements like "delete a session by removing its .jsonl". External tools reading the sessions dir are an anticipated use case, not reverse engineering. The extension event API is likewise documented. This is *more* committed than Claude Code's undocumented registry.

---

## 3. Comparison to the Claude Code architecture

| Signal | Claude Code | Codex | pi |
|---|---|---|---|
| Per-project session transcripts | `~/.claude/projects/<escaped>/<id>.jsonl` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (date-keyed, cwd in header/sqlite) | `~/.pi/agent/sessions/--escaped--/<ts>_<id>.jsonl` |
| Live-process registry | `~/.claude/sessions/<pid>.json` (pid, cwd, status enum, heartbeats) | `thread-writer-locks/<thread_id>.lock` flock (liveness only, no pid, no status) | none |
| Pid available on disk | yes (registry) | only incidentally (`logs_2.sqlite`), else via hook `$PPID` | only via our own extension |
| Push/hook events | 30 hook events, documented | 11 hook events + `notify` turn-complete, documented | extension events (session/turn/tool/message), documented |
| Busy/idle/waiting state | registry `status` field | none on disk; infer from rollout tail / `updated_at_ms` heartbeat / hooks | none on disk; extension events only |
| Transcript flushed live | yes | yes (per event; deferred file creation at session start) | per message only (deferred creation; silent during tool calls) |
| Documented for external readers | mostly undocumented | hooks/app-server documented, storage undocumented | storage and events publicly documented |
| Crash-robust liveness | pid probe + staleness heuristics | flock probe (kernel-truth, best of the three) | pid probe on extension-registry claims |

Recommended per-tool monitor spine, mirroring the hybrid we already use for Claude Code:

- **Codex**: flock-probe `thread-writer-locks/` as the registry; install a `SessionStart`/`SessionEnd`/`Stop` hook for pid + low-latency state; reconcile with rollout mtime and `threads.updated_at_ms`. Note current released CLIs on this machine predate writer locks - version-gate and fall back to sqlite heartbeat + `ps`.
- **pi**: ship a monitor extension into `~/.pi/agent/extensions/` as the registry writer (this is the install-burden equivalent of our Claude Code hooks plugin, but it is the *only* precise option, not an optimization); fall back to sessions-dir scanning + `ps` for uninstrumented sessions, using last-entry timestamps rather than mtime.

---

## Sources

- Local inspection (2026-08-04): `~/.codex` (sessions, `state_5.sqlite` schema and rows, `session_index.jsonl`, `history.jsonl`, `process_manager/chat_processes.json`, `app-server-control/`), `~/.pi/agent` (sessions dirs, file headers), `pi --help` (0.80.6).
- openai/codex @ `e9a692d53ba55d981c353ced88650dd1595c2b5f`: `codex-rs/rollout/src/recorder.rs`, `codex-rs/rollout/src/session_index.rs`, `codex-rs/protocol/src/protocol.rs`, `codex-rs/thread-store/src/local/{writer_lock,live_writer,archive_thread,update_thread_metadata}.rs`, `codex-rs/thread-store/src/thread_metadata_sync.rs`, `codex-rs/state/src/{log_db.rs,sqlite.rs,runtime/{logs,threads}.rs,migrations/}`, `codex-rs/hooks/src/{schema.rs,types.rs,legacy_notify.rs,events/}`, `codex-rs/core/src/{config/mod.rs,hook_runtime.rs,session/}`, `codex-rs/app-server-daemon/src/`, `codex-rs/app-server-transport/src/transport/mod.rs`, `codex-rs/otel/src/`.
- badlogic/pi-mono @ `79cc1ef00ae0643a747f399c267dc8c6c39b5d01`: `packages/coding-agent/src/core/session-manager.ts`, `src/core/extensions/types.ts`, `src/core/agent-session.ts`, `src/modes/{interactive/interactive-mode.ts,print-mode.ts,json-event.ts,rpc/}`, `src/main.ts`, `src/cli/args.ts`, `src/config.ts`; docs: `packages/coding-agent/docs/{session-format.md,sessions.md,extensions.md,rpc.md,json.md,settings.md}`.
- Codex docs: https://learn.chatgpt.com/docs/hooks, https://learn.chatgpt.com/docs/app-server, https://learn.chatgpt.com/docs/developer-commands?surface=cli (redirect targets of developers.openai.com/codex/*).
