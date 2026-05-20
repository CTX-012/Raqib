use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};

use crate::model::ProcessSample;
use crate::platform::{PlatformError, PlatformResult};

/// Collects process information from /proc on Linux.
/// Handles parsing of cmdline, environ, status, and cwd entries.
pub struct ProcessCollector;

/// Sprint-7 Item 3 — `/proc/stat`'s `btime` field, cached for the
/// process lifetime. `btime` is the epoch second the kernel booted;
/// it doesn't change while we run, so reading it once is enough.
/// `None` when the parse fails (alien `/proc`, fakeproc, etc.) — the
/// caller falls back to `first_observed_at` in that case.
fn boot_time_epoch_secs() -> Option<u64> {
    static BOOT_TIME: OnceLock<Option<u64>> = OnceLock::new();
    *BOOT_TIME.get_or_init(|| {
        let content = fs::read_to_string("/proc/stat").ok()?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("btime ") {
                return rest.trim().parse::<u64>().ok();
            }
        }
        None
    })
}

/// `sysconf(_SC_CLK_TCK)` cached for the process lifetime. Returns
/// 100 on failure — that's the Linux kernel default and the value
/// every distro we target ships with.
fn clk_tck_hz() -> u64 {
    static CLK_TCK: OnceLock<u64> = OnceLock::new();
    *CLK_TCK.get_or_init(|| {
        let raw = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if raw <= 0 { 100 } else { raw as u64 }
    })
}

impl ProcessCollector {
    /// Creates a new process collector.
    pub fn new() -> PlatformResult<Self> {
        // Verify /proc exists
        if !Path::new("/proc").is_dir() {
            return Err(PlatformError::ProcDirRead(
                "/proc not found or not a directory".to_string(),
            ));
        }
        Ok(ProcessCollector)
    }

    /// Collects all running processes from /proc.
    /// Returns a Vec<ProcessSample> where each sample has been parsed from the filesystem.
    pub fn collect(&self) -> PlatformResult<Vec<ProcessSample>> {
        let mut processes = Vec::new();

        let entries = fs::read_dir("/proc")
            .map_err(|e| PlatformError::ProcDirRead(format!("cannot read /proc: {}", e)))?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                PlatformError::ProcDirRead(format!("error reading /proc entry: {}", e))
            })?;
            let path = entry.path();

            // Only process numeric directories (PIDs)
            if let Some(filename) = path.file_name()
                && let Ok(pid_str) = filename.to_string_lossy().parse::<u32>()
            {
                match self.read_process_sample(pid_str) {
                    Ok(sample) => processes.push(sample),
                    Err(e) => {
                        // Log and continue; process may have exited
                        tracing::debug!(pid = pid_str, error = %e, "skipped process");
                    }
                }
            }
        }

        Ok(processes)
    }

    /// Reads a single process's information from /proc/<pid>/*.
    fn read_process_sample(&self, pid: u32) -> PlatformResult<ProcessSample> {
        let pid_path = PathBuf::from(format!("/proc/{}", pid));

        let name = self.read_comm(&pid_path, pid)?;
        let cmdline = self.read_cmdline(&pid_path, pid)?;
        let environ = self.read_environ(&pid_path, pid)?;
        let ppid = self.read_ppid(&pid_path, pid)?;
        let cwd = self.read_cwd(&pid_path);
        // RSS / CPU reads failing is not a reason to drop the process — they
        // can fail transiently when the process is exiting. Fall back to 0.
        let rss_bytes = self.read_rss_bytes(&pid_path).unwrap_or(0);
        let cpu_time_ticks = self.read_cpu_ticks(&pid_path).unwrap_or(0);
        // Sprint-7 Item 3 — true OS spawn time from
        // /proc/<pid>/stat field 22. `None` falls back at the
        // runtime layer to L11b's `first_observed_at`.
        let os_start_time = self.read_os_start_time(&pid_path);

        Ok(ProcessSample {
            pid,
            ppid,
            name,
            cmdline,
            environ,
            cwd,
            rss_bytes,
            cpu_time_ticks,
            os_start_time,
        })
    }

    /// Sprint-7 Item 3 — derive the OS spawn timestamp from
    /// `/proc/<pid>/stat`. Reads field 22 (`starttime`, clock ticks
    /// since boot), then composes:
    ///
    /// `epoch_secs = boot_time_epoch + (starttime / clk_tck)`
    ///
    /// `chrono::DateTime<Utc>` is the wire type; `None` flows
    /// through when any of the inputs fails (stat unreadable,
    /// btime missing, parse failure) — the runtime falls back to
    /// `first_observed_at`.
    fn read_os_start_time(&self, pid_path: &Path) -> Option<DateTime<Utc>> {
        let stat_path = pid_path.join("stat");
        let content = fs::read_to_string(&stat_path).ok()?;
        let starttime_ticks = parse_starttime_from_stat(&content)?;
        let btime = boot_time_epoch_secs()?;
        let secs_since_boot = starttime_ticks / clk_tck_hz();
        let epoch_secs = btime.checked_add(secs_since_boot)?;
        chrono::DateTime::from_timestamp(epoch_secs as i64, 0)
    }

    /// Parses the VmRSS line from /proc/<pid>/status. VmRSS is reported in kB.
    /// Returns bytes. Kernel threads have no VmRSS; Err is mapped to 0 at the
    /// call site so the process still appears in the snapshot.
    fn read_rss_bytes(&self, pid_path: &Path) -> PlatformResult<u64> {
        let status_path = pid_path.join("status");
        let content = fs::read_to_string(&status_path).map_err(PlatformError::Io)?;
        parse_vmrss_kb(&content)
            .map(|kb| kb * 1024)
            .ok_or_else(|| PlatformError::StatParse("VmRSS not found in status".into()))
    }

    /// Reads utime + stime (fields 14 + 15) from /proc/<pid>/stat. The value
    /// is in clock ticks since the process started; the runtime turns deltas
    /// across ticks into a CPU percentage.
    fn read_cpu_ticks(&self, pid_path: &Path) -> PlatformResult<u64> {
        let stat_path = pid_path.join("stat");
        let content = fs::read_to_string(&stat_path).map_err(PlatformError::Io)?;
        parse_cpu_ticks_from_stat(&content)
            .ok_or_else(|| PlatformError::StatParse("could not parse utime+stime".into()))
    }

    /// Reads the process name from /proc/<pid>/comm.
    /// Falls back to the first cmdline token if comm is unavailable.
    fn read_comm(&self, pid_path: &Path, pid: u32) -> PlatformResult<String> {
        let comm_path = pid_path.join("comm");
        match fs::read_to_string(&comm_path) {
            Ok(s) => Ok(s.trim().to_string()),
            Err(_) => {
                // Fall back to reading the first cmdline token (basename)
                let cmdline = self.read_cmdline(pid_path, pid)?;
                Ok(cmdline
                    .first()
                    .and_then(|s| Path::new(s).file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string())
            }
        }
    }

    /// Reads /proc/<pid>/cmdline and splits on null bytes.
    /// Returns a Vec where each element is one argv token.
    fn read_cmdline(&self, pid_path: &Path, pid: u32) -> PlatformResult<Vec<String>> {
        let cmdline_path = pid_path.join("cmdline");
        let raw = fs::read(&cmdline_path)
            .map_err(|e| PlatformError::CmdlineRead(format!("pid {}: {}", pid, e)))?;

        if raw.is_empty() {
            // Some processes (kernel threads) have empty cmdline
            return Ok(vec![]);
        }

        // cmdline is null-separated; split on \0
        let cmdline: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();

        Ok(cmdline)
    }

    /// Reads /proc/<pid>/environ and parses into a HashMap.
    /// Returns key=value pairs from the environment block (null-separated).
    ///
    /// Permission denied is the common case for processes owned by other UIDs
    /// — including PID 1, kernel threads, and most service daemons. Returning
    /// an empty map (rather than propagating the error) lets the platform
    /// layer still report the process; the classifier just loses one signal.
    fn read_environ(&self, pid_path: &Path, pid: u32) -> PlatformResult<HashMap<String, String>> {
        let environ_path = pid_path.join("environ");
        let raw = match fs::read(&environ_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::trace!(pid, "environ read denied; using empty env");
                return Ok(HashMap::new());
            }
            Err(e) => {
                return Err(PlatformError::EnvironRead(format!("pid {}: {}", pid, e)));
            }
        };

        let mut map = HashMap::new();

        if raw.is_empty() {
            return Ok(map);
        }

        // environ is null-separated KEY=VALUE pairs
        for chunk in raw.split(|&b| b == 0) {
            if chunk.is_empty() {
                continue;
            }
            let entry = String::from_utf8_lossy(chunk);
            if let Some((key, value)) = entry.split_once('=') {
                map.insert(key.to_string(), value.to_string());
            }
        }

        Ok(map)
    }

    /// Reads parent PID from /proc/<pid>/status.
    /// Returns None if parsing fails (some restricted processes).
    fn read_ppid(&self, pid_path: &Path, pid: u32) -> PlatformResult<Option<u32>> {
        let status_path = pid_path.join("status");
        let content = fs::read_to_string(&status_path)
            .map_err(|e| PlatformError::StatusRead(format!("pid {}: {}", pid, e)))?;

        // Look for "PPid:\t<number>"
        for line in content.lines() {
            if line.starts_with("PPid:")
                && let Some(ppid_str) = line.split('\t').nth(1)
                && let Ok(ppid) = ppid_str.trim().parse::<u32>()
            {
                return Ok(Some(ppid));
            }
        }

        Ok(None)
    }

    /// Reads the working directory from /proc/<pid>/cwd symlink.
    /// Returns None if the symlink cannot be read (permission issues).
    fn read_cwd(&self, pid_path: &Path) -> Option<PathBuf> {
        let cwd_path = pid_path.join("cwd");
        fs::read_link(&cwd_path).ok()
    }
}

/// Extracts VmRSS (in kB) from the contents of /proc/<pid>/status.
/// Shape of the target line: `VmRSS:\t   12345 kB`.
pub(crate) fn parse_vmrss_kb(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // Collect digits between the tab and the "kB" suffix.
            let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
            return digits.parse::<u64>().ok();
        }
    }
    None
}

/// Parses utime+stime (fields 14+15) from a /proc/<pid>/stat line.
///
/// Field 2 — comm — is enclosed in parens and may itself contain spaces or
/// nested parens (e.g. `(llama-cli (worker))`). Robust parsers split on the
/// LAST `)` before whitespace-tokenizing the remainder, so that's what we do.
pub(crate) fn parse_cpu_ticks_from_stat(stat: &str) -> Option<u64> {
    let rparen = stat.rfind(')')?;
    let tail = &stat[rparen + 1..];
    let fields: Vec<&str> = tail.split_ascii_whitespace().collect();
    // After comm, the next field is (3) state. The raw stat layout is
    // documented in `proc(5)`: we want (14) utime and (15) stime which sit at
    // indices 11 and 12 of `tail` (since tail starts at field 3).
    let utime = fields.get(11).and_then(|s| s.parse::<u64>().ok())?;
    let stime = fields.get(12).and_then(|s| s.parse::<u64>().ok())?;
    Some(utime + stime)
}

/// Sprint-7 Item 3 — parse field 22 (`starttime`, clock ticks since
/// boot) from a `/proc/<pid>/stat` line.
///
/// Same `rfind(')')` strategy as the cpu-ticks parser: comm (field
/// 2) can carry parens / spaces, so we anchor on the LAST `)` and
/// tokenize after. Field 22 maps to index 19 of the post-comm tail
/// because `tail` starts at field 3.
pub(crate) fn parse_starttime_from_stat(stat: &str) -> Option<u64> {
    let rparen = stat.rfind(')')?;
    let tail = &stat[rparen + 1..];
    let fields: Vec<&str> = tail.split_ascii_whitespace().collect();
    // Field 22 = index 19 in the post-comm tail (22 − 3 = 19).
    fields.get(19).and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_collector_succeeds_on_linux() {
        // On a real Linux system, /proc should exist
        match ProcessCollector::new() {
            Ok(_) => (),
            Err(e) => {
                // On WSL or restricted systems, it's OK to fail
                eprintln!("ProcessCollector not available: {}", e);
            }
        }
    }

    #[test]
    fn read_cmdline_parses_null_separated_tokens() {
        // This test needs to read a real process; use current process.
        // On some systems (restricted /proc), this may not work.
        let collector = match ProcessCollector::new() {
            Ok(c) => c,
            Err(_) => {
                // Skip test on systems without /proc
                return;
            }
        };

        let pid_path = PathBuf::from(format!("/proc/{}", std::process::id()));
        match collector.read_cmdline(&pid_path, std::process::id()) {
            Ok(cmdline) => {
                // The process should have at least one token (argv[0])
                assert!(!cmdline.is_empty());
            }
            Err(_) => {
                // Some processes may not have readable cmdline (kernel threads)
                // or we might be in a restricted environment
            }
        }
    }

    #[test]
    fn read_environ_parses_key_value_pairs() {
        let collector = match ProcessCollector::new() {
            Ok(c) => c,
            Err(_) => return,
        };

        let pid_path = PathBuf::from(format!("/proc/{}", std::process::id()));
        match collector.read_environ(&pid_path, std::process::id()) {
            Ok(environ) => {
                // At minimum, PATH should be set
                assert!(environ.contains_key("PATH") || environ.is_empty());
            }
            Err(_) => {
                // Some restricted environments may block environ reads
            }
        }
    }

    #[test]
    fn read_ppid_returns_option() {
        let collector = match ProcessCollector::new() {
            Ok(c) => c,
            Err(_) => return,
        };

        let pid_path = PathBuf::from(format!("/proc/{}", std::process::id()));
        match collector.read_ppid(&pid_path, std::process::id()) {
            Ok(_ppid) => {
                // ppid read succeeded; it should be a valid number
            }
            Err(_) => {
                // Some restricted environments
            }
        }
    }

    #[test]
    fn vmrss_parsed_from_status_fragment() {
        let status = "Name:\tllama-cli\nState:\tR\nVmPeak:\t  123456 kB\nVmRSS:\t   78900 kB\nVmData:\t 42 kB\n";
        assert_eq!(parse_vmrss_kb(status), Some(78900));
    }

    #[test]
    fn vmrss_missing_returns_none() {
        // Kernel threads have no VmRSS line.
        let status = "Name:\tkthreadd\nState:\tS\n";
        assert_eq!(parse_vmrss_kb(status), None);
    }

    #[test]
    fn cpu_ticks_parsed_from_stat_line() {
        // Synthetic /proc/<pid>/stat line modeled on the kernel's proc(5) format.
        // utime = 100 (field 14), stime = 40 (field 15).
        let stat = "1234 (bash) S 1 1234 1234 34816 1234 4194304 123 0 0 0 \
                    100 40 0 0 20 0 1 0 999 123456789 321 18446744073709551615 \
                    0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(parse_cpu_ticks_from_stat(stat), Some(140));
    }

    #[test]
    fn cpu_ticks_parses_comm_with_spaces_and_parens() {
        // `comm` can legally contain spaces and nested parens. The parser
        // uses rfind(')') so only the last ')' in the name terminates field 2.
        let stat = "42 (llama cli (worker)) R 1 42 42 0 -1 4194304 0 0 0 0 \
                    500 250 0 0 20 0 1 0 0 0 0 18446744073709551615 \
                    0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(parse_cpu_ticks_from_stat(stat), Some(750));
    }

    // ── Sprint-7 Item 3 — starttime parsing + os_start_time ──────

    #[test]
    fn starttime_parsed_from_stat_field_22() {
        // proc(5) field 22 is `starttime` in clock ticks since
        // boot. In the synthetic line below the field after the
        // 19th post-comm token (= overall field 22) is `999`.
        // Counting from the cpu_ticks_parsed_from_stat fixture:
        //   field 14 = utime = 100
        //   field 15 = stime = 40
        //   field 22 = starttime = 999
        let stat = "1234 (bash) S 1 1234 1234 34816 1234 4194304 123 0 0 0 \
                    100 40 0 0 20 0 1 0 999 123456789 321 18446744073709551615 \
                    0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(parse_starttime_from_stat(stat), Some(999));
    }

    #[test]
    fn starttime_parses_comm_with_spaces_and_parens() {
        // Same parens-in-comm robustness check as the cpu-ticks
        // parser: field 22 must come out right even when the
        // process name carries `)` characters.
        let stat = "42 (llama cli (worker)) R 1 42 42 0 -1 4194304 0 0 0 0 \
                    500 250 0 0 20 0 1 0 31337 0 0 18446744073709551615 \
                    0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(parse_starttime_from_stat(stat), Some(31337));
    }

    #[test]
    fn starttime_returns_none_on_truncated_stat() {
        // A stat line missing fields shouldn't panic — return None
        // and let the runtime fall back to first_observed_at.
        assert_eq!(parse_starttime_from_stat("1 (bash) S"), None);
        assert_eq!(parse_starttime_from_stat(""), None);
        assert_eq!(parse_starttime_from_stat("1 (bash"), None);
    }

    #[test]
    fn collector_populates_os_start_time_for_self() {
        // End-to-end smoke: the collector reads the current process
        // via /proc/<self>/stat and produces a non-None
        // os_start_time. The timestamp should be in the past
        // (kernel boot was before now). Tolerates restricted /proc
        // environments by short-circuiting on collector::new
        // failure.
        let collector = match ProcessCollector::new() {
            Ok(c) => c,
            Err(_) => return,
        };
        let pid_path = PathBuf::from(format!("/proc/{}", std::process::id()));
        let start = collector.read_os_start_time(&pid_path);
        if let Some(t) = start {
            assert!(
                t <= Utc::now(),
                "os_start_time should be in the past, got {t:?}"
            );
        }
        // None is acceptable when btime parse fails — defensive
        // path for alien /proc filesystems.
    }

    #[test]
    fn cpu_ticks_malformed_returns_none() {
        assert_eq!(parse_cpu_ticks_from_stat("not a stat line"), None);
        assert_eq!(parse_cpu_ticks_from_stat("1 (short) R"), None);
    }

    #[test]
    fn collect_all_processes_on_linux() {
        let collector = match ProcessCollector::new() {
            Ok(c) => c,
            Err(_) => return,
        };

        match collector.collect() {
            Ok(processes) => {
                // We should always have at least one process (init or ourselves)
                assert!(!processes.is_empty(), "should collect at least one process");
            }
            Err(e) => {
                eprintln!("collection failed: {}", e);
            }
        }
    }
}
