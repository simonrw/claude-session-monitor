//! A sandboxed `$HOME` for tests that spawn a real workspace binary.
//!
//! Every binary this workspace ships that logs to disk (`csm-watcher`,
//! `csm-reporter`, `csm-codex`) derives its log directory from `$HOME` (see
//! each binary's `main.rs`/`csm-codex.rs`). A test that spawns one of these
//! without overriding `HOME` inherits the *developer's own* `$HOME`, and so
//! appends real, permanent entries - and burns real rotation slots - into
//! the developer's actual `~/.local/share/claude-session-monitor/` (PRO-218).
//!
//! Set `.env("HOME", sandbox_home().path())` on every spawned binary's
//! `Command`, and keep the returned [`tempfile::TempDir`] alive for exactly
//! as long as the spawned process might still be writing to it - dropping
//! it early deletes the directory out from under a still-running child.

/// Returns a fresh temp directory per call (never shared or reused across
/// tests): tests run concurrently within one test binary, so a directory
/// shared across calls would let one test's spawned binary observe
/// another's.
pub fn sandbox_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("create sandbox HOME tempdir")
}
