//! Automatic discovery of Claude Code session registry directories from
//! live process environments (PRO-208).
//!
//! Enumerate every process whose invoked executable looks like a Claude
//! binary, read each one's environment, and take the union of their
//! `CLAUDE_CONFIG_DIR` values (defaulting to `~/.claude` per process where
//! unset) to get the set of registry directories to sweep. A profile
//! created after the watcher started is picked up automatically, because
//! this is a plain function called fresh on every sweep - there is no
//! cached process list to go stale.
//!
//! The same read also captures each process's `TMUX_PANE`, keyed by pid:
//! the registry's own `tmux` field was observed `null` even for sessions
//! demonstrably running inside tmux (see
//! `docs/research/2026-07-24-claude-session-tracking.md`), so the process
//! environment is the only reliable source, and one read serves both
//! purposes. This crate does not resolve a pane id into an activation
//! target - that is PRO-209 - but the pane ids are captured here, alongside
//! the registry directories, from the exact same enumeration, so PRO-209
//! only has to consume [`Discovery::tmux_panes`] rather than read the
//! environment a second time.
//!
//! The same enumeration also captures every live Claude process's pid, kept
//! as [`Discovery::live_pids`] (PRO-211): a "fail loudly" check independent
//! of directory discovery entirely, comparing this set against the pids
//! actually found in the registry to warn on a live process the registry
//! does not know about - see `sweep::sweep`'s use of it.
//!
//! [`sweep::registry_dirs_from_env`](crate::sweep::registry_dirs_from_env)
//! remains a real escape hatch: whenever it yields at least one directory,
//! the caller must use that list directly and never call [`discover`] at
//! all. Discovery itself is only invoked when that override is absent.
//! The override bypasses *directory* discovery only, not pane capture or
//! live-pid capture: [`discover_process_snapshot`] runs the same process
//! enumeration purely for `TMUX_PANE`s and live pids, so a session found via
//! the override still resolves a `tmux_target` rather than silently losing
//! it. `live_pids` is still captured here too, but (PRO-211 review finding
//! 3) `main.rs` deliberately does not feed it to the orphaned-live-process
//! check while the override is set: under the override `live_pids` covers
//! every Claude process on the *host*, while the entries the sweep can
//! possibly see are scoped to only the override's own directories, so every
//! real session outside those directories would compare as an "orphan" -
//! reproduced directly as a false positive on every live process on the
//! machine, every sweep, not a real gap the check exists to surface.
//!
//! The impure part - actually enumerating OS processes - is confined to
//! the per-platform `imp::enumerate_claude_processes` (macOS: `ps -Eww`;
//! Linux: `/proc/<pid>/{cmdline,environ}`). Everything else in this file is
//! a pure function of already-read bytes and is tested against fixture
//! data, including - per PRO-204's testing decisions - the Linux
//! `/proc/<pid>/environ` parser, even though the impure Linux enumeration
//! itself cannot be exercised from macOS.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The environment variable Claude Code reads to relocate its config
/// directory (and therefore its session registry, at
/// `<dir>/sessions/<pid>.json`).
const CLAUDE_CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";

/// The environment variable tmux sets in every process running inside a
/// pane, identifying that pane (e.g. `%38`).
const TMUX_PANE_VAR: &str = "TMUX_PANE";

/// The environment variable a process's own default config directory is
/// derived from (`<HOME>/.claude`), read from that *process's* environment -
/// see [`default_config_dir_for`] (PRO-211 second-round review finding 3).
const HOME_VAR: &str = "HOME";

/// What automatic discovery found: the deduplicated registry directories to
/// sweep, each live Claude process's tmux pane keyed by pid, and the full
/// set of live Claude process pids (PRO-211's orphaned-live-process check).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Discovery {
    pub registry_dirs: Vec<PathBuf>,
    pub tmux_panes: HashMap<i32, String>,
    pub live_pids: HashSet<i32>,
}

/// Process enumeration failed outright: the `ps` invocation errored, or
/// `/proc` itself could not be read.
///
/// This is deliberately narrow. It must **not** cover "enumeration
/// succeeded and found zero *Claude* processes" - that is a genuinely
/// empty, genuinely successful result (see [`discover`]). It also must not
/// cover a *non*-Claude process whose environment happens to be unreadable
/// (e.g. `/sbin/launchd`, owned by another user) - that process was never
/// going to contribute a registry directory, so its unreadable environment
/// is not a "cannot determine" case at all. Only a failure of enumeration
/// itself, or a confirmed Claude process's environment being unreadable
/// (see [`UnreadableEnvironment`](DiscoveryError::UnreadableEnvironment)
/// below), belongs here, because only those failures mean the true set of
/// registry directories (or live Claude processes) is unknown, and
/// publishing an empty or short snapshot in that case would be
/// indistinguishable from "no sessions exist".
///
/// [`EmptyProcessList`](DiscoveryError::EmptyProcessList) is the other side
/// of that same line: zero processes *total* (before any Claude filter is
/// applied) is never a genuine observation on a live host - there is always
/// at least the OS's own init process and the enumerator itself - so it can
/// only mean the enumerator itself is broken (a stubbed or shadowed `ps`
/// that exits 0 printing nothing, for instance). This was reproduced
/// directly: with such a `ps` on `PATH`, the pre-fix code treated an empty
/// *unfiltered* list exactly like a filtered list matching no Claude
/// processes, reported "no live Claude Code processes found", and
/// published (and thereby ended) every real session on the host. Both
/// enumerators must check the unfiltered count - before `is_claude_exe`
/// narrows it - and return this variant when it is zero.
///
/// [`UnreadableEnvironment`](DiscoveryError::UnreadableEnvironment) (PRO-211
/// review finding 4) is for a process **already confirmed** to be a Claude
/// binary by its invoked name (`is_claude_exe`) whose environment could
/// then not be determined at all, and whose *uid matches this watcher's
/// own* - see below for why uid matters. On Linux, `/proc/<pid>/environ`
/// failed to read; on macOS, `ps -Eww` reported zero environment tokens for
/// that line. Before this fix both cases were logged and skipped, silently
/// dropping that profile's `CLAUDE_CONFIG_DIR` from `union_discovery`'s
/// input and thereby ending its sessions with a successful exit - exactly
/// the class of bug this ticket exists to close structurally. This is
/// narrower than it might look: a process confirmed *not* to be Claude with
/// a genuinely empty environment is unaffected - see
/// `parse_ps_output_handles_a_process_with_no_environment_at_all`'s pid-1
/// (`/sbin/launchd`) case, which is filtered out by `is_claude_exe` before
/// this check ever runs - and a *Claude* process with a present-but-narrow
/// environment (no `CLAUDE_CONFIG_DIR` or `TMUX_PANE` key, but other keys
/// still read) is also unaffected - see
/// `discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_and_tmux_pane_from_one_read`'s
/// pid-23195 case; both are real, successful observations, not this error.
///
/// **uid matters (PRO-211 second-round review finding 2).** A Claude-matched
/// line with zero environment tokens does not only mean "`ps` could not read
/// this" - `ps -Eww` prints the full command line for a process owned by
/// *another* user while suppressing its environment entirely, rather than
/// erroring, which is indistinguishable by content alone from a genuine read
/// failure. Verified directly on a real host: `1 /sbin/launchd` and `653
/// /usr/libexec/corebrightnessd --launchd` (both root-owned) both print a
/// full command with zero environment tokens under this watcher's own,
/// unprivileged uid. Before this fix, `build_claude_processes` treated *any*
/// Claude-matched empty-environment line as this error - so a legitimate,
/// unrelated `sudo claude` (or any Claude process on a shared/multi-user
/// host owned by someone else) turned into a **permanent** discovery
/// failure: every sweep, for as long as that foreign process lived, `ps`
/// reported it the same way, `discover()` returned `Err` every time, and the
/// watcher published nothing at all - loud rather than silent, the right
/// direction, but a total, indefinite outage from a legitimate
/// configuration, not a transient one. A foreign-uid Claude process with an
/// unreadable environment is now a warning and a skip (see
/// [`ForeignUidWarnings`]), not this error; only a *same-uid* Claude process
/// - one this watcher genuinely should have been able to read - still
/// becomes this error.
/// [`MalformedPsLine`](DiscoveryError::MalformedPsLine) (PRO-211 third-round
/// review finding 2) is for a `ps -Eww` output line that is not blank but
/// still cannot be parsed into the `pid uid command...` shape the invocation
/// asks for. `ps -o pid=,uid=,command=` is machine-formatted specifically so
/// this project does not have to guess at column widths or headers, so a
/// line that violates that shape - a pid or uid column containing something
/// non-numeric, or a missing uid column entirely - means something is
/// actually wrong (a `ps` version mismatch, an unexpected locale, a
/// genuinely corrupt invocation), not an exotic-but-valid process this
/// project simply doesn't recognise. Before this fix, `parse_ps_output`
/// silently dropped any line it could not parse via `filter_map`; reachable
/// today only because the `uid=` column (added for PRO-211 second-round
/// review finding 2) widened the required-column count from one to two, so
/// a line whose *uid* column alone is malformed can now fail to parse while
/// its pid still looks valid - and a silently-incomplete process list is
/// exactly the shape PRO-211 exists to eliminate: reproduced directly with
/// two live Claude profiles, malforming only one profile's uid column
/// published the other profile alone, with a successful exit, ending every
/// session the malformed profile had. Two shapes are deliberately *not*
/// this error, and stay silently skipped, because they are real, benign
/// output rather than a violation of the expected shape - see
/// `parse_ps_line`'s doc comment.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("failed to enumerate processes: {0}")]
    Enumerate(#[source] std::io::Error),
    #[error(
        "process enumeration returned zero processes total, which is impossible on a live \
         host; treating this as a broken enumerator rather than a genuine observation"
    )]
    EmptyProcessList,
    #[error(
        "process {pid} matched as a Claude Code process but its environment could not be \
         determined: {source}"
    )]
    UnreadableEnvironment {
        pid: i32,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse a line of `ps -Eww -ax -o pid=,uid=,command=` output: {line:?}")]
    MalformedPsLine { line: String },
}

/// One live process whose invoked executable looked like a Claude binary,
/// with the three environment values this project reads from it.
///
/// `home` (PRO-211 second-round review finding 3) is this *process's own*
/// `HOME`, captured from the same environment read as `config_dir` and
/// `tmux_pane` - see [`default_config_dir_for`] for why a process's own
/// `HOME` must be preferred over the watcher's when resolving its default
/// config directory.
#[derive(Debug, Clone, PartialEq)]
struct ClaudeProcess {
    pid: i32,
    config_dir: Option<String>,
    tmux_pane: Option<String>,
    home: Option<String>,
}

/// Enumerate live Claude Code processes and derive the registry
/// directories and tmux panes to use for this sweep.
///
/// Returns `Ok` with a [`Discovery`] whose `registry_dirs` always contains
/// at least the default config directory (see `union_discovery`'s doc
/// comment), even when enumeration succeeded but matched zero Claude
/// processes - there genuinely are no *extra* profiles, and sweeping just
/// the default directory (which itself degrades to a successful empty
/// sweep per PRO-207 if it has no registry) is the correct, self-healing
/// result. Returns `Err` only when enumeration itself could not be
/// completed, including the unfiltered-empty-list floor both platform
/// enumerators apply (see `DiscoveryError::EmptyProcessList`); the caller
/// must treat that as a read failure and publish nothing, exactly as
/// PRO-207 refused to publish when no registry directory was configured at
/// all.
///
/// `foreign_warnings` carries the cross-sweep warn-once state for a
/// foreign-uid Claude process whose environment cannot be read (PRO-211
/// second-round review finding 2) - see [`ForeignUidWarnings`]. The caller
/// owns it for the life of the process, exactly like `sweep::
/// OrphanWarnings`, so a given pid warns once while it stays in that state
/// rather than on every sweep.
pub fn discover(foreign_warnings: &mut ForeignUidWarnings) -> Result<Discovery, DiscoveryError> {
    let processes = imp::enumerate_claude_processes(foreign_warnings)?;
    Ok(union_discovery(&processes))
}

/// Tmux panes and live pids captured from one process enumeration, without
/// deriving any registry directory from it. See [`discover_process_snapshot`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProcessSnapshot {
    pub tmux_panes: HashMap<i32, String>,
    pub live_pids: HashSet<i32>,
}

/// Capture tmux panes and live pids only, from the same process enumeration
/// [`discover`] uses, without deriving any registry directory from it.
///
/// This exists for `main.rs`'s explicit-directory-override path
/// (`CSM_WATCHER_REGISTRY_DIRS`): that override supplies its own registry
/// directories directly and so has no need of directory *discovery*, but it
/// still needs pane ids to resolve `tmux_target` - the override bypasses
/// discovering *where to look for sessions*, not enrichment of the sessions
/// once found. Before pane capture existed here, the override path reported
/// an empty pane map unconditionally, so no session published under the
/// override ever had a `tmux_target`, even when running inside tmux - a
/// silent, user-visible loss of jump-to-session for what PRO-204 documents
/// as a permanent, supported escape hatch, not scaffolding.
///
/// Live pids are captured here too, from the same enumeration (avoiding
/// paying for `ps`/`/proc` a second time per sweep under the override), but
/// - unlike the normal discovery path - `main.rs` deliberately does not feed
/// them to `sweep::sweep`'s orphaned-live-process check while the override
/// is set: see [`Discovery`]'s module-level doc comment above for why doing
/// so would be a pure false positive (PRO-211 review finding 3), not the
/// real gap the check exists to surface. `live_pids` is still returned here
/// rather than dropped, since it costs nothing to capture and keeps this
/// function's shape simple and uniform with the normal discovery path.
///
/// Unlike [`discover`], enumeration failing here is not propagated as an
/// error: this is enrichment, not the truth about which sessions exist, so
/// it degrades to an empty snapshot - "no tmux panes, no live pids this
/// sweep" - exactly like `tmux::resolve_all_panes` degrades when `tmux`
/// itself is unavailable; the caller must never let a capture failure
/// block, delay, or fail the sweep the override path is otherwise
/// perfectly able to complete.
pub fn discover_process_snapshot(foreign_warnings: &mut ForeignUidWarnings) -> ProcessSnapshot {
    match imp::enumerate_claude_processes(foreign_warnings) {
        Ok(processes) => {
            let tmux_panes = processes
                .iter()
                .filter_map(|p| p.tmux_pane.clone().map(|pane| (p.pid, pane)))
                .collect();
            let live_pids = processes.iter().map(|p| p.pid).collect();
            ProcessSnapshot {
                tmux_panes,
                live_pids,
            }
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "process enumeration failed while capturing tmux panes and live pids for the \
                 explicit registry-dirs override; degrading to no enrichment and no \
                 orphaned-process check this sweep"
            );
            ProcessSnapshot::default()
        }
    }
}

/// Union `processes`' config directories - defaulting each unset, blank, or
/// non-absolute one to `~/.claude` - into a deduplicated directory list, and
/// collect their tmux panes keyed by pid.
///
/// The default config directory is seeded into the result unconditionally,
/// before any process is even considered - not only as the per-process
/// fallback. This is a deliberate floor against a total miss in
/// [`is_claude_exe`]: an install that presents itself as, say, `node
/// <cli>` in the process list (an `exec node … cli.js` wrapper, an older
/// npm install with `bin: cli.js`, or an install path containing a space,
/// which the `ps` line parser's tokenizer also mishandles) matches zero
/// entries in `processes`, and without this seed `union_discovery` would
/// return an empty directory list - a silent, total wipe of every session
/// on the host, indistinguishable from a genuine "nothing running" sweep by
/// the time it reaches `sweep`. Reproduced directly: feeding
/// `union_discovery` a process list containing only a `node <cli>`-shaped
/// entry (no match in `is_claude_exe`) produced zero directories before
/// this seed existed.
///
/// The seed is a floor, not a cure. It rescues only the *default* profile:
/// a session under a non-default `CLAUDE_CONFIG_DIR` whose process
/// `is_claude_exe` fails to recognise is still ended, and still with a
/// success exit, because nothing else knows that directory exists.
/// Widening the recognition itself is the only real fix for that, and is
/// tracked separately.
///
/// This can never *resurrect* a session: every directory this function
/// returns is only ever consulted by `sweep`, which pid- and
/// `procStart`-verifies every entry it reads before treating it as live.
/// The seeded directory either has no `sessions/` subdirectory (a
/// successful empty read, per PRO-207), or has one but every entry in it is
/// independently verified. The only observable effect of seeding it is that
/// a total `is_claude_exe` miss degrades to "the default profile is still
/// tracked" instead of a silent wipe.
///
/// This function is only ever reached on the discovery path - `main.rs`
/// calls it exclusively when the explicit `CSM_WATCHER_REGISTRY_DIRS`
/// override is absent - so seeding it here does not weaken that override's
/// promise to bypass discovery (and this default) entirely.
fn union_discovery(processes: &[ClaudeProcess]) -> Discovery {
    let mut registry_dirs = vec![default_config_dir()];
    let mut tmux_panes = HashMap::new();
    let mut live_pids = HashSet::new();
    for p in processes {
        let dir = resolve_process_config_dir(p.config_dir.as_deref(), p.home.as_deref());
        if !registry_dirs.contains(&dir) {
            registry_dirs.push(dir);
        }
        if let Some(pane) = &p.tmux_pane {
            tmux_panes.insert(p.pid, pane.clone());
        }
        live_pids.insert(p.pid);
    }
    Discovery {
        registry_dirs,
        tmux_panes,
        live_pids,
    }
}

/// Resolve one process's `CLAUDE_CONFIG_DIR` value into the directory to
/// sweep for it, falling back to [`default_config_dir_for`] (that same
/// process's own `HOME`, not the watcher's - see its doc comment) when the
/// value is absent, blank, or not an absolute path.
///
/// A relative value is rejected rather than resolved against the watcher's
/// own working directory: the value came from the *Claude process's*
/// environment, and its cwd is not the watcher's, so resolving it against
/// the watcher's cwd would sweep an arbitrary, almost certainly wrong,
/// directory - silently ending that profile's sessions on every future
/// sweep - rather than the one Claude Code actually uses. Falling back to
/// the process's own default config directory instead is not correct either
/// (Claude Code's own resolution of a relative `CLAUDE_CONFIG_DIR` is
/// unspecified here), but it fails toward "still tracked under some
/// directory" rather than toward "silently sweeping the wrong one".
fn resolve_process_config_dir(config_dir: Option<&str>, process_home: Option<&str>) -> PathBuf {
    match config_dir {
        Some(d) if !d.trim().is_empty() => {
            let trimmed = d.trim();
            let path = PathBuf::from(trimmed);
            if path.is_absolute() {
                path
            } else {
                tracing::warn!(
                    value = trimmed,
                    "CLAUDE_CONFIG_DIR is not an absolute path; falling back to the default \
                     config directory rather than resolving it against the watcher's own cwd"
                );
                default_config_dir_for(process_home)
            }
        }
        _ => default_config_dir_for(process_home),
    }
}

/// Resolve one Claude process's default config directory (`<HOME>/.claude`)
/// using *that process's own* `HOME` - captured from the same environment
/// read as `CLAUDE_CONFIG_DIR` - falling back to the watcher's own
/// [`default_config_dir`] only when the process's `HOME` is absent, blank,
/// or not an absolute path (PRO-211 second-round review finding 3,
/// pre-existing since PRO-208).
///
/// Before this fix, every process's default (`CLAUDE_CONFIG_DIR` unset) was
/// resolved against the *watcher's* `$HOME` unconditionally, via
/// `default_config_dir` alone, even though that process's own `HOME` was
/// already sitting right there in the very environment this module reads
/// `CLAUDE_CONFIG_DIR` and `TMUX_PANE` from. A watcher whose own `$HOME`
/// differs from the session owner's - a system-installed launchd/systemd
/// unit, a service account, a watcher started via `sudo` or under `su` -
/// swept the *watcher's* default profile instead of the real one, with a
/// successful exit: the wrong directory either has no `sessions/`
/// subdirectory (an empty sweep) or an unrelated one, and either way the
/// session owner's real default-profile sessions are absent from the
/// published snapshot and get ended.
///
/// A process's `HOME` reaching here as `None` is never itself the
/// "unreadable environment" case: `build_claude_processes` (and the Linux
/// enumerator) already turn a genuinely unreadable environment into
/// `DiscoveryError::UnreadableEnvironment` or a foreign-uid skip before a
/// `ClaudeProcess` is ever constructed - see their doc comments. So a
/// `None` here is an honest observation ("this process's environment was
/// read successfully and does not set `HOME`"), and falling back to the
/// watcher's own default is the documented, acceptable degrade for that
/// case, not a silent resolution to the wrong directory.
fn default_config_dir_for(process_home: Option<&str>) -> PathBuf {
    match process_home {
        Some(h) if !h.trim().is_empty() => {
            let trimmed = h.trim();
            let path = PathBuf::from(trimmed);
            if path.is_absolute() {
                path.join(".claude")
            } else {
                tracing::warn!(
                    value = trimmed,
                    "a Claude process's HOME is not an absolute path; falling back to the \
                     watcher's own default config directory rather than resolving it against \
                     the watcher's own cwd"
                );
                default_config_dir()
            }
        }
        _ => default_config_dir(),
    }
}

/// `~/.claude`, using *the watcher's own* `$HOME` the same way `main.rs`
/// already does for the log directory. This is a floor for two cases only:
/// `union_discovery`'s unconditional seed (there is no specific process to
/// derive it from), and [`default_config_dir_for`]'s own fallback when a
/// process's `HOME` is unavailable - it is deliberately **not** used
/// directly as any *process's* default config directory anymore (PRO-211
/// second-round review finding 3); see `default_config_dir_for`. Falling
/// back to `/.claude` when `$HOME` is unset mirrors `main.rs`'s log
/// directory precedent rather than inventing a new one; a watcher running
/// with no `$HOME` at all has bigger problems than this fallback's exact
/// value.
fn default_config_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
}

/// Whether an executable path/name - the first whitespace-separated token
/// of a process's command line, i.e. how it was invoked - looks like a
/// Claude Code binary: its file stem (name without extension) is `claude`,
/// case-insensitively.
///
/// Matches a bare `claude` on `PATH`, a full install path
/// (`/opt/homebrew/Caskroom/claude-code/<ver>/claude`,
/// `~/.local/share/claude/versions/<ver>/claude`), and a `.exe`-suffixed
/// shim name observed from an npm-based install. Does not match
/// differently-named tools such as `claude-code` or `claudex`, which are
/// not Claude Code's own CLI process.
fn is_claude_exe(exe: &str) -> bool {
    Path::new(exe)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("claude"))
}

/// Parse `ps -Eww -ax -o pid=,uid=,command=` output: one process per line,
/// `pid`, then `uid`, then the invoked command, its arguments, and its
/// environment - all whitespace-joined by `ps` with no quoting or escaping.
/// Returns every parseable line as `(pid, uid, invoked_exe, env)`,
/// unfiltered; callers narrow to Claude processes with [`is_claude_exe`].
/// `uid` is what lets [`build_claude_processes`] distinguish a foreign
/// user's process from a genuine read failure of this watcher's own user's
/// process (PRO-211 second-round review finding 2).
///
/// A line that cannot be parsed is a discovery failure
/// ([`DiscoveryError::MalformedPsLine`], PRO-211 third-round review finding
/// 2), not a silent skip - see the error variant's doc comment for why, and
/// `parse_ps_line`'s for the two shapes of non-parsing line that are *not*
/// this error.
///
/// Compiled whenever this is the real parser in use (macOS) or whenever
/// tests are running (so the fixture tests below exercise it on every CI
/// platform, not only macOS) - never in a plain non-macOS, non-test build,
/// where it would otherwise be legitimately unused dead code.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_output(
    output: &str,
) -> Result<Vec<(i32, u32, String, HashMap<String, String>)>, DiscoveryError> {
    let mut result = Vec::new();
    for line in output.lines() {
        match parse_ps_line(line) {
            PsLineOutcome::Entry(entry) => result.push(entry),
            PsLineOutcome::Benign => continue,
            PsLineOutcome::Malformed => {
                tracing::error!(
                    line,
                    "failed to parse a line of `ps -Eww -ax -o pid=,uid=,command=` output; \
                     treating this as a discovery failure rather than silently dropping the \
                     line, since this output is machine-formatted and a parse failure means \
                     something is actually wrong"
                );
                return Err(DiscoveryError::MalformedPsLine {
                    line: line.to_string(),
                });
            }
        }
    }
    Ok(result)
}

/// The three ways one line of `ps -Eww -ax -o pid=,uid=,command=` output can
/// come back from [`parse_ps_line`].
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq)]
enum PsLineOutcome {
    /// A full `pid uid command...` line, parsed successfully.
    Entry((i32, u32, String, HashMap<String, String>)),
    /// Not a parse failure at all: a real, legitimate line that simply
    /// carries no process entry - see `parse_ps_line`'s doc comment for the
    /// two shapes this covers. Silently skipped, exactly like the pre-fix
    /// behaviour, but *only* for these specific shapes rather than for any
    /// unparseable line whatsoever.
    Benign,
    /// A non-blank line that does not fit the `pid uid command...` shape at
    /// all - see [`DiscoveryError::MalformedPsLine`]. The caller must fail
    /// discovery over this, not skip it.
    Malformed,
}

/// Parse one line of `ps -Eww -ax -o pid=,uid=,command=` output.
///
/// Two shapes are legitimate, real output rather than a violation of the
/// expected format, and come back as [`PsLineOutcome::Benign`] rather than
/// [`PsLineOutcome::Malformed`] (PRO-211 third-round review finding 2):
///
/// - **A blank line.** Nothing at all, or only whitespace - not a process
///   entry to begin with. `ps` invocations do not normally emit one, but a
///   stubbed or wrapped `ps` (as this project's own test suite uses) might,
///   and there is no reason to treat an empty line as a sign something is
///   wrong when it plainly carries no data either way.
/// - **A process with an empty command column.** `pid` and `uid` both parse
///   as real numbers, but nothing follows: `ps` printed nothing at all in
///   the `command=` column for that process. This is a real, if rare, shape
///   `ps` can produce (not every process necessarily has a non-empty argv[0]
///   `ps` is willing to print), and it can never be a Claude process either
///   way (`is_claude_exe` needs a name to match against), so it is dropped
///   as uninteresting rather than failing discovery over it.
///
/// Every other non-parsing shape - a garbled or missing pid, a garbled or
/// entirely missing uid column, anything else that does not fit
/// `pid uid command...` - is [`PsLineOutcome::Malformed`]: `ps -o
/// pid=,uid=,command=` guarantees pid and uid are always present and
/// numeric for any real process line, so a line that violates that
/// guarantee is never simply an unusual-but-valid process; a header line
/// would fall in this bucket too, but this project's `=`-suffixed column
/// spec (`pid=,uid=,command=`) already suppresses `ps` from ever emitting
/// one.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_line(line: &str) -> PsLineOutcome {
    if line.trim().is_empty() {
        return PsLineOutcome::Benign;
    }
    let line = line.trim_start();
    let Some((pid_str, rest)) = line.split_once(char::is_whitespace) else {
        return PsLineOutcome::Malformed;
    };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        return PsLineOutcome::Malformed;
    };
    let rest = rest.trim_start();
    let Some((uid_str, rest)) = rest.split_once(char::is_whitespace) else {
        return PsLineOutcome::Malformed;
    };
    let Ok(uid) = uid_str.trim().parse::<u32>() else {
        return PsLineOutcome::Malformed;
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        // pid and uid both parsed; there is simply no command left - see
        // this function's doc comment for why that is benign, not malformed.
        return PsLineOutcome::Benign;
    }
    let mut tokens = rest.split(' ').filter(|t| !t.is_empty());
    let Some(exe) = tokens.next() else {
        // rest is non-empty but contains no non-whitespace token at all
        // (e.g. embedded non-space whitespace only) - the same "no command"
        // shape as above.
        return PsLineOutcome::Benign;
    };
    let exe = exe.to_string();
    let env = parse_env_tokens(tokens);
    PsLineOutcome::Entry((pid, uid, exe, env))
}

/// Reconstruct `KEY=VALUE` pairs from `ps -E`'s whitespace-joined, unquoted
/// dump of a command's arguments followed by its environment.
///
/// A token starts a new pair only when it looks like a real env-var
/// assignment (`^[A-Za-z_][A-Za-z0-9_]*=`, checked by
/// [`env_assignment_key_len`]); any other token is folded into the value of
/// whichever pair most recently started. This is how a value containing
/// spaces - observed in the wild, e.g. a `PATH` entry
/// `/Applications/VMware Fusion.app/...` - gets put back together. Tokens
/// before the first such pair (the command's own name and arguments) are
/// dropped.
///
/// This reconstruction is inherently ambiguous when a *value* happens to
/// contain a substring shaped like `IDENT=...` of its own - `ps -E` gives
/// no way to disambiguate that from a real boundary. This is not merely a
/// hypothetical about `CLAUDE_CONFIG_DIR` or `TMUX_PANE`'s own values (which
/// are indeed always a bare filesystem path or a `%<n>` pane id and never
/// contain such a substring): it is a real shape produced by an exported
/// bash function, whose `ps`/`environ` representation is
/// `BASH_FUNC_name%%=() { ...body... }`, where `...body...` is
/// unconstrained shell source and can itself contain `CLAUDE_CONFIG_DIR=` -
/// exactly the two-profile helper shape
/// `BASH_FUNC_cw%%=() { CLAUDE_CONFIG_DIR=/opt/personal claude "$@" }`
/// reproduces. A token sequence like that creates a *second*, spurious
/// `CLAUDE_CONFIG_DIR=` boundary inside another variable's value, after the
/// real one.
///
/// The fix is first-wins (`entry(..).or_insert(..)` below), not last-wins
/// (`insert`, which this file used before this fix and which a spurious
/// later boundary silently overwrites). A real environment block from the
/// OS never has a duplicate key - every key in `/proc/<pid>/environ` and
/// in a live process's real environment is unique by construction - so any
/// repeat encountered here is not a second real variable but a false split
/// of some other value. First-wins keeps the genuine assignment and drops
/// the impostor; last-wins does the opposite.
///
/// This is an ordering argument, not a proof: first-wins is correct only
/// because the impostor appears *after* the real assignment. It does so in
/// the case that matters, since bash emits its `BASH_FUNC_*` exports after
/// regular variables - verified on bash 3.2.57 and 5.3.15. An impostor
/// planted ahead of the real assignment would still win, so this narrows
/// the hazard rather than eliminating it.
/// Reproduced directly against a `ps -Eww`-shaped line carrying that exact
/// `BASH_FUNC_cw%%=` token sequence: pre-fix (`insert`), the recovered
/// `CLAUDE_CONFIG_DIR` was the fake path embedded in the function body, not
/// the process's real one, so the real profile's registry directory was
/// never swept and all its sessions were ended.
#[cfg(any(target_os = "macos", test))]
fn parse_env_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for token in tokens {
        if let Some(key_len) = env_assignment_key_len(token) {
            if let Some((key, value)) = current.take() {
                env.entry(key).or_insert_with(|| value.join(" "));
            }
            current = Some((token[..key_len].to_string(), vec![&token[key_len + 1..]]));
        } else if let Some((_, value)) = current.as_mut() {
            value.push(token);
        }
    }
    if let Some((key, value)) = current {
        env.entry(key).or_insert_with(|| value.join(" "));
    }
    env
}

/// If `token` looks like `IDENT=...`, returns the byte length of `IDENT`
/// (the index of the `=`). A valid `IDENT` starts with a letter or
/// underscore and continues with letters, digits, or underscores - the
/// same shape every real environment variable name has.
#[cfg(any(target_os = "macos", test))]
fn env_assignment_key_len(token: &str) -> Option<usize> {
    let eq = token.find('=')?;
    let mut chars = token[..eq].chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(eq)
}

/// Cross-sweep memory of which foreign-uid Claude processes' unreadable
/// environments have already produced a warning (PRO-211 second-round
/// review finding 2), so a given pid warns once while it stays in that
/// state, not on every sweep it remains alive. This is the exact same
/// problem, and the exact same fix, as `sweep::OrphanWarnings` - see its doc
/// comment - kept as a separate type since the two track unrelated pid sets
/// (orphaned-live-process vs. foreign-uid-unreadable-environment) that
/// happen to need identical warn-once treatment.
///
/// The caller (`main.rs`) owns one instance for the life of the process,
/// threading `&mut` through every [`discover`] / [`discover_process_snapshot`]
/// call, exactly like `OrphanWarnings`.
#[derive(Debug, Default)]
pub struct ForeignUidWarnings {
    warned: HashSet<i32>,
}

impl ForeignUidWarnings {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Pure decision, unit-tested directly: given `current` (this sweep's full
/// set of confirmed-Claude, foreign-uid, unreadable-environment pids),
/// return which of them must actually log a warning this call, and update
/// `warned` accordingly. Mirrors `sweep::new_orphan_pids`'s `retain`-then-
/// `insert` idiom exactly: a pid already warned about is forgotten the
/// moment it stops appearing in `current` (its process exited, or its
/// environment became readable), so a later, unrelated process reusing the
/// same pid - or the same process becoming readable and then unreadable
/// again - can still warn again rather than staying suppressed forever.
fn new_foreign_uid_pids(current: &HashSet<i32>, warned: &mut ForeignUidWarnings) -> Vec<i32> {
    warned.warned.retain(|pid| current.contains(pid));
    let mut new: Vec<i32> = current
        .iter()
        .copied()
        .filter(|pid| warned.warned.insert(*pid))
        .collect();
    new.sort_unstable();
    new
}

/// The watcher's own uid, used to distinguish "this Claude process belongs
/// to another user and its environment is genuinely unreadable to us" from
/// "we should have been able to read this and could not" (PRO-211
/// second-round review finding 2).
///
/// **Known limitation (PRO-211 third-round review finding 3):** this uid
/// comparison is a proxy for "unreadable by design", and that proxy is
/// false for a watcher running as root. A system-launchd/systemd deployment
/// (or anything else started as root) can read *every* user's environment,
/// so a genuine read failure on a uid-501 Claude process's environment
/// would, under this check, be misclassified as "foreign uid, expected to
/// be unreadable" and warn-and-skip (see [`build_claude_processes`] and the
/// Linux `imp::enumerate_claude_processes`) rather than fail discovery
/// loudly as PRO-211 otherwise intends for a same-uid read failure.
/// Reachability is low - root can normally read any process's environment
/// on both platforms this crate supports, so the read itself essentially
/// never fails for a root watcher in the first place - but the assumption
/// this check rests on ("a uid mismatch means we were never going to be
/// able to read it") is not universally true, so it is written down here
/// explicitly rather than left implicit.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn current_uid() -> u32 {
    // SAFETY: `getuid()` takes no arguments, dereferences no pointers, and
    // cannot fail - it is unconditionally safe to call.
    unsafe { libc::getuid() }
}

/// Filter `parsed` (every line `ps -Eww` produced, unfiltered) down to
/// Claude processes, and turn each into a [`ClaudeProcess`] - failing loudly
/// (PRO-211 review finding 4) rather than silently resolving to the default
/// config directory when a confirmed Claude process's environment could not
/// actually be read, *except* when that process belongs to another user
/// (PRO-211 second-round review finding 2 - see [`ForeignUidWarnings`] and
/// `DiscoveryError`'s doc comment for why uid matters), in which case it is
/// warned about once and skipped rather than failing discovery.
///
/// `ps -Eww` gives no way to distinguish "this process genuinely has an
/// empty environment" (not really possible for a real process - even a
/// minimal one inherits `PATH` at least) from "`ps` could not read this
/// process's environment at all and silently printed nothing for it"
/// (observed for processes owned by another user, where `ps` degrades
/// rather than erroring). An empty `env` map for a line `is_claude_exe`
/// already matched is therefore never simply accepted as "no environment":
/// for a *same-uid* process (one this watcher should have been able to
/// read), it becomes [`DiscoveryError::UnreadableEnvironment`] - unlike a
/// *non*-Claude process with an empty environment (see
/// `parse_ps_output_handles_a_process_with_no_environment_at_all`'s pid-1
/// case, filtered out before this function ever sees it), a Claude
/// process's `CLAUDE_CONFIG_DIR` is truth this sweep needs, so "cannot
/// determine it" must not silently fall through to [`default_config_dir_for`]
/// via a `None` `config_dir`. For a *foreign-uid* process, it is instead
/// treated as a legitimate, expected gap - `ps` cannot see into another
/// user's environment at all, ever, regardless of anything this watcher
/// does - so it is warned about (once - see [`new_foreign_uid_pids`]) and
/// excluded from the result, rather than failing the whole sweep for as
/// long as that unrelated process happens to be alive.
///
/// A present-but-*narrow* environment (no `CLAUDE_CONFIG_DIR` or
/// `TMUX_PANE` key, but other keys still read - see
/// `discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_and_tmux_pane_from_one_read`'s
/// pid-23195 case) is unaffected by any of this: that is a real, successful
/// observation (the process simply never set those variables), not either
/// error/skip path above.
#[cfg(any(target_os = "macos", test))]
fn build_claude_processes(
    parsed: Vec<(i32, u32, String, HashMap<String, String>)>,
    current_uid: u32,
    foreign_warnings: &mut ForeignUidWarnings,
) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
    let mut result = Vec::new();
    let mut foreign = HashSet::new();
    for (pid, uid, exe, env) in parsed {
        if !is_claude_exe(&exe) {
            continue;
        }
        if env.is_empty() {
            if uid != current_uid {
                foreign.insert(pid);
                continue;
            }
            return Err(DiscoveryError::UnreadableEnvironment {
                pid,
                source: std::io::Error::other(
                    "ps -Eww reported no environment at all for this process, and it is owned \
                     by the same user this watcher runs as, so ps ought to have been able to \
                     read it - this is a genuine read failure, not another user's process ps \
                     cannot see into",
                ),
            });
        }
        result.push(ClaudeProcess {
            pid,
            config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
            tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
            home: env.get(HOME_VAR).cloned(),
        });
    }
    for pid in new_foreign_uid_pids(&foreign, foreign_warnings) {
        tracing::warn!(
            pid,
            "matched a Claude process owned by another user; ps could not read its \
             environment, so its CLAUDE_CONFIG_DIR (if any) cannot be determined - skipping \
             this process rather than treating it as a discovery failure"
        );
    }
    Ok(result)
}

/// Parse a `/proc/<pid>/environ` blob: NUL-separated `KEY=VALUE` entries.
/// Unlike `ps -E`, this is unambiguous - NUL cannot appear inside a real
/// environment variable's key or value, so no reconstruction heuristic is
/// needed.
///
/// Compiled on Linux (the real caller) or under test (so this runs on
/// whatever platform CI happens to be, including macOS, per PRO-208's
/// testing decisions - the impure `/proc` walk in `imp::
/// enumerate_claude_processes` below cannot be exercised outside real
/// Linux, but this pure parser can and must be).
#[cfg(any(target_os = "linux", test))]
fn parse_environ_blob(raw: &[u8]) -> HashMap<String, String> {
    raw.split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let s = String::from_utf8_lossy(entry);
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::time::Duration;

    /// Upper bound on the `ps -Eww -ax -o pid=,command=` invocation.
    ///
    /// This is heavier than `git::DEFAULT_COMMAND_TIMEOUT` or
    /// `tmux::LIST_PANES_TIMEOUT`: it dumps the full command line *and*
    /// environment of every process on the host, not one repository's
    /// worth of plumbing output. Measured directly on this (otherwise
    /// idle) machine: ~114ms at 876 processes, ~370ms at 1874 (after
    /// spawning ~1000 extra `sleep` processes), ~615ms at 2868 (after
    /// ~2000 extra) - a roughly linear ~0.2ms/process. Extrapolating that
    /// slope, a genuinely busy host would need tens of thousands of
    /// processes to approach this bound, which 5 seconds comfortably
    /// outlives with real headroom to spare, while still being far short
    /// of "unbounded": before this timeout existed, a hung `ps` wedged
    /// `csm-watcher --once` past 15 seconds (still running when force-
    /// terminated) rather than degrading.
    ///
    /// Routed through `crate::command::run`, the same bounded runner
    /// `git` and `tmux` use, for the same reason: under PRO-210's polling
    /// loop, a bare `Command::output()` with no timeout - the shape this
    /// module used before this fix - wedges the daemon permanently the
    /// first time `ps` itself hangs, on both the discovery path (`ps`
    /// timing out becomes a discovery failure, publishing nothing - see
    /// `DiscoveryError`) and the `CSM_WATCHER_REGISTRY_DIRS` override path
    /// (`discover_process_snapshot` degrades the same timeout to an empty
    /// snapshot instead, since pane/live-pid capture there is enrichment,
    /// not truth about which sessions exist).
    const PS_TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) fn enumerate_claude_processes(
        foreign_warnings: &mut ForeignUidWarnings,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        let stdout = run_ps("ps")?;
        let parsed = parse_ps_output(&stdout)?;
        // Floor: the *unfiltered* process list - before `is_claude_exe`
        // narrows it - must never be empty on a live host (there is always
        // at least this watcher's own `ps` child and the OS's init
        // process). A successful-but-empty `ps` run is indistinguishable
        // from a broken one (a stub or shadowed `ps` on `PATH` that exits 0
        // printing nothing), so treat it as a failed enumeration rather
        // than "zero Claude processes found" - see `DiscoveryError`'s doc
        // comment for the reproduction.
        if parsed.is_empty() {
            return Err(DiscoveryError::EmptyProcessList);
        }
        build_claude_processes(parsed, current_uid(), foreign_warnings)
    }

    /// Run `<program> -Eww -ax -o pid=,uid=,command=`, bounded by
    /// [`PS_TIMEOUT`], and return its stdout. `-E` includes each process's
    /// environment; `-ww` disables output truncation (the default width
    /// would cut off exactly the tail end - the environment - that this
    /// module needs); `-ax` lists every process, not just ones attached to
    /// a terminal; `uid=` (PRO-211 second-round review finding 2) is what
    /// lets [`build_claude_processes`] tell a foreign user's process apart
    /// from a genuine read failure of this watcher's own user's process.
    ///
    /// A missing binary, a non-zero exit, empty output, or exceeding
    /// [`PS_TIMEOUT`] are all indistinguishable failures of enumeration
    /// itself from this function's caller's point of view, so all four
    /// collapse to the same `DiscoveryError::Enumerate`. That is
    /// deliberately *not* the same thing as `EmptyProcessList` above: this
    /// path fires when `ps` itself could not be run to completion at all,
    /// while `EmptyProcessList` fires when it ran and returned parseable-
    /// but-empty output. Both are still `DiscoveryError`, so both
    /// propagate identically through `discover()` (refuse to publish) and
    /// both degrade identically through `discover_process_snapshot()`
    /// (empty snapshot) - the distinction exists only for the log message.
    ///
    /// `program` is parameterised only so a test can point this at a
    /// binary that does not exist and exercise the "enumeration failed
    /// outright" error path, without mutating process-global state like
    /// `PATH`.
    fn run_ps(program: &str) -> Result<String, DiscoveryError> {
        crate::command::run(
            program,
            &["-Eww", "-ax", "-o", "pid=,uid=,command="],
            None,
            PS_TIMEOUT,
        )
        .ok_or_else(|| {
            DiscoveryError::Enumerate(std::io::Error::other(format!(
                "{program} -Eww -ax -o pid=,uid=,command= failed, produced no output, or \
                 exceeded {PS_TIMEOUT:?}"
            )))
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn run_ps_surfaces_a_missing_binary_as_a_discovery_error() {
            // Also stands in for the timeout path: `run_ps` maps whatever
            // reason `command::run` returns `None` for (missing binary
            // here, a timeout in production) to the same
            // `DiscoveryError::Enumerate` - a real enumeration failure that
            // `discover()` propagates and refuses to publish over, never an
            // empty `Ok` indistinguishable from a genuinely idle host.
            // `EmptyProcessList` is a distinct, narrower case (`ps` ran and
            // returned parseable-but-empty output), asserted separately by
            // the `union_discovery` tests below.
            let err = run_ps("definitely-not-a-real-ps-binary-xyz").unwrap_err();
            assert!(matches!(err, DiscoveryError::Enumerate(_)));
        }

        #[test]
        fn ps_timeout_bounds_a_hung_invocation_well_under_its_own_duration() {
            // Reproduces finding 1 from the second PRO-209 review round
            // directly: before this fix, `enumerate_claude_processes` shelled
            // out to `ps` via a bare `Command::output()` with no timeout at
            // all, so a hung `ps` on `PATH` left `csm-watcher --once` still
            // running - having published nothing - past 15 seconds before it
            // had to be force-terminated. This proves `PS_TIMEOUT` actually
            // bounds a hung invocation, exercised through `command::run`
            // directly (mirroring `tmux::run_list_panes`'s identical timeout
            // test) since `run_ps` itself always invokes the real `ps`
            // binary name and cannot be pointed at a hanging script without
            // mutating process-global `PATH`.
            let start = std::time::Instant::now();
            let result = crate::command::run("sh", &["-c", "sleep 30"], None, PS_TIMEOUT);
            assert_eq!(result, None);
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "must not block anywhere near the hung command's own sleep duration, took {:?}",
                start.elapsed()
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    pub(super) fn enumerate_claude_processes(
        foreign_warnings: &mut ForeignUidWarnings,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        let read_dir = fs::read_dir("/proc").map_err(DiscoveryError::Enumerate)?;
        let mut result = Vec::new();
        let mut foreign = HashSet::new();
        let this_uid = current_uid();
        // Unfiltered count of pid directories actually seen under `/proc`,
        // independent of whether any of them turn out to be Claude
        // processes. A live host always has at least this process itself,
        // so zero here can only mean `/proc` was not really read (e.g. it
        // was bind-mounted empty, or listing raced a container/namespace
        // boundary) - the Linux analogue of the macOS floor below: see
        // `DiscoveryError::EmptyProcessList`'s doc comment.
        let mut pid_dir_count = 0usize;
        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                // Not a pid directory (e.g. `/proc/self`, `/proc/net`).
                continue;
            };
            pid_dir_count += 1;
            // `cmdline` gives argv, NUL-separated; argv[0] is how the
            // process was invoked, the same thing the macOS path reads
            // from `ps`'s command column. A read failure here almost
            // always means the process has since exited (a race against
            // enumeration, not a real error) or belongs to another user;
            // either way it is not a Claude process we can identify, so it
            // is skipped rather than failing the whole enumeration.
            let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
                continue;
            };
            let Some(argv0) = cmdline.split(|&b| b == 0).next().filter(|s| !s.is_empty()) else {
                continue;
            };
            if !is_claude_exe(&String::from_utf8_lossy(argv0)) {
                continue;
            }
            // From here the process is confirmed to be a Claude binary, so
            // its `CLAUDE_CONFIG_DIR` (if any) is truth this sweep needs,
            // not enrichment - unlike `cmdline` above, a failure here must
            // not be swallowed. Before this fix (PRO-211 review finding 4)
            // this branch logged a warning and skipped the process, which
            // silently dropped that profile's registry directory from
            // `union_discovery`'s input and ended its sessions with a
            // successful exit. Propagating `DiscoveryError::
            // UnreadableEnvironment` instead makes `discover()` refuse to
            // publish, matching the "fail loudly" contract this ticket
            // exists to make structural. This is not the same leniency as
            // `registry::read_entries`'s per-file skip-and-warn: a
            // malformed *registry* file is Claude Code's own format, which
            // this project does not control and cannot assume is always
            // well-formed, whereas a Claude process's environment being
            // unreadable here means this sweep's very knowledge of which
            // directories exist is incomplete - the same "cannot determine"
            // class as an unreadable registry directory itself.
            //
            // Exactly one exception (PRO-211 second-round review finding 2,
            // applied here for the same reason it was applied to the macOS
            // `ps` path even though the finding's own reproduction was
            // macOS-specific): a process owned by another uid is never
            // readable to this watcher, by design of the OS - `/proc/<pid>/
            // environ` is `0400`, owned by that process's own user - so an
            // `EACCES` here for a foreign-uid process is not "we should
            // have been able to read this and could not", it is "we were
            // never going to be able to read this, and that is fine
            // provided we say so". Failing discovery outright over an
            // unrelated `sudo claude` or a shared host's other users would
            // otherwise be a permanent, self-inflicted outage exactly like
            // the macOS case - see `DiscoveryError`'s doc comment.
            let raw = match fs::read(format!("/proc/{pid}/environ")) {
                Ok(raw) => raw,
                Err(source) => {
                    let owner_uid = fs::metadata(format!("/proc/{pid}")).ok().map(|m| m.uid());
                    if owner_uid.is_some_and(|uid| uid != this_uid) {
                        foreign.insert(pid);
                        continue;
                    }
                    return Err(DiscoveryError::UnreadableEnvironment { pid, source });
                }
            };
            let env = parse_environ_blob(&raw);
            result.push(ClaudeProcess {
                pid,
                config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
                tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
                home: env.get(HOME_VAR).cloned(),
            });
        }
        if pid_dir_count == 0 {
            return Err(DiscoveryError::EmptyProcessList);
        }
        for pid in new_foreign_uid_pids(&foreign, foreign_warnings) {
            tracing::warn!(
                pid,
                "matched a Claude process owned by another user; /proc/<pid>/environ could \
                 not be read, so its CLAUDE_CONFIG_DIR (if any) cannot be determined - \
                 skipping this process rather than treating it as a discovery failure"
            );
        }
        Ok(result)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub(super) fn enumerate_claude_processes(
        _foreign_warnings: &mut ForeignUidWarnings,
    ) -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        Err(DiscoveryError::Enumerate(std::io::Error::other(
            "process discovery is not implemented on this platform",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // --- is_claude_exe ---

    #[test]
    fn is_claude_exe_matches_bare_name_full_path_and_exe_suffix_case_insensitively() {
        assert!(is_claude_exe("claude"));
        assert!(is_claude_exe(
            "/opt/homebrew/Caskroom/claude-code/2.1.206/claude"
        ));
        assert!(is_claude_exe(
            "/Users/x/.local/share/mise/installs/node/24/bin/claude.exe"
        ));
        assert!(is_claude_exe("Claude"));
    }

    #[test]
    fn is_claude_exe_rejects_similarly_named_tools() {
        assert!(!is_claude_exe("claude-code"));
        assert!(!is_claude_exe("claudex"));
        assert!(!is_claude_exe("/usr/bin/vim"));
        assert!(!is_claude_exe(""));
    }

    // --- macOS `ps -Eww` parsing (pure - fixture bytes, no subprocess) ---

    /// The uid every fixture "own-user" process below is owned by, standing
    /// in for the watcher's own `current_uid()` in tests - see
    /// [`ForeignUidWarnings`]-related tests below for the contrasting
    /// foreign-uid case (pid 99999, owned by `0`/root).
    const TEST_UID: u32 = 501;

    /// A representative capture of `ps -Eww -ax -o pid=,uid=,command=`
    /// output: a right-padded pid column, two Claude processes under
    /// different `CLAUDE_CONFIG_DIR`s (two profiles), a full-path Claude
    /// install, a `.exe`-suffixed Claude process, two non-Claude processes
    /// (one with no environment at all), a `PATH` value containing a
    /// literal space to prove reconstruction survives it without
    /// corrupting a neighbouring key, and a *foreign-uid* Claude process
    /// (pid 99999, uid 0) with no environment at all - reproducing,
    /// directly on this project's own host, the reviewer's `1 /sbin/launchd`
    /// and `653 /usr/libexec/corebrightnessd --launchd` observation that
    /// `ps -Eww` prints a full command line for another user's process
    /// while silently suppressing its environment, rather than erroring
    /// (PRO-211 second-round review finding 2).
    const SAMPLE_PS_OUTPUT: &str = "\
65682   501 claude --model claude-opus-5 SSH_TTY=/dev/ttys017 CLAUDE_CONFIG_DIR=/opt/profile-a/.claude TMUX_PANE=%38 HOME=/opt/profile-a
 1760   501 claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work TMUX_PANE=%2 HOME=/opt/profile-b
23195   501 /opt/homebrew/Caskroom/claude-code/2.1.206/claude HOME=/opt/profile-a
  131   501 /Applications/Ghostty.app/Contents/MacOS/ghostty OSLogRateLimit=64 USER=simon HOME=/opt/profile-a
    1     0 /sbin/launchd
70773   501 /Users/x/.local/share/mise/installs/node/24/bin/claude.exe CLAUDE_CONFIG_DIR=/opt/profile-a/.claude TMUX_PANE=%5 HOME=/opt/profile-a
95255   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude PATH=/opt/my dir/bin:/usr/bin TMUX_PANE=%7 HOME=/opt/profile-a
99999     0 claude
";

    #[test]
    fn parse_ps_output_extracts_pid_uid_exe_and_full_env_per_line() {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT).unwrap();
        assert_eq!(parsed.len(), 8, "every line, Claude or not, is parsed");

        let (pid, uid, exe, env) = parsed.iter().find(|(pid, ..)| *pid == 65682).unwrap();
        assert_eq!(*pid, 65682);
        assert_eq!(*uid, 501);
        assert_eq!(exe, "claude");
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(env.get(TMUX_PANE_VAR).map(String::as_str), Some("%38"));
    }

    #[test]
    fn parse_ps_output_reconstructs_a_value_containing_a_literal_space_without_corrupting_neighbours()
     {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT).unwrap();
        let (_, _, _, env) = parsed.iter().find(|(pid, ..)| *pid == 95255).unwrap();
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/opt/my dir/bin:/usr/bin"),
            "the space inside the PATH value must be preserved, not treated as a new key"
        );
        // The keys either side of the space-containing PATH value must
        // still be extracted correctly.
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(env.get(TMUX_PANE_VAR).map(String::as_str), Some("%7"));
    }

    #[test]
    fn parse_ps_output_handles_a_process_with_no_environment_at_all() {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT).unwrap();
        let (_, uid, exe, env) = parsed.iter().find(|(pid, ..)| *pid == 1).unwrap();
        assert_eq!(exe, "/sbin/launchd");
        assert_eq!(
            *uid, 0,
            "launchd is root-owned - part of why this case must not be an error"
        );
        assert!(env.is_empty());
    }

    // --- PRO-211 third-round review finding 2: a malformed ps line must
    // fail discovery, not be silently dropped ---

    #[test]
    fn parse_ps_output_fails_on_a_line_with_an_unparseable_uid_column_instead_of_dropping_it() {
        // Reproduces the reviewer's demonstration directly: two live
        // profiles, one line's uid column corrupted. Before this fix,
        // `parse_ps_output`'s `filter_map` silently dropped the malformed
        // line, `build_claude_processes` never saw it at all, and
        // discovery succeeded with only the other profile - a
        // silently-incomplete process list ending every session the
        // dropped profile had, with a successful exit.
        let two_profiles_one_corrupt = "\
65682   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude
 1760   notauid claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work
";
        let err = parse_ps_output(two_profiles_one_corrupt).unwrap_err();
        assert!(
            matches!(err, DiscoveryError::MalformedPsLine { .. }),
            "expected MalformedPsLine, got {err:?}"
        );
    }

    #[test]
    fn parse_ps_output_fails_on_a_line_missing_the_uid_column_entirely() {
        // A pid column that parses fine but nothing at all after it (no uid,
        // no command) is not "a process with an empty command" - see
        // `parse_ps_line`'s doc comment - it is missing a column `ps`
        // guarantees is always present, so it must fail rather than be
        // silently skipped.
        let err = parse_ps_output("12345\n").unwrap_err();
        assert!(matches!(err, DiscoveryError::MalformedPsLine { .. }));
    }

    #[test]
    fn parse_ps_output_skips_a_blank_line_without_failing() {
        // A blank line carries no process entry at all - not a violation of
        // the expected shape, just nothing to parse - so it must not fail
        // discovery over it.
        let with_blank_line = "\
65682   501 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude

 1760   501 claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work
";
        let parsed = parse_ps_output(with_blank_line).unwrap();
        assert_eq!(
            parsed.iter().map(|(pid, ..)| *pid).collect::<Vec<_>>(),
            vec![65682, 1760],
            "both real lines must still be parsed around the blank one"
        );
    }

    #[test]
    fn parse_ps_output_skips_a_process_with_an_empty_command_column_without_failing() {
        // pid and uid both parse; `ps` simply printed nothing in the
        // `command=` column for this process - a real, if rare, shape (see
        // `parse_ps_line`'s doc comment), not a sign the line is malformed.
        // Such a line can never be `is_claude_exe` either way, so it is
        // dropped rather than failing discovery over it.
        let empty_command = "55555   501   \n";
        let parsed = parse_ps_output(empty_command).unwrap();
        assert!(
            parsed.is_empty(),
            "a process with an empty command column must be silently dropped, not surfaced as \
             a discovery failure"
        );
    }

    #[test]
    fn parse_env_tokens_first_wins_when_a_later_value_impersonates_a_key_boundary() {
        // Reproduces the exact wipe the reviewer found against real
        // `ps -Eww` output: a two-profile bash helper exported as a
        // function, `BASH_FUNC_cw%%=() { CLAUDE_CONFIG_DIR=/opt/personal
        // claude "$@" }` (PRO-204's user stories 11 and 12's two-profile
        // shape). `BASH_FUNC_cw%%=` itself is not a valid `IDENT=` (the
        // `%%` characters fail `env_assignment_key_len`), so it folds into
        // the real `CLAUDE_CONFIG_DIR`'s value - but the
        // `CLAUDE_CONFIG_DIR=/opt/personal` token *inside* that function
        // body still looks exactly like a fresh, valid key boundary, and is
        // read as one.
        //
        // Before this fix, `parse_env_tokens` used `insert` (last-wins), so
        // this spurious second boundary silently overwrote the real
        // `CLAUDE_CONFIG_DIR`, and the real profile was never swept.
        let tokens = [
            "HOME=/opt/real-profile",
            "CLAUDE_CONFIG_DIR=/opt/real-profile/.claude",
            // A real, unrelated env var sits between the genuine
            // assignment and the exported function - the ordinary shape of
            // a real `ps -Eww` environment block - so the fold from the
            // non-key `BASH_FUNC_cw%%=()` token below lands on *this*
            // pair's value, not on the already-flushed, already-correct
            // `CLAUDE_CONFIG_DIR` entry.
            "PATH=/usr/bin:/bin",
            "BASH_FUNC_cw%%=()",
            "{",
            "CLAUDE_CONFIG_DIR=/opt/personal",
            "claude",
            "\"$@\"",
            "}",
        ];
        let env = parse_env_tokens(tokens.into_iter());
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/opt/real-profile/.claude"),
            "the first, real CLAUDE_CONFIG_DIR= boundary must win over a later one embedded \
             inside another variable's value"
        );
    }

    #[test]
    fn discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_tmux_pane_and_home_from_one_read()
     {
        let claude_processes = build_claude_processes(
            parse_ps_output(SAMPLE_PS_OUTPUT).unwrap(),
            TEST_UID,
            &mut ForeignUidWarnings::new(),
        )
        .unwrap();

        // Non-Claude processes (ghostty, launchd) are excluded; so is the
        // foreign-uid Claude process (pid 99999, see the tests below) - the
        // full-path install, the bare name, and the `.exe`-suffixed
        // *same-uid* Claude processes are all included.
        let pids: Vec<i32> = claude_processes.iter().map(|p| p.pid).collect();
        assert_eq!(
            pids,
            vec![65682, 1760, 23195, 70773, 95255],
            "exactly the same-uid Claude-binary lines survive filtering, in source order"
        );

        let pid_65682 = claude_processes.iter().find(|p| p.pid == 65682).unwrap();
        assert_eq!(
            pid_65682.config_dir.as_deref(),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(pid_65682.tmux_pane.as_deref(), Some("%38"));
        assert_eq!(pid_65682.home.as_deref(), Some("/opt/profile-a"));

        // pid 23195 has no CLAUDE_CONFIG_DIR or TMUX_PANE in its
        // environment at all - both come back None, not an empty string -
        // but it does have HOME (PRO-211 second-round review finding 3).
        let pid_23195 = claude_processes.iter().find(|p| p.pid == 23195).unwrap();
        assert_eq!(pid_23195.config_dir, None);
        assert_eq!(pid_23195.tmux_pane, None);
        assert_eq!(pid_23195.home.as_deref(), Some("/opt/profile-a"));
    }

    #[test]
    fn a_same_uid_claude_matched_process_with_no_environment_at_all_is_a_discovery_error_not_a_silent_default()
     {
        // PRO-211 review finding 4, narrowed by the second-round review's
        // finding 2: `ps -Eww` reporting zero environment tokens for a line
        // already confirmed to be a Claude process (by its invoked name)
        // *and* owned by this watcher's own uid means `ps` could not read a
        // process's environment that it ought to have been able to - not
        // that the process genuinely has no environment, and not a foreign
        // user's process it was never going to be able to read (see the
        // foreign-uid test below). Before finding 4's fix,
        // `env.get(CLAUDE_CONFIG_DIR_VAR)` on the empty map silently
        // returned `None`, which `resolve_process_config_dir` then resolved
        // to the *default* config directory - sweeping the wrong directory
        // instead of refusing to publish, and ending that profile's real
        // sessions with a successful exit.
        let claude_line_no_env = "\
55555   501 claude
";
        let parsed = parse_ps_output(claude_line_no_env).unwrap();
        assert_eq!(parsed.len(), 1, "sanity: the line parses at all");
        let err =
            build_claude_processes(parsed, TEST_UID, &mut ForeignUidWarnings::new()).unwrap_err();
        assert!(
            matches!(
                err,
                DiscoveryError::UnreadableEnvironment { pid: 55555, .. }
            ),
            "expected UnreadableEnvironment for pid 55555, got {err:?}"
        );
    }

    #[test]
    fn a_non_claude_process_with_no_environment_at_all_is_not_an_error() {
        // The counterpart to the test above: pid 1 (`/sbin/launchd`) in
        // `SAMPLE_PS_OUTPUT` also has zero environment tokens, but it is
        // filtered out by `is_claude_exe` before `build_claude_processes`'s
        // empty-env check ever runs, since a non-Claude process's
        // environment is never truth this project needs.
        let claude_processes = build_claude_processes(
            parse_ps_output(SAMPLE_PS_OUTPUT).unwrap(),
            TEST_UID,
            &mut ForeignUidWarnings::new(),
        )
        .unwrap();
        assert!(
            claude_processes.iter().all(|p| p.pid != 1),
            "pid 1 (/sbin/launchd) must be filtered out, not surfaced as an error"
        );
    }

    // --- PRO-211 second-round review finding 2: foreign-uid Claude processes ---

    #[test]
    fn a_foreign_uid_claude_matched_process_with_no_environment_is_skipped_not_a_discovery_error() {
        // Pid 99999 in SAMPLE_PS_OUTPUT: a Claude-named process owned by
        // uid 0 (root) - standing in for `sudo claude`, or any Claude
        // process on a shared host owned by a different user - with zero
        // environment tokens, the same shape `ps -Eww` reports for a
        // genuine same-uid read failure. Before this fix, this was
        // indistinguishable from that failure and turned into
        // `DiscoveryError::UnreadableEnvironment`, which - since this
        // process's environment can *never* be read by this watcher,
        // regardless of retries - became a permanent discovery failure for
        // as long as the foreign process stayed alive: every sweep,
        // `discover()` returned `Err`, and the watcher published nothing at
        // all.
        let result = build_claude_processes(
            parse_ps_output(SAMPLE_PS_OUTPUT).unwrap(),
            TEST_UID,
            &mut ForeignUidWarnings::new(),
        );
        let claude_processes = result.expect(
            "a foreign-uid Claude process with an unreadable environment must not fail \
             discovery outright",
        );
        assert!(
            claude_processes.iter().all(|p| p.pid != 99999),
            "the foreign-uid process must be excluded from the result, not silently defaulted"
        );
    }

    #[test]
    fn new_foreign_uid_pids_warns_once_then_forgets_a_pid_that_stops_appearing() {
        // Mirrors `sweep::new_orphan_pids`'s warn-once/forget behaviour
        // exactly - see its doc comment - proven directly here since this
        // pure decision function is what `build_claude_processes` and the
        // Linux enumerator both delegate to for the actual warn-once
        // bookkeeping.
        let mut warnings = ForeignUidWarnings::new();

        let first = new_foreign_uid_pids(&HashSet::from([100]), &mut warnings);
        assert_eq!(first, vec![100], "must warn the first time a pid is seen");

        let second = new_foreign_uid_pids(&HashSet::from([100]), &mut warnings);
        assert!(
            second.is_empty(),
            "must not warn again for the same pid while it stays foreign every call"
        );

        let third = new_foreign_uid_pids(&HashSet::new(), &mut warnings);
        assert!(third.is_empty(), "an empty current set warns about nothing");

        let fourth = new_foreign_uid_pids(&HashSet::from([100]), &mut warnings);
        assert_eq!(
            fourth,
            vec![100],
            "a pid forgotten because it stopped appearing (process exited, or its environment \
             became readable) must be able to warn again if it reappears, not stay suppressed \
             forever"
        );
    }

    // --- Linux `/proc/<pid>/environ` parsing (pure - fixture bytes) ---
    //
    // The impure half of Linux discovery - walking `/proc` and reading
    // these files for real - cannot be exercised on macOS, where this
    // suite runs; see the module doc comment. This is the pure half, and
    // it runs on every platform this crate builds for, proving the parser
    // itself is correct independently of ever running on real Linux.

    #[test]
    fn parse_environ_blob_extracts_config_dir_and_tmux_pane_from_one_blob() {
        let raw =
            b"HOME=/home/alice\0CLAUDE_CONFIG_DIR=/home/alice/.claude-personal\0TMUX_PANE=%3\0PATH=/usr/bin:/bin\0";
        let env = parse_environ_blob(raw);
        assert_eq!(
            env.get(CLAUDE_CONFIG_DIR_VAR).map(String::as_str),
            Some("/home/alice/.claude-personal")
        );
        assert_eq!(env.get(TMUX_PANE_VAR).map(String::as_str), Some("%3"));
    }

    #[test]
    fn parse_environ_blob_splits_only_on_the_first_equals() {
        // A value that itself contains `=` (plausible for e.g. a base64 or
        // query-string-shaped value) must not be truncated - NUL, not `=`,
        // is the field separator here.
        let raw = b"SOME_TOKEN=abc=def=ghi\0";
        let env = parse_environ_blob(raw);
        assert_eq!(
            env.get("SOME_TOKEN").map(String::as_str),
            Some("abc=def=ghi")
        );
    }

    #[test]
    fn parse_environ_blob_ignores_empty_entries() {
        // A trailing NUL (the common real-world shape of this file) must
        // not produce a bogus empty entry.
        let raw = b"HOME=/home/alice\0\0";
        let env = parse_environ_blob(raw);
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn parse_environ_blob_empty_input_yields_empty_map() {
        assert!(parse_environ_blob(b"").is_empty());
    }

    // --- union_discovery ---

    fn cp(
        pid: i32,
        config_dir: Option<&str>,
        tmux_pane: Option<&str>,
        home: Option<&str>,
    ) -> ClaudeProcess {
        ClaudeProcess {
            pid,
            config_dir: config_dir.map(str::to_string),
            tmux_pane: tmux_pane.map(str::to_string),
            home: home.map(str::to_string),
        }
    }

    #[test]
    fn union_discovery_two_profiles_under_different_config_dirs_both_appear() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), None, None),
            cp(200, Some("/opt/profile-b/.claude-work"), None, None),
        ]);
        assert_eq!(
            discovery.registry_dirs,
            vec![
                // The default config directory is always seeded first, as
                // a floor against a total `is_claude_exe` miss - see
                // `union_discovery`'s doc comment - even though every
                // process here explicitly named its own directory.
                default_config_dir(),
                PathBuf::from("/opt/profile-a/.claude"),
                PathBuf::from("/opt/profile-b/.claude-work"),
            ]
        );
    }

    #[test]
    fn union_discovery_deduplicates_the_same_config_dir_across_processes() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), None, None),
            cp(200, Some("/opt/profile-a/.claude"), None, None),
        ]);
        assert_eq!(
            discovery.registry_dirs,
            vec![
                default_config_dir(),
                PathBuf::from("/opt/profile-a/.claude")
            ]
        );
    }

    #[test]
    fn union_discovery_defaults_unset_or_blank_config_dir_to_home_claude() {
        let discovery =
            union_discovery(&[cp(100, None, None, None), cp(200, Some("   "), None, None)]);
        // Both processes and the unconditional seed all resolve to the same
        // default directory, so it still dedupes down to exactly one entry.
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_rejects_a_relative_config_dir_and_falls_back_to_default() {
        // A relative CLAUDE_CONFIG_DIR was read from the *Claude process's*
        // environment, not the watcher's; resolving it against the
        // watcher's own working directory would sweep the wrong directory
        // entirely, silently ending that profile's sessions - see
        // `resolve_process_config_dir`'s doc comment.
        let discovery = union_discovery(&[cp(100, Some("relative/config/dir"), None, None)]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_captures_tmux_panes_keyed_by_pid() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), Some("%1"), None),
            cp(200, Some("/opt/profile-a/.claude"), None, None),
            cp(300, Some("/opt/profile-b/.claude"), Some("%9"), None),
        ]);
        assert_eq!(
            discovery.tmux_panes.get(&100).map(String::as_str),
            Some("%1")
        );
        assert_eq!(discovery.tmux_panes.get(&200), None);
        assert_eq!(
            discovery.tmux_panes.get(&300).map(String::as_str),
            Some("%9")
        );
        assert_eq!(discovery.tmux_panes.len(), 2);
    }

    #[test]
    fn union_discovery_captures_live_pids_for_every_process_regardless_of_tmux_pane() {
        // PRO-211: `live_pids` is the set discovery hands to `sweep::sweep`
        // for the orphaned-live-process warning, so it must include every
        // process's pid - not only ones running inside tmux (pid 200 has no
        // tmux pane at all, unlike `union_discovery_captures_tmux_panes_keyed_by_pid`
        // above, which only asserts on `tmux_panes`).
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), Some("%1"), None),
            cp(200, Some("/opt/profile-a/.claude"), None, None),
        ]);
        assert_eq!(discovery.live_pids, HashSet::from([100, 200]));
    }

    #[test]
    fn union_discovery_of_no_processes_still_seeds_the_default_config_dir() {
        // This is the fix for finding 2: a total `is_claude_exe` miss -
        // every real process filtered out, e.g. because Claude Code
        // presented as `node <cli>` in the process list - must not degrade
        // to an empty `Discovery`. Before this fix, `union_discovery(&[])`
        // produced `Discovery::default()` (empty `registry_dirs`), which
        // `sweep` would publish as an empty snapshot and end every session
        // on the host.
        let discovery = union_discovery(&[]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
        assert!(discovery.tmux_panes.is_empty());
        assert!(discovery.live_pids.is_empty());
    }

    // --- PRO-211 second-round review finding 3: per-process HOME, not the watcher's ---

    #[test]
    fn union_discovery_resolves_a_process_with_no_claude_config_dir_to_its_own_home_not_the_watchers()
     {
        // Before this fix, a process with no CLAUDE_CONFIG_DIR resolved to
        // `default_config_dir()`, which reads the *watcher's own* `$HOME` -
        // not the Claude process's. A watcher whose `$HOME` differs from
        // the session owner's (e.g. run under a service account, or by
        // `sudo -u`, or simply a differently configured shell) swept the
        // wrong default profile entirely, silently ending the real one with
        // a successful exit even though that process's own `HOME` was
        // sitting right there in the environment already parsed for it.
        let discovery = union_discovery(&[cp(100, None, None, Some("/home/alice"))]);
        assert!(
            discovery
                .registry_dirs
                .contains(&PathBuf::from("/home/alice/.claude")),
            "must resolve the process's own HOME, not the watcher's, got {:?}",
            discovery.registry_dirs
        );
    }

    #[test]
    fn union_discovery_falls_back_to_the_watchers_home_when_a_process_has_no_home_at_all() {
        // A process whose environment could be read but simply had no HOME
        // entry (rare, but not impossible) must not crash or silently
        // resolve to some bogus path - falling back to the watcher's own
        // default is the least-wrong available answer, and is exactly what
        // happened unconditionally before this fix.
        let discovery = union_discovery(&[cp(100, None, None, None)]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_rejects_a_relative_process_home_and_falls_back_to_the_watchers_default() {
        // A relative HOME is nonsensical to resolve (relative to what? -
        // the watcher's cwd would be arbitrary and wrong) - this must fall
        // back to the watcher's own default rather than silently joining
        // ".claude" onto a relative path and resolving it against whatever
        // the watcher's current directory happens to be.
        let discovery = union_discovery(&[cp(100, None, None, Some("relative/home"))]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }
}
