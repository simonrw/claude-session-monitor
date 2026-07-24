# Robustly tracking Claude Code sessions from an external monitor

Date: 2026-07-24
Claude Code version inspected: **2.1.206** (primary). Binary is a Bun-compiled single-file Mach-O arm64 executable, 240 MB, at `/opt/homebrew/Caskroom/claude-code/2.1.206/claude` (Homebrew cask). Additional versions present locally: `2.1.211`, `2.1.212`, `2.1.214` under `~/.local/share/claude/versions/`. Some transcript records on disk were written by `2.1.197`. All "binary" citations below are string/AST fragments grepped out of the shipped `claude` executable with `rg -a` / `rg -aoU`; the compiled binary has no meaningful line numbers, so fragments are quoted verbatim instead.

Docs live at `https://code.claude.com/docs/en/*` (the old `docs.claude.com/en/docs/claude-code/*` URLs 301-redirect there).

Where the shipped binary and the docs disagree, I trust the binary and say so inline.

---

## TL;DR / recommended architecture

**Use the session registry as the spine, hooks as the low-latency state signal, and process + transcript as the reconciliation backstop. This is a hybrid, and every robust monitor in the ecosystem is a hybrid.**

The single most under-appreciated primitive on this machine is `~/.claude/sessions/<pid>.json`. Modern Claude Code (>= ~2.1.x) writes one JSON file per running CLI process, keyed by OS pid, containing `pid`, `sessionId`, `cwd`, `startedAt`, `procStart`, `version`, `kind`, `entrypoint`, a human `name`, a live `status` field constrained to `["busy","shell","idle","waiting"]`, an optional `waitingFor`, a `tmux` pane id, `updatedAt`/`statusUpdatedAt` heartbeat timestamps, and a `bridgeSessionId`. This is an authoritative, self-maintained, poll-friendly registry of every session, its cwd, and its coarse state - exactly the thing a monitor wants - and it already does the pid<->cwd<->state mapping for you. It is not documented anywhere. Poll this directory (or watch it with fsnotify), cross-check `process.kill(pid, 0)` liveness, and you have 80% of a monitor with almost zero install burden.

For low-latency state transitions (busy the instant a prompt is submitted, idle the instant the turn ends, waiting the instant a permission prompt appears) install a **hooks plugin**. Hooks are push, sub-second, and carry `session_id` + `transcript_path` + `cwd` + `permission_mode` in every payload, so they slot straight into the registry keyed by `session_id`. The catch that every hook-only design gets wrong: hooks are not delivered on `SIGKILL`, on `kill -9`, on a crash, or on a lost terminal, and `SessionEnd` fires only on graceful teardown. So hooks can tell you a session *became* busy but cannot be trusted to tell you it *ended*. You must reconcile.

For reconciliation and for sessions started without your plugin, fall back to (a) pid liveness sweeps, (b) `sessions/*.json` `updatedAt` staleness, (c) transcript-file mtime, and (d) tmux pane existence. Tail the transcript JSONL only when you need fine-grained per-message state or a full activity history; it is the richest source but also the highest-effort and most schema-fragile. Do not build a monitor that depends *solely* on transcript tailing (miss the "waiting for permission" state) or *solely* on process scanning (no idle/busy distinction). The registry + hooks + a reconcile loop is the sweet spot.

---

## 1. The hooks system

### 1.1 The complete event list (from the binary)

The binary contains the canonical list of every hook event name. The full internal set (30 events) is emitted as a literal array:

```
"PreToolUse","PostToolUse","PostToolUseFailure","PostToolBatch","Notification",
"UserPromptSubmit","UserPromptExpansion","SessionStart","SessionEnd","Stop",
"StopFailure","SubagentStart","SubagentStop","PreCompact","PostCompact",
"PermissionRequest","PermissionDenied","Setup","TeammateIdle","TaskCreated",
"TaskCompleted","Elicitation","ElicitationResult","ConfigChange","WorktreeCreate",
"WorktreeRemove","InstructionsLoaded","CwdChanged","FileChanged","MessageDisplay"
```
(binary, verbatim array). The docs list the same 30 ( https://code.claude.com/docs/en/hooks ).

A **second, smaller array** in the binary enumerates the events that are the "classic" user-configurable subset (these are the safe, stable ones to register in `settings.json`):

```
"PreToolUse","PostToolUse","Notification","UserPromptSubmit","UserPromptExpansion",
"SessionStart","SessionEnd","Stop","SubagentStop","PreCompact","PostCompact",
"TeammateIdle","TaskCreated","TaskCompleted"
```
(binary, verbatim array).

Every event named in the question exists. `StopFailure`, `PermissionDenied`, `CwdChanged`, `TaskCreated`, `TaskCompleted`, `TeammateIdle`, `SubagentStart` are all real. Events beyond the original documented set (newer/less-documented): `PostToolUseFailure`, `PostToolBatch`, `UserPromptExpansion`, `PermissionRequest`, `Setup`, `Elicitation`, `ElicitationResult`, `ConfigChange`, `WorktreeCreate`, `WorktreeRemove`, `InstructionsLoaded`, `FileChanged`, `MessageDisplay`, `PostCompact`. There is **no** `PreToolBatch`.

### 1.2 Common (base) fields on every hook payload

Every hook stdin payload is built from a base object (binary constructor `Rf(...)`):

```
session_id, transcript_path, cwd, prompt_id, permission_mode,
agent_id, agent_type, effort, hook_event_name
```
Binary fragment:
```
session_id:n, transcript_path:eH(n), cwd:Ct(), prompt_id:$mt()??void 0,
permission_mode:e, agent_id:r?.agentId, agent_type:o, effort:a
```
- `transcript_path` is derived from the session id (`eH(n)`), so a hook gives you the exact JSONL path for free.
- `prompt_id` is a UUID correlating a user prompt with all subsequent events until the next prompt; the binary's own Zod `.describe()` says it is "Same value emitted on OpenTelemetry events as the `prompt.id` attribute, so hook output can be joined to OTel events at prompt grain. Absent until the [first prompt]". Requires v2.1.196+ (docs).
- `permission_mode` enum (docs): `default | plan | acceptEdits | auto | dontAsk | bypassPermissions`.
- `agent_id` / `agent_type` are only populated inside subagents / `--agent` runs.

### 1.3 Per-event payload fields (from the binary constructors)

Every fragment below is the literal object built in the binary right before it is handed to the hook runner (`ND({hookInput:...})` / `KPd(n,r)`). Fields shown are *in addition to* the base fields in 1.2.

| Event | Extra payload fields (verbatim from binary) | Fires when | Can be missed? |
|---|---|---|---|
| `SessionStart` | `source, agent_type, model, session_title` | Session begins or resumes/forks/compacts | Reliable at startup; missed if plugin not installed before start |
| `Setup` | `trigger` | CLI `--init` / `--maintenance` / `--init-only` | Only on init runs |
| `UserPromptSubmit` | `prompt, session_title` | User submits a prompt (turn start) | Missed if crash between submit and hook |
| `UserPromptExpansion` | `expansion_type, command_name, command` | Slash-command / expansion into a prompt | Only on expansions |
| `PreToolUse` | `tool_name, tool_input, tool_use_id` | Before each tool call | Missed on kill mid-turn |
| `PermissionRequest` | `tool_name, tool_input, permission_suggestions` | Permission dialog shown | Only when a prompt is raised |
| `PermissionDenied` | `tool_name, tool_input, tool_use_id, reason` | Auto-mode classifier denies a call | Only on denial |
| `PostToolUse` | `tool_name, tool_input, tool_response, tool_use_id` | After a tool call succeeds | Missed on kill mid-tool |
| `PostToolUseFailure` | `tool_name, tool_input, tool_use_id` | After a tool call fails | Only on failure |
| `PostToolBatch` | `tool_calls` (array) | After a parallel tool batch resolves | Only on batches |
| `Notification` | `message, title, notification_type` | Claude emits a notification (see 1.5) | Async; can be dropped if session ends fast |
| `MessageDisplay` | `turn_id, message_id, ...` | While assistant text renders | High-frequency; 10s timeout |
| `SubagentStart` | `agent_id, agent_type` | Subagent spawned | Only with subagents |
| `SubagentStop` | `stop_hook_active, agent_id, agent_transcript_path, agent_type, last_assistant_message` | Subagent finishes | Missed on kill |
| `TaskCreated` | `task_id, task_subject, task_description, teammate_name, team_name` | `TaskCreate` tool used | Only in team mode |
| `TaskCompleted` | `task_id, task_subject, task_description, teammate_name, team_name` | Task marked completed | Only in team mode |
| `TeammateIdle` | `teammate_name, team_name` | A team teammate is about to go idle | Only in team mode |
| `Stop` | `stop_hook_active, last_assistant_message` | Main agent finishes responding (turn end) | Missed on kill; see 1.6 caveats |
| `StopFailure` | `error, error_details, last_assistant_message` | Turn ends due to API error | Only on API failure |
| `PreCompact` | `trigger, custom_instructions` | Before context compaction | Only on compaction |
| `PostCompact` | `trigger, compact_summary` | After compaction completes | Only on compaction |
| `Elicitation` | `mcp_server_name, message, mode, url, elicitation_id` | MCP server requests input | Only with MCP elicitation |
| `ElicitationResult` | `mcp_server_name, elicitation_id, mode` | After user responds to elicitation | Only with MCP elicitation |
| `ConfigChange` | `source, file_path` | Config file changes mid-session | Only on config edits |
| `CwdChanged` | `old_cwd, new_cwd` | Working dir changes (`/cwd`, `cd`) | Async; can be missed |
| `FileChanged` | `file_path, event` | A watched file changes on disk | Async; can be missed |
| `WorktreeCreate` | `name` | Worktree being created | Only on worktree ops |
| `WorktreeRemove` | `worktree_path` | Worktree being removed | Only on worktree ops |
| `InstructionsLoaded` | `file_path, memory_type, load_reason` | CLAUDE.md / rules loaded | Only on load |
| `SessionEnd` | `reason` | Session terminates gracefully | **Not delivered on SIGKILL/crash** |

`SessionStart.source` enum in the binary is `["startup","resume","clear","compact"]`; the docs also list `fork`. Trust the binary set plus `fork` as newer.
`SessionEnd.reason` observed value `"other"`; docs list `clear | logout | prompt_input_exit | other`.

### 1.4 Hook output schema (what a hook can send back)

From the binary's embedded doc block (verbatim):

```json
{
  "systemMessage": "Warning shown to user in UI",
  "continue": false,
  "stopReason": "Message shown when blocking",
  "suppressOutput": false,
  "decision": "block",
  "reason": "Explanation for decision",
  "hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": "Context injected back to model"
  }
}
```
`decision:"block"` applies to PostToolUse/Stop/UserPromptSubmit; for PreToolUse use `hookSpecificOutput.permissionDecision` instead (deprecation noted in the binary doc). A monitor should emit nothing (or `{"suppressOutput":true}`) so it never perturbs the session.

### 1.5 `Notification.notification_type` values (from the binary)

These are the notification subtypes a monitor can key on:
```
agent_completed, agent_needs_input, auth_success,
computer_use_enter, computer_use_exit,
elicitation_complete, elicitation_response, idle_prompt
```
(binary, `notificationType:"..."` literals). For state detection the important ones are `agent_needs_input` (waiting on the user / permission), `idle_prompt` (session went idle at the prompt), and `agent_completed`.

### 1.6 Ordering, timeout, missability, crash behaviour

- **Once per session:** `SessionStart`, `SessionEnd`. **Once per turn:** `UserPromptSubmit`, `Stop`/`StopFailure`. **Per tool call:** `PreToolUse`/`PostToolUse` ( https://code.claude.com/docs/en/hooks ).
- **Parallelism:** all matching hooks for one event run in parallel; identical handlers are deduplicated by command/args (command hooks) or URL (HTTP hooks) (docs).
- **Timeouts (docs):** default per hook type - `command`/`http`/`mcp_tool` = 600s, `prompt` = 30s, `agent` = 60s. `UserPromptSubmit` lowers the command/http/mcp defaults to 30s; `MessageDisplay` lowers them to 10s. (The binary is littered with `timeout:60000`/`30000`/`5000` constants, but those are internal subsystem timeouts, not the hook defaults - trust the docs table here, and note older Claude Code documented a flat 60s default.)
- **Can be missed:** async events (`FileChanged`, `CwdChanged`, `Notification`) may be queued and dropped if the session ends before they flush (docs). Any per-turn/per-tool hook is missed if the process dies mid-turn.
- **On crash/kill:** hooks fire from the CLI process. `SIGKILL`/`kill -9`/OOM/terminal loss => **no hook at all**, including no `SessionEnd`. `SessionEnd` is only a graceful-shutdown signal. This is the fundamental reason a hook-only monitor leaks "ghost" sessions and why liveness reconciliation (section 9) is mandatory.

---

## 2. The transcript JSONL

Path: `~/.claude/projects/<path-slug>/<session-id>.jsonl`. The `<path-slug>` is the absolute cwd with `/`, `.`, `_` etc. collapsed to `-` (binary uses a chain of `.replace(/[._]/g,"-")`, `.replace(/\//g,"-")`, `.replace(/-+/g,"-")` style transforms). Example on disk: `/Users/simon/dev/claude-session-monitor` -> `-Users-simon-dev-claude-session-monitor`. Note: this is lossy - two different paths can slug-collide, so read `cwd` from a record rather than un-slugging the directory name.

### 2.1 Record `type` values observed on real data

Enumerated with `jq -r '.type'` across a live project's transcripts:

| `type` | Meaning |
|---|---|
| `user` | User message OR a tool result (tool results carry `toolUseResult`) |
| `assistant` | Assistant message (text / tool_use blocks); carries `requestId` |
| `attachment` | Attached content (files, images, pastes) |
| `system` | System event; has a `subtype` |
| `queue-operation` | Prompt-queue add/remove; keys `content, operation, sessionId, timestamp, type` |
| `last-prompt` | Records the last prompt text (`lastPrompt`) |
| `ai-title` | AI-generated session title (`aiTitle`) |
| `permission-mode` | A permission-mode change (`permissionMode`) |
| `mode` | Mode change (`mode`) |
| `bridge-session` | Cloud/bridge linkage: `sessionId, bridgeSessionId, lastSequenceNum` |
| `file-history-snapshot` | Editor file snapshot: `isSnapshotUpdate, messageId, snapshot, type` |
| `agent-setting` | Agent setting change (`agentSetting`) |

`system.subtype` values seen: `bridge_status`, `local_command`, `stop_hook_summary`.

### 2.2 Full key set observed (real data, frequency-ranked)

```
type, sessionId, timestamp, version, uuid, userType, parentUuid, isSidechain,
gitBranch, entrypoint, cwd, message, slug, requestId, promptId, toolUseResult,
sourceToolAssistantUUID, session_id, attachment, operation, leafUuid, lastPrompt,
aiTitle, teamName, permissionMode, agentName, content, mode, promptSource, origin,
isMeta, attributionSkill, subtype, lastSequenceNum, bridgeSessionId, snapshot,
messageId, isSnapshotUpdate, level, agentSetting, url, toolUseID, stopReason,
sourceToolUseID, rewound, preventedContinuation, hookInfos, hookErrors, hookCount,
hookAdditionalContext, hasOutput, explicit
```

### 2.3 Core record schema (real field names)

| Field | Type | Notes |
|---|---|---|
| `type` | string | see 2.1 |
| `uuid` | string | this record's id |
| `parentUuid` | string\|null | previous record's uuid; forms the DAG. `null` at a root/fork point |
| `timestamp` | ISO-8601 string | wall-clock |
| `sessionId` | string (uuid) | session this record belongs to |
| `cwd` | string | working dir at time of record (authoritative, not the slug) |
| `gitBranch` | string | branch at time of record |
| `version` | string | Claude Code version that wrote it (e.g. `2.1.197`) |
| `entrypoint` | string | e.g. `cli` |
| `userType` | string | e.g. `external` |
| `isSidechain` | bool\|null | `true` => this record belongs to a subagent side-conversation |
| `slug` | string | session/turn slug (e.g. `turn-the-hooks-into-purring-candy`) |
| `requestId` | string | on `assistant` records; the API `req_...` id |
| `promptId` | string | correlates to hook `prompt_id` / OTel `prompt.id` |
| `message` | object | the Anthropic message (role, content blocks) |
| `toolUseResult` | object | present on `user` records that are tool results |
| `sourceToolUseID` / `sourceToolAssistantUUID` | string | links a subagent/tool-spawned record back to its originating `tool_use` |
| `agentName` / `teamName` | string\|null | populated in team/subagent mode |
| `isMeta` | bool | meta bookkeeping record (e.g. injected context), content often `null` |
| `stopReason` | string | on stop-related records |
| `rewound` / `preventedContinuation` / `explicit` | bool | rewind / stop-hook bookkeeping |
| `hookInfos` / `hookErrors` / `hookCount` / `hookAdditionalContext` | mixed | recorded hook activity for a turn |

`system` + `subtype:"stop_hook_summary"` record keys (a good "turn ended" marker in the transcript): `cwd, entrypoint, gitBranch, hasOutput, hookAdditionalContext, hookCount, hookErrors, hookInfos, isSidechain, level, parentUuid, sessionId, session_id, stopReason, subtype, timestamp, toolUseID, type, userType, uuid, version, preventedContinuation`.

### 2.4 Sidechains / subagents, resumes, forks

- **Sidechains (older mechanism):** a subagent's messages are written into the *same* transcript file with `isSidechain: true` and their own `parentUuid` chain, linked to the spawning `tool_use` via `sourceToolUseID` / `sourceToolAssistantUUID`. The schema still carries `isSidechain` in 2.1.206, but **on this machine no current transcript contained any `isSidechain:true` records** - modern multi-agent work has moved to the teams/tasks subsystem (section 3.4), where members are separate sessions with their own transcripts and `agentName`/`teamName` set. Treat `isSidechain` as still-supported but increasingly superseded.
- **Resume:** appending to the existing `<session-id>.jsonl`; a new `SessionStart` with `source:"resume"` marks the boundary. `parentUuid` continues the chain.
- **Fork:** `SessionStart source:"fork"` (docs); a fork starts a new session id / new transcript whose early records point back via `parentUuid`/`leafUuid` to the forked-from message.
- `leafUuid` marks the tip a resume/fork branched from.

### 2.5 Deriving state by tailing

Tail the newest `<session-id>.jsonl` and track the last record:
- last record `type:"user"` with real user content, no newer `assistant` => **busy** (model is about to respond / responding).
- last `assistant` block is a `tool_use` with no following `user` tool-result => **busy** (tool running).
- a `system`/`stop_hook_summary` record or a trailing `assistant` text with nothing after => **idle** (turn ended).
- `permission-mode` / `Notification`-adjacent records do NOT appear reliably enough in the transcript to detect the *waiting-for-permission* state - that is why you need the hook or the registry `status:"waiting"`.
- File mtime is a cheap "recently active" proxy for reconciliation.

---

## 3. State files under `~/.claude`

### 3.1 `~/.claude/sessions/<pid>.json` - the session registry (authoritative, undocumented)

One file per live CLI process, filename = OS pid. Real example (secrets none; sanitised copy):

```json
{"pid":22684,"sessionId":"a969...a90","cwd":"/Users/simon/dev/claude-session-monitor",
 "startedAt":1784922619773,"procStart":"Fri Jul 24 19:50:18 2026","version":"2.1.206",
 "peerProtocol":1,"kind":"interactive","entrypoint":"cli",
 "name":"claude-session-monitor-69","nameSource":"derived",
 "status":"busy","updatedAt":1784922678218,"statusUpdatedAt":1784922678218}
```
A second real record additionally carried `"bridgeSessionId":"session_..."`.

Field/behaviour notes from the binary:
- Writer literal: `pid:process.pid, sessionId:Rt(), cwd:rn(), startedAt:Date.now(), procStart:await wR(process.pid), version:..., peerProtocol:GGm, kind:t, entrypoint:process.env.CLAUDE_CODE_ENTRYPOINT, ... name:U4l(rn()), nameSource:"derived", logPath:process.env.CLAUDE_CODE_SESSION_LOG, agent:process.env.CLAUDE_CODE_AGENT, jobId:...`.
- `kind` enum: `["interactive","bg","daemon","daemon-worker"]` (binary `zty=[...]`).
- `status` enum: `["busy","shell","idle","waiting"]` (binary `Yty=[...]`). Mapper `Owf`: `idle->idle`, `waiting->waiting`, else `busy`. `shell` = a shell/`!`-command is in the foreground.
- `waitingFor` (string) is set alongside `status:"waiting"` to say what it is blocked on. There is also a general status-object shape in the binary using the richer enum `["idle","working","waiting","completed","archived","cancelled","rejected"]` for team members.
- Heartbeat: `updatedAt` / `statusUpdatedAt` are refreshed on every status change; a session whose `updatedAt` is old but whose pid is alive is a stuck/idle session; old `updatedAt` + dead pid is a leak.
- `tmux` field holds the `TMUX_PANE` value when the CLI runs inside tmux (binary reads `process.env.TMUX_PANE`).
- Lifecycle functions in the binary: `persistSession`, `removeSession`, `deleteSession`, `cleanupSession`. The registry is self-cleaning on graceful exit, and there is an enumerate-then-liveness-filter reader (`process.kill(pid,0)` per entry) that prunes dead pids on read - so stale files can linger until something reads the directory.

**This is the primary primitive a monitor should build on.** It gives pid, cwd, session id, coarse status, name, tmux pane, and version in one poll.

### 3.2 `~/.claude.json` (global config, ~40 KB)

Top-level keys are config/telemetry/onboarding flags (`oauthAccount`, `userID`, `machineID`, `mcpServers`, `numStartups`, `projects`, ... ~110 keys - do not print values, several are identifiers/tokens). The useful part for monitoring is `projects["<abs-path>"]`, whose subkeys include `lastSessionId`, `lastCost`, `lastDuration`, `lastAPIDuration`, `lastLinesAdded`, `lastLinesRemoved`, `lastTotalInputTokens`, `lastTotalOutputTokens`, `lastTotalCacheReadInputTokens`, `hasTrustDialogAccepted`, `mcpServers`, `allowedTools`. This is **last-run** data, not live state - stale-prone and only updated at end of run. Use it for per-project cost/history, never for liveness.

### 3.3 `~/.claude/ide/*.lock`

Directory exists but was **empty at inspection** (no IDE attached). These lockfiles are created when a VS Code / JetBrains extension connects; the filename is the WebSocket port and the JSON historically contains `pid`, `workspaceFolders`, `ideName`, `transport`, `runningInWindows`, and an `authToken`. Because I could not capture a live one, treat that inner schema as **unverified for 2.1.206**. Presence/absence of a lock is a reliable "an IDE is attached to a session in this workspace" signal; contents are stale-prone if the IDE crashes.

### 3.4 Teams / tasks / session-env (modern multi-agent)

- `~/.claude/teams/session-<shortid>/config.json`: `{name, createdAt, leadAgentId, leadSessionId, members:[{agentId, name, agentType, joinedAt, tmuxPaneId, cwd, subscriptions, backendType}]}`. `backendType` seen: `in-process`; `tmuxPaneId` can be `"leader"` or a pane id. This is authoritative for "which teammates exist in this session and where".
- `~/.claude/teams/session-<shortid>/inboxes/<member-name>.json`: message inbox array (empty when drained).
- `~/.claude/tasks/session-<shortid>/<n>.json`: `{id, subject, description, status, blocks, blockedBy}`. `status` enum `["pending","in_progress","completed","deleted"]`. Mirrors the `TaskCreated`/`TaskCompleted` hooks.
- `~/.claude/session-env/<session-id>/`: per-session scratch/env dir (often empty).

### 3.5 Other directories

- `~/.claude/statsig/`: **not present** on this machine (feature-flag/exposure cache when it exists) - irrelevant to tracking.
- `~/.claude/shell-snapshots/snapshot-zsh-<ts>-<rand>.sh`: captured shell env used to run Bash-tool commands. A fresh snapshot's timestamp correlates with a session starting, but it is not keyed to a session id - weak signal.
- `~/.claude/history.jsonl`: global prompt history (append-only). Not per-session-liveness.
- `~/.claude/daemon/`: `roster.json` = `{proto, supervisorPid, updatedAt, workers:{}}` for the background-task daemon/supervisor; `control.key` (a 32-byte control secret - do not read/print); `dispatch/` IPC dir. Relevant only if you also want to track `kind:"daemon"`/`bg` jobs.
- `~/.claude/jobs/<shortid>/` and `~/.claude/jobs/pins.json`: background job state.
- `~/.claude/sdk/`, `~/.claude/plans/`, `~/.claude/file-history/`, `~/.claude/backups/`: not liveness sources.

**Authoritative vs stale-prone:** `sessions/*.json` (authoritative for live state, self-pruning but can lag on hard kill), `teams|tasks/*` (authoritative while session alive), `ide/*.lock` (authoritative-ish, stale on IDE crash). Stale-prone: `.claude.json` projects block (last-run only), `shell-snapshots`, `history.jsonl`.

---

## 4. Process-based discovery

- **Find sessions:** `pgrep -f 'claude'` is noisy (matches this doc's own agents, editors). Prefer enumerating `~/.claude/sessions/*.json` and validating each `pid`. If you must scan processes, match the resolved binary path (`/opt/homebrew/Caskroom/claude-code/*/claude` or `~/.local/share/claude/versions/*`) and read the process's `procStart` to disambiguate pid reuse.
- **pid -> cwd:** on macOS `lsof -a -p <pid> -d cwd -Fn` (or `lsof -p <pid> | awk '$4=="cwd"'`); on Linux `readlink /proc/<pid>/cwd`. But the registry already records `cwd`, so process-level cwd is only needed for sessions started without the registry (older versions).
- **pid -> tty / tmux pane:** the registry's `tmux` field gives the pane directly. Otherwise `ps -o tty= -p <pid>`, and map tty->pane via `tmux list-panes -a -F '#{pane_tty} #{pane_id} #{session_name}:#{window_index}.#{pane_index}'`. `tmux-agent-sidebar` relies on tmux options + pane ids for exactly this.
- **Parent tree:** `ps -o ppid= -p <pid>` to see if a `claude` is a subagent/`bg` child vs a top-level interactive session; `kind` in the registry usually tells you already.
- **Liveness:** `process.kill(pid, 0)` semantics - the binary's own check is `function JA(e){if(e<=1)return!1;try{return process.kill(e,0),!0}catch...}`. From an external tool: `kill -0 <pid>` (returns 0 if alive, ESRCH if gone, EPERM if alive-but-not-yours). Combine with `procStart` match to defeat pid reuse.

---

## 5. Statusline

`statusLine` is a command configured in settings; the CLI pipes JSON on stdin. Exact schema (verbatim from the binary's embedded doc):

```
{
  "session_id": "string",       // Unique session ID
  "session_name": "string",     // Optional: human name set via /rename
  "prompt_id": "string",        // Optional: UUID of the prompt (same as OTel prompt.id)
  "transcript_path": "string",  // Path to the conversation transcript
  "cwd": "string",              // Current working directory
  "model": { "id": "string", "display_name": "string" },
  "workspace": {
    "current_dir": "string",
    "project_dir": "string",
    "added_dirs": ["string"],   // via /add-dir
    "git_worktree": "string",   // optional
    "repo": { "host": "string", "owner": "string", "name": "string" }  // optional
  },
  "version": "string",          // Claude Code version
  "output_style": { "name": "string" },
  "context_window": {
    "total_input_tokens": number,
    "total_output_tokens": number,
    "context_window_size": number,
    "current_usage": {
      "input_tokens": number, "output_tokens": number,
      "cache_creation_input_tokens": number, "cache_read_input_tokens": number
    } | null,
    "used_percentage": number | ...
  }
  // plus cost fields elsewhere in the schema: total_duration_ms, total_api_duration_ms,
  // total_lines_added, total_lines_removed, total_cost_usd, exceeds_200k_tokens
}
```
**Invocation frequency:** the statusLine command is re-run on state changes, **debounced to at most once every ~300ms** (binary: "debounces", "300ms"; docs describe it as updating as the conversation changes, not on a fixed timer).

**As a heartbeat:** usable but hacky. You *could* set `statusLine` to a script that appends `session_id`+timestamp somewhere and returns the real status text, giving you a ~300ms-granularity liveness ping while the UI is active. Downsides: it only fires when the TUI re-renders (idle sessions go quiet, so it under-reports much like hooks), it hijacks a user-facing setting, and it does not run in headless/`--print` mode. Prefer the registry `statusUpdatedAt` heartbeat, which the CLI maintains for free.

---

## 6. OTEL / telemetry export

Enable with `CLAUDE_CODE_ENABLE_TELEMETRY=1` plus standard `OTEL_*` exporter config (`OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, ...). Cardinality toggles: `OTEL_METRICS_INCLUDE_SESSION_ID` (default true), `OTEL_METRICS_INCLUDE_ENTRYPOINT`, `OTEL_METRICS_INCLUDE_VERSION`, `OTEL_METRICS_INCLUDE_ACCOUNT_UUID`. Content toggles: `OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_ASSISTANT_RESPONSES`, `OTEL_LOG_TOOL_DETAILS`. Enhanced span tracing: `CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1`.

Metric names present in the binary (`claude_code.*`): `session.count`, `interaction`, `active_time.total`, `token.usage`, `cost.usage`, `lines_of_code.count`, `commit.count`, `pull_request.count`, `code_edit_tool.decision`, `tool.execution`, `tool.blocked_on_user`, `bash.subprocess`, `subagent.spawn`, `llm_request`, `mcp.rpc`, `hook`, `compaction`, `events`, `tracing`.

Events/logs (docs, with correlation attrs): `claude_code.user_prompt` (`prompt.id`,`message.uuid`), `claude_code.assistant_response`, `claude_code.tool_result` (`tool_use_id`,`success`,`duration_ms`), `claude_code.tool_decision`, `claude_code.api_request`/`api_error`/`api_refusal`, `claude_code.permission_mode_changed` (`from_mode`,`to_mode`,`trigger`), `claude_code.auth`, `claude_code.mcp_server_connection`, `claude_code.internal_error`, `claude_code.plugin_installed`/`plugin_loaded`. All carry `session.id`.

**Session-state derivability:** session *start* is directly observable via `claude_code.session.count` (`start_type` attribute) and every event's `session.id`. **There is no session-end metric/event** - stop must be inferred from silence-after-timeout on a `session.id`, or from `permission_mode_changed`/`tool.blocked_on_user` for the waiting state. `claude_code.tool.blocked_on_user` is the cleanest telemetry signal for "waiting on the user". Latency is export-interval bound (metrics default 60s, logs default 5s), so OTEL is good for fleet dashboards but too laggy and end-blind to be a monitor's primary liveness source. It is push-to-collector, so it also requires you to run a collector.

---

## 7. SDK / headless / Agent SDK

- Headless CLI: `claude -p/--print` runs one turn and exits; `--output-format stream-json` (with `--verbose`) emits the same message objects you see in the transcript as an NDJSON stream on stdout, plus a final `result` message with cost/duration. `--input-format stream-json` lets a host feed turns. Session ids are stable across `--resume`/`--continue`.
- The registry writes `kind:"sdk"` and `entrypoint` values `sdk-cli` / `sdk-py` (binary compares `CLAUDE_CODE_ENTRYPOINT!=="sdk-cli"` / `"sdk-py"`), so SDK sessions still show up in `~/.claude/sessions/` and are trackable the same way.
- The Agent SDK (TS/Python `query()`) exposes a message stream, `SessionStart`/tool callbacks, and permission callbacks *in-process* - a programmatic host sees permission requests and tool decisions synchronously that the external monitor can only infer. If you own the host, prefer SDK callbacks; if you are external, you are back to registry+hooks+transcript.
- Headless/cron note: interactively-authenticated MCP servers may be absent in headless runs, and statusLine does not run under `--print`.

---

## 8. State detection (busy / idle / waiting / ended / crashed)

Best single source is registry `status` + `waitingFor`, corroborated by hooks:

| Target state | Registry signal | Hook signal | Transcript signal |
|---|---|---|---|
| busy (thinking/tool running) | `status:"busy"` | `UserPromptSubmit`, `PreToolUse`, `MessageDisplay` seen; no `Stop` yet | last record is user-prompt or open `tool_use` |
| idle (turn done, at prompt) | `status:"idle"` | `Stop` fired | trailing assistant text / `stop_hook_summary` |
| waiting for permission | `status:"waiting"` + `waitingFor` | `PermissionRequest`; `Notification` type `agent_needs_input` | (unreliable) |
| waiting for user input (idle prompt) | `status:"idle"`/`waiting` | `Notification` type `idle_prompt` | - |
| running foreground shell | `status:"shell"` | `PreToolUse`/`PostToolUse` for Bash | Bash `tool_use` open |
| ended (graceful) | file removed / pruned | `SessionEnd` (reason) | file stops growing |
| crashed / killed | file lingers, pid dead, `updatedAt` stale | **nothing** | file stops growing |

Known hard cases:
- **No hook on SIGKILL:** `SessionEnd` never arrives on `kill -9`/OOM/terminal loss. Only pid-liveness + stale `updatedAt` catch this. This is the #1 source of ghost sessions.
- **`Stop` fires while background bash still runs:** `Stop` means the *model turn* ended, not that all spawned processes finished. A backgrounded Bash (`&`/`run_in_background`) keeps running; do not mark the machine "done" purely on `Stop`. Cross-check child processes if you care.
- **`Notification` semantics:** it is a UI notification, not a state machine. `agent_needs_input` ~ waiting-on-user; `idle_prompt` ~ went idle; `agent_completed` ~ finished. Use `notification_type`, not the presence of the event.
- **Permission prompts:** appear as registry `status:"waiting"`/`waitingFor` and `PermissionRequest` hook; the transcript does not reliably show the pending prompt, so a transcript-only monitor misses this state entirely.
- **Plan-mode approval:** `permission_mode:"plan"` on hook payloads and a `permission-mode` transcript record / `claude_code.permission_mode_changed` OTel event; the "waiting for plan approval" pause looks like `waiting`.
- **Compaction:** `PreCompact`/`PostCompact` bracket a busy-but-not-user-driven period; treat as busy, not a new turn. Don't mistake the compaction pause for idle.

---

## 9. Stale-session cleanup / reconciliation

Signals, cheapest first: registry `updatedAt`/`statusUpdatedAt` age; pid liveness (`kill -0` + `procStart` match to defeat pid reuse); transcript-file mtime; tmux pane existence (`tmux list-panes -a`); IDE lock presence. A session is **live** if pid alive AND (`updatedAt` fresh OR transcript mtime fresh). It is **dead/leaked** if pid gone, regardless of any file still on disk. It is **stuck** if pid alive but no activity for a long grace window (surface it, don't reap it).

### Recommended reconciliation loop (pseudocode)

```
GRACE_STALE   = 90s    # updatedAt/mtime older than this => "stale, verify"
GHOST_TIMEOUT = 10s    # dead pid persists this long after last-seen => drop

state = {}   # session_id -> {pid, cwd, status, procStart, last_seen, source}

# Push path: hooks upsert immediately (SessionStart/UserPromptSubmit/Stop/
# PreToolUse/PostToolUse/Notification/PermissionRequest/SessionEnd) keyed by session_id.
on_hook(ev):
    s = state.upsert(ev.session_id, cwd=ev.cwd, transcript=ev.transcript_path)
    s.status   = derive_status(ev)        # busy/idle/waiting/shell
    s.last_seen = now()
    if ev.hook_event_name == "SessionEnd": s.pending_close = true

# Poll path: every ~2s reconcile against ground truth.
every 2s:
    reg = read_dir("~/.claude/sessions/*.json")           # authoritative registry
    for f in reg:
        if not alive(f.pid, f.procStart): continue         # skip dead registry entries
        s = state.upsert(f.sessionId, pid=f.pid, cwd=f.cwd, procStart=f.procStart)
        s.status   = f.status                              # busy|shell|idle|waiting
        s.waiting_for = f.waitingFor
        s.tmux     = f.tmux
        s.last_seen = max(s.last_seen, f.updatedAt)

    for s in state.values():
        pid_alive = s.pid and alive(s.pid, s.procStart)
        mtime_fresh = s.transcript and (now() - mtime(s.transcript) < GRACE_STALE)
        reg_present = s.session_id in reg_ids

        if not pid_alive and not reg_present:
            if now() - s.last_seen > GHOST_TIMEOUT: drop(s)   # crash/kill => reap
        elif s.pending_close and not pid_alive:
            drop(s)                                           # graceful end confirmed
        elif pid_alive and not mtime_fresh and now()-s.last_seen > GRACE_STALE:
            s.status = "stuck?"                               # surface, don't reap
        # else: keep as-is (hook status wins for freshness)

    # Optional: prune tmux-orphaned panes, and reconcile team members from
    # ~/.claude/teams/session-*/config.json for subagent rows.
```
Key rules: **pid death is the only authoritative "ended" signal**; `SessionEnd` is a hint. Always match `procStart` when checking a remembered pid. Let registry `status` win for coarse state and let hooks win for latency; use transcript mtime only as a staleness tiebreaker.

---

## 10. Comparison of approaches

| Approach | Latency | Completeness (state granularity) | Robust to crash/kill | Install burden | Version fragility |
|---|---|---|---|---|---|
| **Session registry** (`~/.claude/sessions/*.json`, poll/watch) | ~poll interval (sub-second if fsnotify) | High: pid, cwd, name, busy/shell/idle/waiting, tmux, version | High for detection (pid check), self-prunes; lingers briefly on hard kill | **Zero** (nothing to install) | Medium: undocumented, field/enum names can change |
| **Hooks** (plugin in settings) | Sub-second, push | High for transitions incl. permission/plan/compact/subagent/team | **Low**: no event on SIGKILL/crash; `SessionEnd` graceful-only | Medium: user installs a hooks plugin; only sees sessions started after install | Low-medium: event set is versioned but stable core |
| **Transcript tailing** (`projects/*/*.jsonl`) | Low (fs events) | Highest history, but **misses waiting-for-permission** | Medium: file persists after crash (no live/dead signal by itself) | Zero | High: richest schema = most churn |
| **Process scanning** (`pgrep`/`lsof`/`/proc`) | Poll interval | Low: liveness + cwd + tty only, no busy/idle | **Highest** for liveness (pid is ground truth) | Zero | Low: OS-level, version-independent |
| **Statusline heartbeat** | ~300ms while UI active | Low-medium: whatever you compute; quiet when idle; none in `--print` | Low: stops with the process, no end signal | Medium: hijacks a user setting | Low-medium |
| **OTEL export** | Seconds (export interval), end-blind | Medium: start/cost/tool/permission events, **no session-end** | Low for liveness | High: run a collector + env config | Low: stable, documented |
| **Hybrid (registry + hooks + pid/mtime reconcile)** | Sub-second transitions, poll-interval reconcile | Highest overall | **High** (pid liveness backstops missing hooks) | Low-medium (optional hooks plugin) | Medium (mitigated by multiple sources) |

Ecosystem cross-check (what other monitors lean on):

- **`tmux-agent-sidebar`** is hook-driven plus pid-sweep reconciliation, i.e. close to the hybrid recommended here, but *without* the registry. Its `hooks/hooks.json` registers **14** events (`SessionStart, SessionEnd, UserPromptSubmit, Notification, Stop, StopFailure, PermissionDenied, CwdChanged, SubagentStart, SubagentStop, PostToolUse, TaskCreated, TaskCompleted, TeammateIdle`) and a test (`hook_registrations_match_parse_arms`) pins the registration table to the parse arms so they cannot drift. Hooks write into **tmux pane options** (`@pane_status`, `@pane_cwd`, `@pane_attention`, `@pane_wait_reason`, `@pane_bg_cmd`, `@pane_subagents`, ...); the TUI reads them all back with one `tmux list-panes -a` every 1s. Reconciliation is a 10s `ps` process-tree sweep plus, for Codex/OpenCode (which have no exit hook), a "pane_current_command is a shell and no agent in the tree" teardown. Notably it **does** read `~/.claude/sessions/*.json` (`src/session.rs:33-42`) but only for the `name` field - it ignores `pid`, `status`, `cwd`, and `tmux` in the very same file, and reimplements all of that via tmux + `ps`. Identity is the tmux pane, not the session id, so an agent started outside tmux is invisible.
- **`claude-session-monitor`** (this repo) does **not** read transcripts, despite receiving `transcript_path` on every hook payload - it is discarded into a `#[serde(flatten)] _extra` catch-all (`crates/reporter/src/hook.rs:28-29`). It registers 7 events and is purely hook-driven with no liveness model: no pid, no registry read, no process scan, no sweeper, no TTL. See the gap analysis in the section below.
- `ccusage` / `claude-code-otel` / `cchistory` primarily parse the transcript JSONL and/or OTEL for cost/history rather than live liveness. `ccstatusline` consumes the section-5 statusLine JSON.

---

## Open questions / unverified

- **`ide/*.lock` inner schema for 2.1.206** - directory was empty (no IDE attached during inspection); the `{pid, workspaceFolders, ideName, transport, authToken, port-as-filename}` shape is from prior knowledge, not verified against this build.
- **`isSidechain:true` records** - none present in any current transcript on this machine, so the exact modern subagent-in-transcript layout (`sourceToolUseID` linkage) is inferred from the schema keys, not from a live example. Multi-agent now flows through teams/tasks.
- **Hook default timeouts** - taken from current docs (600s command/http/mcp, 30s prompt, 60s agent). The binary carries many `timeout:60000`/`30000` constants that are not clearly the hook defaults; older docs said a flat 60s. Not fully reconciled.
- **`waitingFor` value set** - it is a free-form string in the writer; I did not enumerate the exhaustive list of reasons it is populated with.
- **Exactly when the registry file is unlinked vs pruned-on-read** - both `deleteSession`/`cleanupSession` (explicit) and a read-time liveness filter exist; the precise ordering on abnormal exit (how long a dead entry lingers) was not measured empirically.
- **`SessionStart.source` `fork` and `SessionEnd.reason` full enum** - `fork` and the `logout|prompt_input_exit` reasons come from docs; the binary literals I captured showed `["startup","resume","clear","compact"]` and `reason:"other"` only.
- **Slug collision behaviour** - the exact normalization regex chain is approximate; always read `cwd` from a record rather than reversing the slug.

---

## Sources

Primary (shipped binary, Claude Code 2.1.206, `/opt/homebrew/Caskroom/claude-code/2.1.206/claude`, grepped with `rg -a`/`rg -aoU`):
- Canonical 30-event hook array and the 14-event user-configurable subset.
- Per-event payload constructors (`hook_event_name:"..."` literals) and base-field builder `Rf(...)`.
- Hook output schema doc block (`systemMessage`/`continue`/`hookSpecificOutput`...).
- `notificationType:"..."` literals.
- Session registry writer literal, `kind` enum `zty`, `status` enum `Yty=["busy","shell","idle","waiting"]`, mapper `Owf`, liveness `JA` (`process.kill(e,0)`), lifecycle fns `persistSession`/`deleteSession`/`cleanupSession`/`removeSession`, `tmux`/`TMUX_PANE`.
- Embedded statusLine stdin schema and `300ms` debounce.
- `claude_code.*` metric-name strings and `OTEL_*` env-var strings.
- `CLAUDE_CODE_ENTRYPOINT` comparisons (`sdk-cli`/`sdk-py`/`local-agent`).

Primary (real data on this machine, structure/field-names only, no secrets):
- `~/.claude/sessions/{1760,22684}.json` (registry records).
- `~/.claude/projects/-Users-simon-dev-claude-session-monitor/*.jsonl` (transcript `type`/key enumeration via `jq`).
- `~/.claude/teams/session-*/config.json`, `~/.claude/tasks/session-*/<n>.json`, `~/.claude/daemon/roster.json`, `~/.claude.json` (keys only), `~/.claude/ide/` (empty), `~/.claude/shell-snapshots/`.

Docs:
- https://code.claude.com/docs/en/hooks (hook events, input fields, timeouts, ordering).
- https://code.claude.com/docs/en/statusline (statusLine JSON input).
- https://code.claude.com/docs/en/monitoring-usage (OTEL env vars, metrics, events, session-end non-derivability).

Reference implementations (primitives noted, not deep-read here):
- `/Users/simon/dev/forks/tmux-agent-sidebar` - `hooks/hooks.json`, `src/adapter/claude/mod.rs` (hook-driven + tmux + pid sweeps).
- `/Users/simon/dev/claude-session-monitor` - `crates/{reporter,server,common}` (transcript + state store).

Version command: `claude --version` -> `2.1.206 (Claude Code)`.
