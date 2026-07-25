//! `csm-watcher`: reads Claude Code's undocumented session registry and
//! publishes what it finds to the coordination server as one snapshot.
//!
//! `sweep` is the callable unit; this crate deliberately does not own a
//! polling loop itself - that arrives in PRO-210 (`main.rs`'s `run_daemon`),
//! which calls `sweep::sweep` repeatedly on an interval. Cross-sweep state
//! (debouncing an absent session across consecutive sweeps) is separate
//! again, and is not added by PRO-210 either - that is PRO-211, which builds
//! directly on the daemon loop PRO-210 adds.
//!
//! `discovery` finds the registry directories to sweep by reading live
//! Claude process environments, with `sweep::registry_dirs_from_env`
//! remaining as an explicit override that bypasses it entirely. See
//! `discovery`'s module doc comment for the full design.
//!
//! `tmux` resolves each session's pane id (from `discovery::Discovery::tmux_panes`)
//! into an activation target, and `git` derives and caches each session's
//! branch and remote from its `cwd`. Both are consumed by `sweep::sweep` and
//! both degrade to "no enrichment" on any failure rather than failing the
//! sweep - see each module's own doc comment. Both route their subprocess
//! calls through `command`, which bounds every invocation by a timeout and
//! kills a timed-out child's whole process group rather than leaking it.

mod command;
mod registry;
mod status;
mod tmux;

pub mod discovery;
pub mod git;
pub mod publish;
pub mod sweep;
