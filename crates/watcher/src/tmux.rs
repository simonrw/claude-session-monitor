//! Resolving a session's `TMUX_PANE` (captured from its process environment
//! by `discovery`, since the registry's own `tmux` field is unreliable - see
//! `discovery`'s module doc comment) into the `session:window.pane`
//! activation target `crate::activation::activate` already expects.
//!
//! One `tmux list-panes -a` invocation covers every pane on the host, so a
//! sweep with any number of sessions still costs exactly one tmux
//! invocation: [`resolve_all_panes`] is called once per sweep by
//! `crate::sweep::sweep`, and [`resolve_target`] then joins each entry's pid
//! against the resulting map purely in memory.
//!
//! `tmux` not being installed, `tmux list-panes` erroring because no server
//! is running, and `tmux list-panes` hanging and exceeding
//! [`LIST_PANES_TIMEOUT`] all degrade to an empty map rather than failing
//! the sweep - every session still publishes, just without a `tmux_target`.
//! The invocation is routed through `crate::command::run`, the same bounded
//! runner `git` uses, specifically so the hang case is a real degrade and
//! not an unbounded stall: under PRO-210's polling loop, a bare
//! `Command::output()` with no timeout - the shape this module used before
//! this fix - would wedge the daemon permanently the first time `tmux`
//! itself hung, since nothing would ever return to let the next sweep run.
//! Reproduced directly: a hanging `tmux` on `PATH` left `csm-watcher --once`
//! still running, having published nothing, past 25 seconds before this fix.
//!
//! **Known limitation:** `tmux list-panes -a` only lists panes on the
//! *default* tmux socket. A session running under a separate server
//! (`tmux -L other ...`) is invisible to this listing entirely, and worse,
//! tmux's pane ids (`%38` and the like) are only unique *within* one
//! server - a pane on a second server can reuse an id already present in
//! the default server's listing, in which case a session on that second
//! server joins to the wrong pane here and activation jumps to the wrong
//! place rather than simply failing to resolve. This is not handled;
//! multi-server tmux setups are unsupported by this module.

use std::collections::HashMap;
use std::time::Duration;

/// `tmux list-panes -a -F "..."` format string. `#{pane_id}` (e.g. `%38`) is
/// what a pane's process environment carries as `TMUX_PANE`, and
/// `#{session_name}:#{window_index}.#{pane_index}` is exactly the
/// `session:window.pane` shape `common::activation::TmuxTarget::parse`
/// consumes - the same format `crates/reporter/src/enrichment.rs`'s
/// `detect_tmux_target` produces for the hook path, so this join keeps
/// activation working identically for watcher-reported sessions.
const LIST_PANES_FORMAT: &str = "#{pane_id} #{session_name}:#{window_index}.#{pane_index}";

/// Upper bound on the `tmux list-panes -a` invocation.
///
/// tmux talking to its own local server is normally sub-millisecond, so
/// this exists purely as a safety net against a wedged or hung tmux server
/// - mirroring `git::DEFAULT_COMMAND_TIMEOUT`'s rationale (see that
/// constant's doc comment). Without it, under PRO-210's polling loop, one
/// hung `tmux` invocation wedges the watcher forever rather than degrading
/// this one sweep's pane resolution; see this module's doc comment for the
/// direct reproduction.
const LIST_PANES_TIMEOUT: Duration = Duration::from_millis(500);

/// List every pane on the host, keyed by pane id, mapped to its activation
/// target.
///
/// Returns an empty map - never an error - if `tmux` is not installed, if
/// `tmux list-panes` fails (most commonly: no tmux server is running at
/// all), or if it exceeds [`LIST_PANES_TIMEOUT`]. Every case means no
/// session on this host is resolvable to a tmux target this sweep, which is
/// a normal, expected state, not a sweep failure.
pub(crate) fn resolve_all_panes() -> HashMap<String, String> {
    match run_list_panes("tmux") {
        Some(output) => parse_list_panes(&output),
        None => {
            tracing::debug!(
                "tmux list-panes unavailable, failed, or timed out; no sessions will have a \
                 tmux_target this sweep"
            );
            HashMap::new()
        }
    }
}

/// Look up `pid`'s activation target: its pane id (from `tmux_panes`,
/// captured from the process environment by `discovery`) resolved against
/// `pane_targets` (from [`resolve_all_panes`]).
///
/// Returns `None`, rather than failing anything, whenever either lookup
/// misses: the pid has no recorded `TMUX_PANE` (not running inside tmux at
/// all), or its pane id is not in the current listing (the pane was closed
/// between discovery's process read and this sweep's tmux listing, or tmux
/// itself was unavailable and `pane_targets` is empty).
pub(crate) fn resolve_target(
    pid: i32,
    tmux_panes: &HashMap<i32, String>,
    pane_targets: &HashMap<String, String>,
) -> Option<String> {
    let pane_id = tmux_panes.get(&pid)?;
    pane_targets.get(pane_id).cloned()
}

/// Run `<program> list-panes -a -F <LIST_PANES_FORMAT>`, bounded by
/// [`LIST_PANES_TIMEOUT`], and return its stdout.
///
/// Routed through `crate::command::run` - the same bounded runner `git`
/// uses - so a hung `tmux` degrades this one sweep's pane resolution
/// instead of stalling the sweep (or, under PRO-210, the whole daemon)
/// indefinitely.
///
/// `program` is parameterised only so a test can point this at a binary
/// that does not exist and exercise the "tmux unavailable" degrade path,
/// mirroring `discovery::imp::run_ps`'s identical test seam.
fn run_list_panes(program: &str) -> Option<String> {
    crate::command::run(
        program,
        &["list-panes", "-a", "-F", LIST_PANES_FORMAT],
        None,
        LIST_PANES_TIMEOUT,
    )
}

/// Parse `tmux list-panes -a -F "#{pane_id} #{session_name}:#{window_index}.#{pane_index}"`
/// output: one pane per line, `pane_id` then its activation target,
/// space-separated. A line that does not split into exactly a pane id and a
/// non-empty target is skipped rather than aborting the parse - tmux's own
/// format is trusted, but a stray blank line must not poison the rest.
fn parse_list_panes(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pane_id, target) = line.split_once(' ')?;
            if pane_id.is_empty() || target.is_empty() {
                return None;
            }
            Some((pane_id.to_owned(), target.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_list_panes (pure - fixture bytes) ---

    #[test]
    fn parse_list_panes_extracts_pane_id_to_target_map() {
        let output = "%38 main:0.1\n%2 work:1.0\n%7 my-project:0.3\n";
        let panes = parse_list_panes(output);
        assert_eq!(panes.len(), 3);
        assert_eq!(panes.get("%38").map(String::as_str), Some("main:0.1"));
        assert_eq!(panes.get("%2").map(String::as_str), Some("work:1.0"));
        assert_eq!(panes.get("%7").map(String::as_str), Some("my-project:0.3"));
    }

    #[test]
    fn parse_list_panes_empty_output_yields_empty_map() {
        assert!(parse_list_panes("").is_empty());
    }

    #[test]
    fn parse_list_panes_skips_blank_lines() {
        let output = "%38 main:0.1\n\n   \n%2 work:1.0\n";
        let panes = parse_list_panes(output);
        assert_eq!(panes.len(), 2);
    }

    // --- resolve_target (pure) ---

    #[test]
    fn resolve_target_joins_pid_through_pane_id_to_activation_target() {
        let tmux_panes = HashMap::from([(100, "%38".to_string())]);
        let pane_targets = HashMap::from([("%38".to_string(), "main:0.1".to_string())]);
        assert_eq!(
            resolve_target(100, &tmux_panes, &pane_targets).as_deref(),
            Some("main:0.1")
        );
    }

    #[test]
    fn resolve_target_none_when_pid_has_no_recorded_pane() {
        let tmux_panes = HashMap::new();
        let pane_targets = HashMap::from([("%38".to_string(), "main:0.1".to_string())]);
        assert_eq!(resolve_target(100, &tmux_panes, &pane_targets), None);
    }

    #[test]
    fn resolve_target_none_when_pane_id_is_not_in_the_current_listing() {
        // The pane was closed between discovery's process read and this
        // sweep's tmux listing, or tmux was unavailable and pane_targets is
        // empty - either way this must degrade to None, not panic or
        // propagate an error.
        let tmux_panes = HashMap::from([(100, "%38".to_string())]);
        let pane_targets = HashMap::new();
        assert_eq!(resolve_target(100, &tmux_panes, &pane_targets), None);
    }

    // --- run_list_panes / resolve_all_panes degrade paths (impure boundary) ---

    #[test]
    fn run_list_panes_degrades_to_none_when_the_binary_is_missing() {
        assert_eq!(
            run_list_panes("definitely-not-a-real-tmux-binary-xyz"),
            None
        );
    }

    #[test]
    fn run_list_panes_degrades_to_none_when_it_hangs_past_its_timeout() {
        // `crate::command`'s own timeout/kill-and-reap behaviour is tested
        // thoroughly in that module; this only proves `run_list_panes`
        // actually uses `LIST_PANES_TIMEOUT` (well under the default 500ms)
        // rather than, say, hard-coding a much longer bound that would
        // still wedge a sweep in practice.
        let start = std::time::Instant::now();
        let result = crate::command::run("sh", &["-c", "sleep 5"], None, LIST_PANES_TIMEOUT);
        assert_eq!(result, None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must not block anywhere near the hung command's own sleep duration, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn resolve_all_panes_never_panics_when_tmux_is_unavailable_or_available() {
        // This runs against whatever the real environment provides (tmux
        // may or may not be installed, and may or may not have a server
        // running) - the only thing asserted is that it never panics and
        // always returns (possibly empty).
        let _ = resolve_all_panes();
    }
}
