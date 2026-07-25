//! Reading and validating entries from Claude Code's session registry.
//!
//! The registry format (`<registry-dir>/sessions/<pid>.json`) is
//! undocumented and owned by Claude Code, not this project, so parsing here
//! is deliberately lenient: unknown JSON fields are ignored (`serde`'s
//! default behaviour), and a file that fails to parse, is empty, or is not
//! valid JSON is logged at `warn` and skipped rather than failing the whole
//! sweep.

use std::path::Path;

use chrono::{NaiveDateTime, TimeZone, Utc};
use serde::Deserialize;

/// How far the registry's `procStart` (a whole-second UTC ctime string) may
/// differ from the OS's own recorded process start time before the pid is
/// treated as reused rather than the one the registry entry describes.
///
/// The registry value is truncated to whole seconds, and the OS's raw value
/// may floor differently by up to a second either side of that truncation;
/// one extra second of buffer absorbs that skew without opening the door to
/// treating a genuinely reused pid as a match, since a pid being recycled
/// within a couple of seconds of the process it replaced is not realistic.
const PROC_START_TOLERANCE_SECS: i64 = 2;

/// One entry read from a registry file, after lenient parsing. Only the
/// fields this crate needs are declared; `serde` ignores everything else in
/// the JSON object by default.
#[derive(Debug, Deserialize)]
pub(crate) struct RegistryEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub pid: i32,
    #[serde(rename = "procStart")]
    pub proc_start: String,
    pub kind: String,
    pub status: String,
    #[serde(rename = "waitingFor", default)]
    pub waiting_for: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Why a registry directory's `sessions` subdirectory could not be read.
///
/// A missing subdirectory is *not* one of these - see `read_entries` - only
/// a directory that exists but couldn't be opened (EACCES, EIO, and the
/// like) produces this.
///
/// This is structural prep for PRO-211, which owns the actual policy for a
/// failed sweep ("a failed sweep publishes nothing"). For now this crate's
/// only consumer, `sweep::sweep`, treats this exactly as it treated a
/// silent empty `Vec` before this type existed: logs a warning and
/// contributes no entries from this directory. PRO-211 will match on this
/// signal to fail the sweep instead.
#[derive(Debug)]
pub(crate) struct ReadDirError {
    pub dir: std::path::PathBuf,
    pub source: std::io::Error,
}

/// Read and leniently parse every file in `<registry_dir>/sessions/`.
///
/// A missing `sessions` subdirectory yields an empty result, not an error -
/// a registry only exists on a machine where Claude Code has actually run,
/// and a watcher configured with a directory that doesn't (yet) have one
/// must not fail its sweep. Any other error opening the directory (EACCES,
/// EIO, ...) is reported via `Err(ReadDirError)` rather than silently
/// folded into an empty `Vec`, so a caller can eventually distinguish "no
/// registry here" from "couldn't read the registry that's here" - see
/// `ReadDirError`. Per-file read/parse failures are logged at `warn` and
/// skipped individually.
pub(crate) fn read_entries(registry_dir: &Path) -> Result<Vec<RegistryEntry>, ReadDirError> {
    let sessions_dir = registry_dir.join("sessions");
    let read_dir = match std::fs::read_dir(&sessions_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                dir = %sessions_dir.display(),
                "registry directory does not exist, treating as empty"
            );
            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(ReadDirError {
                dir: sessions_dir,
                source: e,
            });
        }
    };

    let mut entries = Vec::new();
    for dirent in read_dir {
        let dirent = match dirent {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read a registry directory entry, skipping");
                continue;
            }
        };
        let path = dirent.path();
        if !path.is_file() {
            continue;
        }
        if let Some(entry) = parse_file(&path) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn parse_file(path: &Path) -> Option<RegistryEntry> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read registry file, skipping");
            return None;
        }
    };
    match serde_json::from_str::<RegistryEntry>(&body) {
        Ok(entry) => Some(entry),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse registry file, skipping");
            None
        }
    }
}

/// Parse the registry's ctime-style `procStart` string (e.g.
/// `"Fri Jul 24 20:55:59 2026"`) as UTC, returning Unix epoch seconds.
///
/// Empirically confirmed against this project's own live sessions: the
/// value is UTC (not local time) and truncated to whole seconds. Whitespace
/// is normalized before parsing because the day-of-month field may be
/// padded with either one or two spaces depending on the platform that
/// produced it, and `chrono`'s `%e` only accepts a single specific form.
fn parse_proc_start(raw: &str) -> Option<i64> {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let naive = NaiveDateTime::parse_from_str(&normalized, "%a %b %e %H:%M:%S %Y").ok()?;
    Some(Utc.from_utc_datetime(&naive).timestamp())
}

/// Whether `entry` describes a genuinely live interactive session.
///
/// Three conditions, all required: `kind == "interactive"` (background jobs
/// and daemon workers are never sessions); the pid exists per
/// `common::process::is_alive` (the registry self-prunes but lags on a hard
/// kill, so the pid, not the file's mere presence, is authoritative); and
/// the registry's `procStart` matches the OS's own recorded start time for
/// that pid within [`PROC_START_TOLERANCE_SECS`] (this is what stops a
/// pid the OS has since recycled from resurrecting an ended session).
pub(crate) fn is_live(entry: &RegistryEntry) -> bool {
    if entry.kind != "interactive" {
        return false;
    }
    if !common::process::is_alive(entry.pid) {
        return false;
    }
    let Some(registry_start) = parse_proc_start(&entry.proc_start) else {
        tracing::warn!(
            session_id = %entry.session_id,
            proc_start = %entry.proc_start,
            "failed to parse procStart, treating session as dead"
        );
        return false;
    };
    let Some(os_start) = common::process::start_time(entry.pid) else {
        tracing::warn!(
            session_id = %entry.session_id,
            pid = entry.pid,
            "could not determine OS start time for pid, treating session as dead"
        );
        return false;
    };
    (registry_start - os_start).abs() <= PROC_START_TOLERANCE_SECS
}

// Deliberately no unit tests here for directory reading, malformed/unknown
// JSON handling, or `is_live`'s kind/pid/procStart-match branches: those are
// all exercised end to end via the real registry format in
// `crates/server/tests/reconciliation.rs`, and per PRO-204's testing
// decisions this crate must not pin the shape of the parsed registry
// structs or its own internal sweep helpers, since the registry format
// belongs to Claude Code and will change.
//
// What remains are the two `parse_proc_start` tests below: pure-function
// tests of parsing logic this project owns, which the integration fixture
// cannot catch because the fixture derives `procStart` from this same
// code - a timezone regression here would silently pass every integration
// test while making every real session vanish.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_start_parses_single_and_double_space_padded_days() {
        assert!(parse_proc_start("Fri Jul 24 20:55:59 2026").is_some());
        assert!(parse_proc_start("Thu Jan  1 00:00:00 2026").is_some());
        assert!(parse_proc_start("Thu Jan 1 00:00:00 2026").is_some());
        let a = parse_proc_start("Thu Jan  1 00:00:00 2026").unwrap();
        let b = parse_proc_start("Thu Jan 1 00:00:00 2026").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn proc_start_is_parsed_as_utc_not_local() {
        // 1784926559 == 2026-07-24T20:55:59Z (matches the empirical
        // procStart -> pbi_start_tvsec correspondence used to derive the
        // parsing/tolerance approach in the first place).
        let epoch = parse_proc_start("Fri Jul 24 20:55:59 2026").unwrap();
        assert_eq!(epoch, 1784926559);
    }
}
