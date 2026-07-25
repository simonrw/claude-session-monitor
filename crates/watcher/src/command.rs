//! A subprocess invocation bounded by a timeout, shared by every module in
//! this crate that shells out (`git`, `tmux`).
//!
//! Two properties matter enough to centralise here rather than duplicate:
//!
//! 1. A hung child must never stall a sweep - PRO-210 runs the sweep in a
//!    loop every couple of seconds, so a single unbounded subprocess call
//!    wedges the daemon permanently rather than just degrading one lookup.
//! 2. A child that times out is killed by its whole process group, not
//!    just its own pid. `git` in particular spawns helpers - a credential
//!    helper, `git-*` subcommands, hooks - that inherit the group; killing
//!    only the direct child leaves those grandchildren running as orphans.
//!    Reproduced directly: with a `git` stub that backgrounded a `sleep
//!    300` before hanging itself, killing only the direct pid left the
//!    `sleep 300` process running with ppid 1 after the timeout. Under a
//!    two-second sweep loop that accumulates - six such orphans were still
//!    running after one short reproduction run - so [`kill_and_reap`] signals
//!    the negative pid (the process group) instead.
//!
//! A third property, easy to miss because it only shows up under a large
//! output: stdout is drained on a background thread concurrently with the
//! wait loop below, not read only after the child has already exited. A
//! pipe's kernel buffer is small (~64KB) - a child that writes more than
//! that before exiting blocks on the write once the buffer fills, and stays
//! blocked until something reads from the other end. Reading only after
//! `try_wait` reports the child gone is exactly backwards for such a child:
//! nothing ever reads, so it never finishes writing, so it never exits, so
//! [`run`] waits out the full `timeout` and reports failure - silently
//! discarding output that would otherwise have arrived in milliseconds.
//! Reproduced directly: a child writing ~283KB (4000 lines) succeeded
//! immediately with the read-after-wait shape this module used before this
//! fix at a smaller size, but at that size ran out the full timeout and
//! returned `None`, with the real command (`tmux list-panes -a`, on a host
//! with enough panes) far more likely to cross that threshold than `git`'s
//! typically one-line outputs. `stderr` needs no equivalent treatment: it
//! is never piped (see [`run`]) - it goes straight to `/dev/null`, which
//! never fills or blocks a writer - so there is nothing to drain there.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How often a still-running command is polled for completion while
/// waiting out its timeout. Well under any timeout this crate configures
/// (half a second, for both git and tmux), so a timeout is enforced to
/// within one poll tick rather than one whole extra interval; coarse enough
/// that polling a single subprocess never shows up as meaningful CPU use,
/// since at most one command is ever being awaited at a time.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Run `program args...` (in `dir`, if given), bounded by `timeout`,
/// returning trimmed stdout on success or `None` on any failure: the binary
/// is missing, `dir` (when given) does not exist, the command exits
/// non-zero, its stdout is empty, or it exceeds `timeout` - in which case
/// its whole process group is killed and reaped rather than left running or
/// leaked (see [`kill_and_reap`]).
///
/// This is the one impure boundary both `git` and `tmux` route their
/// subprocess calls through, so the timeout, process-group-kill, and reap
/// behaviour only has to be gotten right - and tested - once.
pub(crate) fn run(
    program: &str,
    args: &[&str],
    dir: Option<&Path>,
    timeout: Duration,
) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    configure_process_group(&mut command);
    let mut child = command.spawn().ok()?;

    // Drain stdout on a background thread, concurrently with the wait loop
    // below, rather than only after the child has exited - see this
    // module's doc comment for why reading only after exit deadlocks a
    // child whose output exceeds the pipe's kernel buffer. `take()` moves
    // the pipe's read end into the thread; nothing else reads it, so
    // there's no risk of the two racing over the same handle.
    let stdout_pipe = child.stdout.take();
    let reader = stdout_pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            // A read error here (e.g. invalid UTF-8) just means less output
            // was captured than the child actually wrote; the exit status
            // check below is what actually decides success or failure, so
            // this thread doesn't need its own error path.
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    tracing::debug!(
                        program,
                        ?args,
                        ?timeout,
                        "command exceeded timeout, killing its process group"
                    );
                    kill_and_reap(&mut child);
                    // Killing the process group closes the child's (and any
                    // descendant's) end of the stdout pipe, so the reader
                    // thread's `read_to_string` above hits EOF and returns
                    // promptly rather than this join blocking indefinitely.
                    join_reader(reader);
                    return None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                tracing::debug!(program, ?args, error = %e, "failed waiting for command");
                // The direct-child reap on the timeout path above was
                // already correct before this module existed; this arm was
                // the one that leaked - it returned without killing or
                // waiting on the child at all, so a `try_wait` error left
                // the process (and, absent the fix above, its group) behind
                // indefinitely.
                kill_and_reap(&mut child);
                join_reader(reader);
                return None;
            }
        }
    };

    // The child has exited by this point, which closes its end of the
    // pipe; the reader thread sees EOF and returns on its own, so this join
    // is bounded by however long it takes to hand back an already-collected
    // buffer, not by anything still running.
    let stdout = join_reader(reader)?;

    if !status.success() {
        return None;
    }

    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Join the background stdout-reading thread [`run`] spawned, returning the
/// output it collected. `None` only when there was no pipe to read in the
/// first place (`child.stdout` was already absent) or the thread itself
/// panicked - both already-degenerate cases upstream, not new failure modes
/// this function introduces.
fn join_reader(reader: Option<std::thread::JoinHandle<String>>) -> Option<String> {
    reader.and_then(|r| r.join().ok())
}

/// Give `command`'s child its own process group, whose id equals its own
/// pid (`process_group(0)`), so [`kill_and_reap`] can later signal every
/// process it spawned, not just itself.
#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

/// Kill `child`'s entire process group and reap it.
///
/// `child` was spawned via [`configure_process_group`], so its pgid equals
/// its own pid; signalling the negation of that pid, per POSIX `kill(2)`,
/// signals every process in the group instead of just `child` itself. The
/// direct child is still killed and reaped by this - it is a member of its
/// own group - so this is a strict superset of the old direct-child-only
/// behaviour, not a change to it.
#[cfg(unix)]
fn kill_and_reap(child: &mut Child) {
    let pgid = child.id() as libc::pid_t;
    // SAFETY: `libc::kill` has no memory-safety preconditions; passing a
    // negative pid is the documented, intentional way to target a whole
    // process group rather than a single process.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_drains_output_well_over_the_pipe_buffer_without_deadlocking() {
        // Reproduces finding 2 from the second PRO-209 review round
        // directly: before concurrent draining, stdout was only read after
        // `try_wait` reported the child had exited, so a child writing more
        // than the pipe's kernel buffer (~64KB) blocked forever on the
        // write and was killed at the timeout instead of completing -
        // measured directly against the pre-fix code, ~283KB (4000 lines)
        // ran out a generous 5s timeout and returned `None`. This produces
        // the same ~283KB and uses a timeout an order of magnitude tighter
        // (2s) than that reproduction to prove the fix isn't just "happens
        // to finish before a lenient bound", plus a line count assertion so
        // truncated output would fail the test even if `run` still returned
        // `Some`.
        let script = (1..=4000)
            .map(|i| {
                format!(
                    "echo line-{i}-012345678901234567890123456789012345678901234567890123456789"
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let start = Instant::now();
        let result = run("sh", &["-c", &script], None, Duration::from_secs(2));
        let output = result.expect("large stdout must be drained, not deadlocked into a timeout");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must complete well before the timeout, not be rescued by it, took {:?}",
            start.elapsed()
        );
        assert_eq!(
            output.lines().count(),
            4000,
            "every line must survive the drain"
        );
        assert!(output.starts_with("line-1-"));
        assert!(
            output.ends_with(
                "line-4000-012345678901234567890123456789012345678901234567890123456789"
            )
        );
    }

    #[test]
    fn run_missing_binary_degrades_to_none() {
        let dir = std::env::temp_dir();
        let result = run(
            "definitely-not-a-real-command-xyz",
            &["--version"],
            Some(&dir),
            Duration::from_secs(1),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn run_nonexistent_dir_degrades_to_none() {
        let result = run(
            "sh",
            &["-c", "echo hi"],
            Some(Path::new("/nonexistent/path/that/does/not/exist")),
            Duration::from_secs(1),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn run_with_no_dir_uses_the_caller_s_own_working_directory() {
        // `tmux` has no meaningful "cwd" the way `git` does, so `dir` is
        // `None` for its calls - this proves that path works at all.
        let result = run("sh", &["-c", "echo hi"], None, Duration::from_secs(1));
        assert_eq!(result.as_deref(), Some("hi"));
    }

    #[test]
    fn run_kills_and_degrades_to_none_on_timeout() {
        let dir = std::env::temp_dir();
        let start = Instant::now();
        let result = run(
            "sh",
            &["-c", "sleep 5"],
            Some(&dir),
            Duration::from_millis(100),
        );
        assert_eq!(result, None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a timed-out command must not block anywhere near its own sleep duration, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn run_nonzero_exit_degrades_to_none() {
        let dir = std::env::temp_dir();
        let result = run("sh", &["-c", "exit 1"], Some(&dir), Duration::from_secs(1));
        assert_eq!(result, None);
    }

    #[test]
    fn run_trims_and_treats_empty_output_as_none() {
        let dir = std::env::temp_dir();
        let result = run(
            "sh",
            &["-c", "echo '  hello  '"],
            Some(&dir),
            Duration::from_secs(1),
        );
        assert_eq!(result.as_deref(), Some("hello"));

        let empty = run("sh", &["-c", "true"], Some(&dir), Duration::from_secs(1));
        assert_eq!(empty, None);
    }

    #[test]
    #[cfg(unix)]
    fn timeout_kills_the_whole_process_group_not_just_the_direct_child() {
        // Reproduces finding 2 from the PRO-209 review directly: `sh -c`
        // backgrounds a grandchild `sleep`, records its pid to a file, then
        // hangs itself so the outer command times out. Before this module
        // existed, `Child::kill()` signalled only the direct `sh` pid,
        // leaving the backgrounded `sleep` running as an orphan (ppid 1)
        // after the timeout.
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("csm-watcher-orphan-test-{}", std::process::id()));
        let marker_path = marker.to_str().unwrap();
        let script = format!("sleep 300 & echo $! > '{marker_path}'; sleep 300");

        let result = run(
            "sh",
            &["-c", &script],
            Some(&dir),
            Duration::from_millis(150),
        );
        assert_eq!(result, None);

        // Give the grandchild a moment to actually have started and
        // written its pid before we check it's gone; the timeout above
        // already waited out the parent, so this is a short, bounded
        // extra wait, not a race against the kill itself.
        let mut grandchild_pid = None;
        for _ in 0..50 {
            if let Ok(contents) = std::fs::read_to_string(&marker) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(&marker);
        let grandchild_pid = grandchild_pid.expect("grandchild must have recorded its own pid");

        // Poll briefly: the group signal is sent synchronously by
        // `kill_and_reap`, but the OS reaping the grandchild (so a fresh
        // `kill(pid, 0)` reports it gone) is not instantaneous.
        let mut still_alive = true;
        for _ in 0..50 {
            if !common::process::is_alive(grandchild_pid) {
                still_alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !still_alive,
            "grandchild pid {grandchild_pid} must not survive the timeout - process-group kill \
             must reach it, not just the direct child"
        );
    }
}
