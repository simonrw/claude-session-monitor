# ADR 0001: Use Codex writer locks for session liveness

- Status: Accepted
- Date: 2026-08-07
- Issue: [#153](https://github.com/simonrw/claude-session-monitor/issues/153)

## Context

The Codex monitoring spec in [#151](https://github.com/simonrw/claude-session-monitor/issues/151) proposes detecting live Codex threads by probing per-thread writer locks. The earlier research could not find those locks in Codex 0.144.x, so the mechanism needed verification against a runnable release before the watcher depended on it.

OpenAI added the implementation on 2026-07-23 in [`openai/codex@5c94796`](https://github.com/openai/codex/commit/5c94796dc9e88580fdf0b05ef9ce9d975a86e1a6). The implementation creates the directory `$CODEX_HOME/thread-writer-locks`, uses `.coordination.lock` to serialize cleanup, and names per-thread locks `<thread_id>.lock`, where `thread_id` is a UUID. Codex home defaults to `~/.codex` and can be overridden with `CODEX_HOME`.

## Experiment

Official Apple Silicon release binaries were downloaded to a temporary directory and run against a real Codex CLI session. The installed Codex 0.145.0 was not changed.

| Version | Result |
| --- | --- |
| 0.145.0 | No `thread-writer-locks` directory. This predates the upstream implementation. |
| 0.146.0 | Contains the upstream writer-lock commit, but a normal `codex exec` session emitted no writer-lock directory or per-thread lock. |
| 0.147.0 | A normal `codex exec` session emitted `$CODEX_HOME/thread-writer-locks/<thread_id>.lock`. |

For Codex 0.147.0, a second process opened the live thread's lock file and attempted `flock(LOCK_EX | LOCK_NB)`:

- While the Codex session was live, the attempt failed with `EWOULDBLOCK` (`errno 35` on macOS).
- After graceful interruption, the same open file descriptor acquired the lock successfully.
- In a separate run, after sending SIGKILL to the exact Codex process, the same open file descriptor acquired the lock successfully.
- Graceful shutdown removes its per-thread lock file. SIGKILL can leave the file behind, but the kernel releases the lock, so a stale file is distinguishable from a live writer.

One run created a second UUID-named live lock in addition to the top-level session ID printed by `codex exec`. Writer locks therefore identify live Codex threads, not operating-system processes or only top-level interactive sessions. Enrichment and filtering must tolerate child threads.

## Decision

Proceed with the flock liveness spine in #156, with Codex 0.147.0 as the minimum verified stable version.

The watcher will enumerate `$CODEX_HOME/thread-writer-locks/*.lock`, excluding `.coordination.lock`, and attempt a non-blocking exclusive flock on each file. `EWOULDBLOCK` means the thread has a live writer. Successful acquisition means the file is stale and must not be published. The watcher must not delete stale files because Codex owns their cleanup.

Absence of the directory or per-thread locks is graceful degradation, not an error. In particular, the watcher must publish no guessed Codex sessions for 0.146.0 and earlier.

## Consequences

- Kernel lock ownership gives prompt cleanup after normal exit, crash, and SIGKILL without relying on a PID recorded by Codex.
- Codex 0.147.0 is the minimum verified stable version for this source.
- Version detection is unnecessary for correctness. Missing locks naturally produce an empty Codex snapshot and may produce a debug-level diagnostic.
- A lock can represent a child thread. The source must not assume every live lock maps one-to-one to a top-level CLI process.
- If a future Codex release stops emitting these locks for normal CLI sessions, fall back to no published Codex sessions and revisit the weaker process-scan plus state/mtime design before enabling it. Mtime alone is not accepted as liveness because rollout re-materialisation can refresh it.
