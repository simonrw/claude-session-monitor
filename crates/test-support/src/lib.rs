//! Shared helpers for cross-crate integration tests.
//!
//! Integration tests in `crates/server/tests` and `crates/reporter/tests`
//! (and future test files in other crates) need to locate binaries built by
//! *other* workspace members and spin up a real server for the test to talk
//! to. A plain `tests/common/mod.rs` inside one crate is not reachable from
//! another crate's `tests/`, so these helpers live in their own small crate
//! that both can take as a dev-dependency.

mod binary;
mod server_harness;
mod wait_for;

pub use binary::locate_bin;
pub use server_harness::start_test_server;
pub use wait_for::wait_for;
