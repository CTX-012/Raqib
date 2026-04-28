//! PID-reuse defenses (CLAUDE.md safety rule, TEST.md G.1.11).
//!
//! When the governor escalates from SIGTERM to SIGKILL, the original
//! process may have exited during the grace period. On Linux the kernel
//! is free to reassign that PID number to an unrelated new process at
//! any time after the original is reaped. Sending SIGKILL by raw PID
//! after the original is gone risks killing a stranger — including
//! potentially an allowlisted process.
//!
//! Two defenses are layered here. The first is the gold standard; the
//! second is the universally-available fallback:
//!
//! 1. **`pidfd_open(2)` + `pidfd_send_signal(2)`** (Linux 5.3, Sep 2019).
//!    A pidfd is a file descriptor that pins one specific instance of a
//!    process. The kernel guarantees signals delivered through it never
//!    race PID reuse. Capture the pidfd at SIGTERM time, send SIGKILL
//!    through it.
//! 2. **`/proc/<pid>/stat` field 22 (`starttime`)**. Kernel-clock-tick
//!    timestamp of when the process was created. Read at SIGTERM time
//!    and re-read at SIGKILL time; if it differs (or if `/proc` is
//!    gone) the original process is dead and the PID has been recycled
//!    or freed — abort the SIGKILL.
//!
//! Both are best-effort: `pidfd_open` may fail on old kernels or in
//! restricted user namespaces, and `/proc` parsing may return None on
//! exotic filesystems. When neither token can be captured at SIGTERM
//! time, the executor records `None` for both and falls back to the
//! "no captured identity" path — which is conservative (refuses the
//! SIGKILL rather than risking a stranger).

use std::fs;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// Read the kernel-clock-tick `starttime` (field 22 of `/proc/<pid>/stat`).
///
/// Field 2 of `/proc/<pid>/stat` is `comm`, which is wrapped in parentheses
/// and may itself contain spaces and parens (e.g. a process whose argv[0]
/// contains `") evil ("`). Splitting on whitespace from the start of the
/// line therefore mis-aligns later fields. The kernel guarantees the LAST
/// `)` in the line ends the comm field, so we anchor parsing there.
///
/// Returns None when the file is unreadable, malformed, or the process
/// has exited (`/proc/<pid>/stat` does not exist after reap).
pub fn read_starttime(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let last_paren = raw.rfind(')')?;
    let after = raw.get(last_paren + 1..)?;
    // After the closing `)` of comm, the next whitespace-separated token is
    // field 3 (state). starttime is field 22, so it is the (22 - 3) = 19th
    // 0-indexed token after that boundary.
    let mut iter = after.split_whitespace();
    iter.nth(19)?.parse::<u64>().ok()
}

/// Best-effort `pidfd_open(pid, 0)`. Returns `None` for any failure
/// (unsupported kernel, ESRCH, EPERM, ENOSYS) — the caller falls back
/// to starttime-based identity verification.
pub fn try_pidfd_open(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open takes (pid_t, unsigned int flags) and returns a new
    // fd or -1. No memory is read or written through this call.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
    if raw < 0 {
        return None;
    }
    // SAFETY: kernel returned a valid file descriptor; OwnedFd takes
    // ownership and closes it on drop.
    Some(unsafe { OwnedFd::from_raw_fd(raw as RawFd) })
}

/// Send `signal` through a captured `pidfd`. Race-free with respect to
/// PID reuse — the kernel resolves the pidfd to the original process
/// instance even if the PID number has since been reassigned.
pub fn pidfd_send_kill(fd: BorrowedFd<'_>, signal: i32) -> io::Result<()> {
    // SAFETY: pidfd_send_signal(fd, sig, NULL, 0) — no userspace memory
    // is dereferenced when siginfo is NULL.
    let r = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_starttime_for_self_is_some() {
        let me = std::process::id();
        let st = read_starttime(me);
        assert!(
            st.is_some(),
            "/proc/<self>/stat must be readable on Linux test hosts"
        );
    }

    #[test]
    fn read_starttime_handles_comm_with_parens() {
        // Synthetic stat line: comm contains spaces and parens, which is
        // legal — the kernel does not escape it.  We exercise the parse
        // path indirectly by writing a fixture into a temp file and
        // pointing read_starttime at a manually-constructed path. Since
        // read_starttime hard-codes /proc/<pid>/stat we reproduce the
        // same parse logic inline.
        let line =
            "1 (weird ) ) name) S 0 1 1 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 12345 6700 0";
        let last = line.rfind(')').unwrap();
        let after = &line[last + 1..];
        let starttime: u64 = after.split_whitespace().nth(19).unwrap().parse().unwrap();
        assert_eq!(starttime, 12345);
    }

    #[test]
    fn read_starttime_returns_none_for_dead_pid() {
        // PID 0 doesn't exist in /proc; PID u32::MAX is well above pid_max.
        assert!(read_starttime(u32::MAX).is_none());
    }

    #[test]
    fn try_pidfd_open_for_self_succeeds_or_skips() {
        // pidfd_open requires Linux 5.3. On a supported host, opening our
        // own PID always works and returns a valid fd. On unsupported
        // hosts we expect None — and skip rather than fail.
        let me = std::process::id();
        if let Some(fd) = try_pidfd_open(me) {
            // Send signal 0 (probe — no actual signal delivered) just
            // to verify the fd works end-to-end on this kernel.
            use std::os::fd::AsFd;
            let result = pidfd_send_kill(fd.as_fd(), 0);
            assert!(result.is_ok(), "pidfd_send_signal probe failed: {result:?}");
        }
    }
}
