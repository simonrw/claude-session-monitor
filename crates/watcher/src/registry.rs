//! Reading and validating entries from Claude Code's session registry.
//!
//! The registry format (`<registry-dir>/sessions/<pid>.json`) is
//! undocumented and owned by Claude Code, not this project, so *content*
//! parsing here is deliberately lenient: unknown JSON fields are ignored
//! (`serde`'s default behaviour), and a file that is empty, malformed, or
//! not valid JSON is logged at `warn` and skipped rather than failing the
//! whole sweep - PRO-204 story 30 only requires malformed files be skipped,
//! not unreadable ones.
//!
//! *I/O* failures are a different matter and are never this lenient (PRO-211
//! review finding 4): a directory that cannot be opened or iterated, or an
//! individual file that cannot be read at all (EACCES, EIO, ...), means this
//! function's caller cannot honestly say what the registry currently
//! contains, so those failures propagate as [`ReadError`] instead of being
//! folded into the same silent skip as a malformed file.
//!
//! One I/O failure shape is an exception to that rule (PRO-211 second-round
//! review finding 1): a file present in `read_dir`'s listing but gone
//! (`ENOENT`) by the time its bytes are actually read. Claude Code deletes
//! `<pid>.json` the moment a session ends, so this is not a sign this
//! function's view of the registry is unreliable - it is a normal, expected
//! race between listing the directory and reading each entry, and it will
//! happen routinely in real use. `parse_file` treats it the same way
//! `read_entries` already treats a missing `sessions/` directory: a
//! successful, honest omission, not a whole-sweep failure. See
//! [`ReadError::File`] for why this is safe.

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

/// Why reading a registry directory's `sessions` subdirectory could not be
/// completed honestly.
///
/// A missing subdirectory is *not* one of these - see `read_entries` - a
/// registry only exists on a machine where Claude Code has actually run, so
/// that case yields a successful empty result instead. Nor is a single
/// file that fails to *parse* (empty or malformed JSON) one of these - that
/// stays a per-file warn-and-skip, since the registry's content format
/// belongs to Claude Code, not this project (see the module doc comment).
///
/// Both variants here are genuine I/O failures, split so a caller (and its
/// error message) can say which stage failed:
///
/// - [`Dir`](ReadError::Dir): the `sessions` subdirectory itself could not
///   be opened (EACCES, EIO, ...), an error surfaced while iterating its
///   entries (PRO-211 review finding 4 - before this fix, a dirent iteration
///   error was logged and skipped, silently yielding a partial directory
///   listing under `Ok`, indistinguishable from "these are all the sessions
///   there are"), *or* an entry's type could not be determined (PRO-211
///   second-round review finding 1 - before this fix, `read_entries` used
///   `path.is_file()` to decide whether an entry was worth reading, which
///   performs a `stat` and folds any error, including `EACCES`, into
///   `false`; a directory that is readable but not executable/searchable
///   lets `read_dir` list every name while every per-entry `stat` fails, so
///   every entry looked like "not a file" and was silently skipped).
/// - [`File`](ReadError::File): a specific file's *bytes* could not be read
///   at all (PRO-211 review finding 4 - before this fix, this was folded
///   into the same lenient `None` as a JSON parse failure, so an unreadable
///   file silently vanished from the swept set exactly like a malformed
///   one, even though the two mean very different things: a malformed file
///   is Claude Code's own format being weird, but an unreadable file means
///   this function cannot know whether that session is live at all).
///
///   `ENOENT` specifically is carved back out of this and does *not* become
///   a `File` error (PRO-211 second-round review finding 1): a file that
///   `read_dir` listed but has since been deleted is not "this function
///   cannot know whether that session is live" - a vanished registry file
///   means Claude Code has already removed it, which is exactly what
///   happens when a session genuinely ends, so treating it as "this session
///   is not currently in the registry" is correct, not a guess. The only
///   way this could be wrong is if the file was mid-rewrite rather than
///   deleted for good (some future Claude Code version replacing a file via
///   unlink-then-recreate rather than an atomic rename, say) - the two-sweep
///   debounce (`crate::debounce`) exists precisely to absorb a single
///   sweep's honest, transient omission of a session it saw a moment
///   earlier, so even that case self-heals within one more sweep interval
///   rather than ending the session outright. Every other `File` read
///   failure (`EACCES`, `EIO`, ...) still propagates exactly as described
///   above; only `ENOENT` gets this treatment, in `parse_file` below.
///
/// This is `sweep::sweep`'s only consumer; PRO-211 policy there is "a
/// failed sweep publishes nothing" for either variant, converted via
/// `From<ReadError> for sweep::SweepError`.
#[derive(Debug)]
pub(crate) enum ReadError {
    Dir {
        dir: std::path::PathBuf,
        source: std::io::Error,
    },
    File {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// Read and leniently parse every file in `<registry_dir>/sessions/`.
///
/// A missing `sessions` subdirectory yields an empty result, not an error -
/// a registry only exists on a machine where Claude Code has actually run,
/// and a watcher configured with a directory that doesn't (yet) have one
/// must not fail its sweep. Any other error opening or iterating the
/// directory, or reading an individual file's bytes, is reported via
/// `Err(ReadError)` rather than silently folded into a partial or empty
/// `Vec` - see [`ReadError`]. A file's bytes reading successfully but
/// failing to *parse* as the expected JSON shape is still a per-file `warn`
/// and skip, not an error - see the module doc comment.
pub(crate) fn read_entries(registry_dir: &Path) -> Result<Vec<RegistryEntry>, ReadError> {
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
            return Err(ReadError::Dir {
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
                // PRO-211 review finding 4: this used to be logged and
                // skipped (`continue`), which yielded a partial directory
                // listing under `Ok` - indistinguishable from "these are
                // all the live sessions there are". An error partway
                // through iterating `read_dir` means this function no
                // longer knows the true membership of the directory, so it
                // must fail the same way a failure to open the directory
                // does.
                return Err(ReadError::Dir {
                    dir: sessions_dir,
                    source: e,
                });
            }
        };
        // PRO-211 second-round review finding 1: this used to check
        // `dirent.path().is_file()`, which calls `fs::metadata` (a `stat`)
        // and folds *any* error - including `EACCES` - into `false`. A
        // `sessions` directory that is readable but not executable/
        // searchable (e.g. mode `0o444`) lets `read_dir` list every entry's
        // name successfully while every per-entry `stat` fails with
        // `EACCES`; `is_file()` swallowed that as "not a file", so every
        // entry was silently skipped and the sweep returned a successful,
        // empty result. `DirEntry::file_type()` reads the type `readdir`
        // already returned (`d_type`) without a further `stat` call, so it
        // does not require search permission on the parent and surfaces the
        // real failure instead of masking it. Its `Err` is a directory-level
        // read failure, exactly like a `read_dir` iteration error above; a
        // dangling symlink or any other stat-unreadable entry that
        // `file_type()` itself resolves without error still falls through to
        // `parse_file`, whose own `read_to_string` will surface *its*
        // failure to open as `ReadError::File`.
        let path = dirent.path();
        match dirent.file_type() {
            Ok(ft) if ft.is_dir() => continue,
            Ok(_) => {}
            Err(e) => {
                return Err(ReadError::Dir {
                    dir: sessions_dir,
                    source: e,
                });
            }
        }
        if let Some(entry) = parse_file(&path)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Read and parse one registry file. An I/O failure reading its bytes
/// (`Err(ReadError::File)`) is distinct from a JSON parse failure
/// (`Ok(None)`, logged at `warn`) - see [`ReadError`]'s doc comment for why
/// these must not be folded together. `ENOENT` is a further exception to
/// that split (PRO-211 second-round review finding 1): it is treated as a
/// third, successful outcome (`Ok(None)`, logged at `debug` since it is
/// routine rather than noteworthy) rather than `ReadError::File` - see
/// [`ReadError::File`]'s doc comment for why a file that vanished between
/// `read_dir` listing it and this function reading it is not the same kind
/// of "cannot know" as every other read failure.
fn parse_file(path: &Path) -> Result<Option<RegistryEntry>, ReadError> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                path = %path.display(),
                "registry file vanished between listing and reading (the session it \
                 described has ended), skipping"
            );
            return Ok(None);
        }
        Err(e) => {
            return Err(ReadError::File {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    match serde_json::from_str::<RegistryEntry>(&body) {
        Ok(entry) => Ok(Some(entry)),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse registry file, skipping");
            Ok(None)
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
