//! `csm-watcher`: reads Claude Code's undocumented session registry and
//! publishes what it finds to the coordination server as one snapshot.
//!
//! `sweep` is the callable unit; this crate deliberately does not own a
//! polling loop or any cross-sweep state (debouncing an absent session
//! across consecutive sweeps, etc.) - that arrives in PRO-210, which is
//! expected to call `sweep::sweep` repeatedly.
//!
//! `discovery` finds the registry directories to sweep by reading live
//! Claude process environments, with `sweep::registry_dirs_from_env`
//! remaining as an explicit override that bypasses it entirely. See
//! `discovery`'s module doc comment for the full design.

mod registry;
mod status;

pub mod discovery;
pub mod publish;
pub mod sweep;
