//! Locating binaries built by other workspace members.

use std::path::PathBuf;

/// Locate a binary built elsewhere in the workspace by name.
///
/// Cargo sets `CARGO_BIN_EXE_<name>` only for binaries that belong to the
/// crate whose tests are currently running, so a test that needs a binary
/// from a *different* crate (the reporter binary from a server test, the
/// watcher binary from a server test, and so on) cannot rely on it.
///
/// Instead this walks up from the current test binary's own path -
/// `<target_dir>/<profile>/deps/<test-name>-<hash>` - to the shared
/// `<target_dir>/<profile>/` directory, where `cargo build --workspace` and
/// `cargo test --workspace` place every binary in the workspace.
///
/// Panics if the binary is not found; run `cargo build --workspace` (or
/// `cargo test --workspace`, which builds it automatically) first.
pub fn locate_bin(name: &str) -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .unwrap()
        .to_path_buf();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(name);
    assert!(
        path.exists(),
        "binary `{name}` not found at {path:?} -- run `cargo build --workspace` first"
    );
    path
}
