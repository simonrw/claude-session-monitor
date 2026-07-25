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
//! [`sweep::registry_dirs_from_env`](crate::sweep::registry_dirs_from_env)
//! remains a real escape hatch: whenever it yields at least one directory,
//! the caller must use that list directly and never call [`discover`] at
//! all. Discovery itself is only invoked when that override is absent.
//!
//! The impure part - actually enumerating OS processes - is confined to
//! the per-platform `imp::enumerate_claude_processes` (macOS: `ps -Eww`;
//! Linux: `/proc/<pid>/{cmdline,environ}`). Everything else in this file is
//! a pure function of already-read bytes and is tested against fixture
//! data, including - per PRO-204's testing decisions - the Linux
//! `/proc/<pid>/environ` parser, even though the impure Linux enumeration
//! itself cannot be exercised from macOS.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The environment variable Claude Code reads to relocate its config
/// directory (and therefore its session registry, at
/// `<dir>/sessions/<pid>.json`).
const CLAUDE_CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";

/// The environment variable tmux sets in every process running inside a
/// pane, identifying that pane (e.g. `%38`).
const TMUX_PANE_VAR: &str = "TMUX_PANE";

/// What automatic discovery found: the deduplicated registry directories to
/// sweep, and each live Claude process's tmux pane keyed by pid.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Discovery {
    pub registry_dirs: Vec<PathBuf>,
    pub tmux_panes: HashMap<i32, String>,
}

/// Process enumeration failed outright: the `ps` invocation errored, or
/// `/proc` itself could not be read.
///
/// This is deliberately narrow. It must **not** cover "enumeration
/// succeeded and found zero *Claude* processes" - that is a genuinely
/// empty, genuinely successful result (see [`discover`]) - nor a single
/// process whose environment could not be read (logged and skipped by the
/// relevant `imp` module, the same leniency `registry::read_entries`
/// applies to a single unreadable registry file). Only a failure of
/// enumeration itself belongs here, because only that failure means the
/// true set of live Claude processes is unknown, and publishing an empty
/// snapshot in that case would be indistinguishable from "no sessions
/// exist".
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
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("failed to enumerate processes: {0}")]
    Enumerate(#[source] std::io::Error),
    #[error(
        "process enumeration returned zero processes total, which is impossible on a live \
         host; treating this as a broken enumerator rather than a genuine observation"
    )]
    EmptyProcessList,
}

/// One live process whose invoked executable looked like a Claude binary,
/// with the two environment values this project reads from it.
#[derive(Debug, Clone, PartialEq)]
struct ClaudeProcess {
    pid: i32,
    config_dir: Option<String>,
    tmux_pane: Option<String>,
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
pub fn discover() -> Result<Discovery, DiscoveryError> {
    let processes = imp::enumerate_claude_processes()?;
    Ok(union_discovery(&processes))
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
    for p in processes {
        let dir = resolve_process_config_dir(p.config_dir.as_deref());
        if !registry_dirs.contains(&dir) {
            registry_dirs.push(dir);
        }
        if let Some(pane) = &p.tmux_pane {
            tmux_panes.insert(p.pid, pane.clone());
        }
    }
    Discovery {
        registry_dirs,
        tmux_panes,
    }
}

/// Resolve one process's `CLAUDE_CONFIG_DIR` value into the directory to
/// sweep for it, falling back to [`default_config_dir`] when the value is
/// absent, blank, or not an absolute path.
///
/// A relative value is rejected rather than resolved against the watcher's
/// own working directory: the value came from the *Claude process's*
/// environment, and its cwd is not the watcher's, so resolving it against
/// the watcher's cwd would sweep an arbitrary, almost certainly wrong,
/// directory - silently ending that profile's sessions on every future
/// sweep - rather than the one Claude Code actually uses. Falling back to
/// the default config directory instead is not correct either (Claude
/// Code's own resolution of a relative `CLAUDE_CONFIG_DIR` is unspecified
/// here), but it fails toward "still tracked under some directory" rather
/// than toward "silently sweeping the wrong one".
fn resolve_process_config_dir(config_dir: Option<&str>) -> PathBuf {
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
                default_config_dir()
            }
        }
        _ => default_config_dir(),
    }
}

/// `~/.claude`, using `$HOME` the same way `main.rs` already does for the
/// log directory. Falling back to `/.claude` when `$HOME` is unset mirrors
/// that precedent rather than inventing a new one; a watcher running with
/// no `$HOME` at all has bigger problems than this fallback's exact value.
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

/// Parse `ps -Eww -ax -o pid=,command=` output: one process per line, `pid`
/// then the invoked command, its arguments, and its environment - all
/// whitespace-joined by `ps` with no quoting or escaping. Returns every
/// parseable line as `(pid, invoked_exe, env)`, unfiltered; callers narrow
/// to Claude processes with [`is_claude_exe`].
///
/// Compiled whenever this is the real parser in use (macOS) or whenever
/// tests are running (so the fixture tests below exercise it on every CI
/// platform, not only macOS) - never in a plain non-macOS, non-test build,
/// where it would otherwise be legitimately unused dead code.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_output(output: &str) -> Vec<(i32, String, HashMap<String, String>)> {
    output.lines().filter_map(parse_ps_line).collect()
}

#[cfg(any(target_os = "macos", test))]
fn parse_ps_line(line: &str) -> Option<(i32, String, HashMap<String, String>)> {
    let line = line.trim_start();
    let (pid_str, rest) = line.split_once(char::is_whitespace)?;
    let pid: i32 = pid_str.trim().parse().ok()?;
    let mut tokens = rest.trim_start().split(' ').filter(|t| !t.is_empty());
    let exe = tokens.next()?.to_string();
    let env = parse_env_tokens(tokens);
    Some((pid, exe, env))
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
    use std::process::Command;

    pub(super) fn enumerate_claude_processes() -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        let stdout = run_ps("ps")?;
        let parsed = parse_ps_output(&stdout);
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
        Ok(parsed
            .into_iter()
            .filter(|(_, exe, _)| is_claude_exe(exe))
            .map(|(pid, _, env)| ClaudeProcess {
                pid,
                config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
                tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
            })
            .collect())
    }

    /// Run `<program> -Eww -ax -o pid=,command=` and return its stdout.
    /// `-E` includes each process's environment; `-ww` disables output
    /// truncation (the default width would cut off exactly the tail end -
    /// the environment - that this module needs); `-ax` lists every
    /// process, not just ones attached to a terminal.
    ///
    /// `program` is parameterised only so a test can point this at a
    /// binary that does not exist and exercise the "enumeration failed
    /// outright" error path, without mutating process-global state like
    /// `PATH`.
    fn run_ps(program: &str) -> Result<String, DiscoveryError> {
        let output = Command::new(program)
            .args(["-Eww", "-ax", "-o", "pid=,command="])
            .output()
            .map_err(DiscoveryError::Enumerate)?;
        if !output.status.success() {
            return Err(DiscoveryError::Enumerate(std::io::Error::other(format!(
                "ps exited with {}",
                output.status
            ))));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn run_ps_surfaces_a_missing_binary_as_a_discovery_error() {
            let err = run_ps("definitely-not-a-real-ps-binary-xyz").unwrap_err();
            assert!(matches!(err, DiscoveryError::Enumerate(_)));
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;

    pub(super) fn enumerate_claude_processes() -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        let read_dir = fs::read_dir("/proc").map_err(DiscoveryError::Enumerate)?;
        let mut result = Vec::new();
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
            // a failure to read its environment is worth a warning (mirrors
            // `registry::read_entries`'s per-file leniency: one process's
            // unreadable environment - most likely EPERM, a Claude process
            // owned by another user - must not abort the whole sweep).
            match fs::read(format!("/proc/{pid}/environ")) {
                Ok(raw) => {
                    let env = parse_environ_blob(&raw);
                    result.push(ClaudeProcess {
                        pid,
                        config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
                        tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        pid,
                        error = %e,
                        "matched a Claude process but could not read its environment, skipping"
                    );
                }
            }
        }
        if pid_dir_count == 0 {
            return Err(DiscoveryError::EmptyProcessList);
        }
        Ok(result)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub(super) fn enumerate_claude_processes() -> Result<Vec<ClaudeProcess>, DiscoveryError> {
        Err(DiscoveryError::Enumerate(std::io::Error::other(
            "process discovery is not implemented on this platform",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A representative capture of `ps -Eww -ax -o pid=,command=` output:
    /// a right-padded pid column, two Claude processes under different
    /// `CLAUDE_CONFIG_DIR`s (two profiles), a full-path Claude install, a
    /// `.exe`-suffixed Claude process, two non-Claude processes (one with
    /// no environment at all), and a `PATH` value containing a literal
    /// space to prove reconstruction survives it without corrupting a
    /// neighbouring key.
    const SAMPLE_PS_OUTPUT: &str = "\
65682 claude --model claude-opus-5 SSH_TTY=/dev/ttys017 CLAUDE_CONFIG_DIR=/opt/profile-a/.claude TMUX_PANE=%38 HOME=/opt/profile-a
 1760 claude CLAUDE_CONFIG_DIR=/opt/profile-b/.claude-work TMUX_PANE=%2 HOME=/opt/profile-b
23195 /opt/homebrew/Caskroom/claude-code/2.1.206/claude HOME=/opt/profile-a
  131 /Applications/Ghostty.app/Contents/MacOS/ghostty OSLogRateLimit=64 USER=simon HOME=/opt/profile-a
    1 /sbin/launchd
70773 /Users/x/.local/share/mise/installs/node/24/bin/claude.exe CLAUDE_CONFIG_DIR=/opt/profile-a/.claude TMUX_PANE=%5 HOME=/opt/profile-a
95255 claude CLAUDE_CONFIG_DIR=/opt/profile-a/.claude PATH=/opt/my dir/bin:/usr/bin TMUX_PANE=%7 HOME=/opt/profile-a
";

    #[test]
    fn parse_ps_output_extracts_pid_exe_and_full_env_per_line() {
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT);
        assert_eq!(parsed.len(), 7, "every line, Claude or not, is parsed");

        let (pid, exe, env) = parsed.iter().find(|(pid, ..)| *pid == 65682).unwrap();
        assert_eq!(*pid, 65682);
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
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT);
        let (_, _, env) = parsed.iter().find(|(pid, ..)| *pid == 95255).unwrap();
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
        let parsed = parse_ps_output(SAMPLE_PS_OUTPUT);
        let (_, exe, env) = parsed.iter().find(|(pid, ..)| *pid == 1).unwrap();
        assert_eq!(exe, "/sbin/launchd");
        assert!(env.is_empty());
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
    fn discovery_pipeline_filters_to_claude_processes_and_captures_config_dir_and_tmux_pane_from_one_read()
     {
        let claude_processes: Vec<ClaudeProcess> = parse_ps_output(SAMPLE_PS_OUTPUT)
            .into_iter()
            .filter(|(_, exe, _)| is_claude_exe(exe))
            .map(|(pid, _, env)| ClaudeProcess {
                pid,
                config_dir: env.get(CLAUDE_CONFIG_DIR_VAR).cloned(),
                tmux_pane: env.get(TMUX_PANE_VAR).cloned(),
            })
            .collect();

        // Non-Claude processes (ghostty, launchd) are excluded; the
        // full-path install, the bare name, and the `.exe`-suffixed one are
        // all included.
        let pids: Vec<i32> = claude_processes.iter().map(|p| p.pid).collect();
        assert_eq!(
            pids,
            vec![65682, 1760, 23195, 70773, 95255],
            "exactly the Claude-binary lines survive filtering, in source order"
        );

        let pid_65682 = claude_processes.iter().find(|p| p.pid == 65682).unwrap();
        assert_eq!(
            pid_65682.config_dir.as_deref(),
            Some("/opt/profile-a/.claude")
        );
        assert_eq!(pid_65682.tmux_pane.as_deref(), Some("%38"));

        // pid 23195 has no CLAUDE_CONFIG_DIR or TMUX_PANE in its
        // environment at all - both come back None, not an empty string.
        let pid_23195 = claude_processes.iter().find(|p| p.pid == 23195).unwrap();
        assert_eq!(pid_23195.config_dir, None);
        assert_eq!(pid_23195.tmux_pane, None);
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

    fn cp(pid: i32, config_dir: Option<&str>, tmux_pane: Option<&str>) -> ClaudeProcess {
        ClaudeProcess {
            pid,
            config_dir: config_dir.map(str::to_string),
            tmux_pane: tmux_pane.map(str::to_string),
        }
    }

    #[test]
    fn union_discovery_two_profiles_under_different_config_dirs_both_appear() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), None),
            cp(200, Some("/opt/profile-b/.claude-work"), None),
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
            cp(100, Some("/opt/profile-a/.claude"), None),
            cp(200, Some("/opt/profile-a/.claude"), None),
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
        let discovery = union_discovery(&[cp(100, None, None), cp(200, Some("   "), None)]);
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
        let discovery = union_discovery(&[cp(100, Some("relative/config/dir"), None)]);
        assert_eq!(discovery.registry_dirs, vec![default_config_dir()]);
    }

    #[test]
    fn union_discovery_captures_tmux_panes_keyed_by_pid() {
        let discovery = union_discovery(&[
            cp(100, Some("/opt/profile-a/.claude"), Some("%1")),
            cp(200, Some("/opt/profile-a/.claude"), None),
            cp(300, Some("/opt/profile-b/.claude"), Some("%9")),
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
    }
}
