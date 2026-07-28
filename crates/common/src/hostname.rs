//! Hostname resolution, shared by every agent that reports a session
//! (the reporter today, the watcher soon).

/// Resolve the local machine's hostname.
///
/// Returns `None` if the hostname cannot be determined, or is not valid
/// UTF-8.
pub fn resolve() -> Option<String> {
    hostname::get().ok().and_then(|h| h.into_string().ok())
}
