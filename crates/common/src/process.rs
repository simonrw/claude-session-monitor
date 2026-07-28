//! OS process introspection: liveness and start-time lookup by pid.
//!
//! Shared by any agent that needs to distinguish a genuinely live process
//! from a stale record referring to a pid that has since exited or been
//! recycled by the OS (the watcher's registry sweep is the first caller,
//! matching a session's recorded start time against the OS's own record to
//! defend against pid reuse).

/// Return whether a process with the given pid currently exists.
///
/// Uses `kill(pid, 0)`: sending signal 0 performs no actual signal delivery,
/// only the existence/permission check. `EPERM` (process exists but is
/// owned by another user) still counts as alive; `ESRCH` (no such process)
/// is the only case that means dead.
///
/// `pid <= 0` is rejected up front rather than handed to `kill`: `0` means
/// "every process in the caller's process group" and negative pids mean
/// "every process in that process group" (or, for `-1`, every process the
/// caller may signal), so both return success for reasons that have
/// nothing to do with any single registry entry being alive. No registry
/// entry should ever carry such a pid, but a session with one is dropped
/// anyway once enrichment reaches `start_time` (which returns `None` for
/// it) - this guard just makes that intentional here rather than incidental
/// fallout from `kill`'s group semantics.
#[cfg(unix)]
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn is_alive(_pid: i32) -> bool {
    false
}

/// Return the OS-recorded start time of the process with the given pid, as
/// Unix epoch seconds (UTC). Returns `None` if it cannot be determined (the
/// process has exited, or the platform is unsupported).
pub fn start_time(pid: i32) -> Option<i64> {
    imp::start_time(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    /// `proc_pidinfo(pid, PROC_PIDTBSDINFO, ...)` fills a `proc_bsdinfo`
    /// struct whose `pbi_start_tvsec` field is the process's start time as
    /// Unix epoch seconds.
    pub fn start_time(pid: i32) -> Option<i64> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let ret = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if ret != size {
            return None;
        }
        Some(info.pbi_start_tvsec as i64)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// Linux has no equivalent single syscall; the start time is read from
    /// `/proc/<pid>/stat` field 22 (`starttime`, in clock ticks since boot),
    /// converted to seconds via `sysconf(_SC_CLK_TCK)` and added to the
    /// system boot time from `/proc/stat`'s `btime` line.
    pub fn start_time(pid: i32) -> Option<i64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

        // Field 2 (`comm`) is parenthesized and may itself contain spaces or
        // closing parens, so find the *last* ')' rather than splitting
        // naively on whitespace, then resume field counting from field 3.
        let after_comm = stat.rfind(')')?;
        let rest = &stat[after_comm + 1..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // `starttime` is field 22 overall; `rest` starts at field 3, so its
        // index here is 22 - 3 = 19.
        let starttime_ticks: u64 = fields.get(19)?.parse().ok()?;

        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if clk_tck <= 0 {
            return None;
        }
        let starttime_secs = starttime_ticks / clk_tck as u64;

        let proc_stat = std::fs::read_to_string("/proc/stat").ok()?;
        let btime = proc_stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|v| v.trim().parse::<i64>().ok())?;

        Some(btime + starttime_secs as i64)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn start_time(_pid: i32) -> Option<i64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_alive(std::process::id() as i32));
    }

    #[test]
    fn non_positive_pids_are_never_alive() {
        // `kill(0, 0)` and `kill(-1, 0)` both succeed - they mean "signal a
        // process group", not "does this one process exist" - so both must
        // be rejected before reaching `kill` at all.
        assert!(!is_alive(0));
        assert!(!is_alive(-1));
    }

    #[test]
    fn exited_child_is_not_alive() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let pid = child.id() as i32;
        child.wait().expect("wait for child");
        assert!(!is_alive(pid));
    }

    #[test]
    fn current_process_start_time_is_recent_and_stable() {
        let a = start_time(std::process::id() as i32).expect("start time for self");
        let b = start_time(std::process::id() as i32).expect("start time for self");
        assert_eq!(a, b, "start time must be stable across calls");

        let now = chrono::Utc::now().timestamp();
        assert!(
            a <= now + 5 && a > now - 3600,
            "start time {a} should be within the last hour of {now}"
        );
    }
}
