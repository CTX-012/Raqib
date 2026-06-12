mod gpu_nvidia;
pub mod host_vitals;
mod linux_proc;

pub use gpu_nvidia::{GpuCollector, GpuDeviceMetrics, GpuSnapshot};
pub use linux_proc::ProcessCollector;

use crate::model::ProcessSample;
use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;

/// Errors produced by the platform layer when collecting process metrics.
#[derive(Error, Debug)]
pub enum PlatformError {
    #[error("failed to read /proc/<pid>/cmdline: {0}")]
    CmdlineRead(String),

    #[error("failed to read /proc/<pid>/environ: {0}")]
    EnvironRead(String),

    #[error("failed to read /proc/<pid>/status: {0}")]
    StatusRead(String),

    #[error("failed to parse /proc/<pid>/stat: {0}")]
    StatParse(String),

    #[error("failed to read /proc directory: {0}")]
    ProcDirRead(String),

    #[error("sysinfo error: {0}")]
    SysInfo(String),

    /// v1.1.10 ITEM 2 — the process is a zombie (`/proc/<pid>/stat`
    /// State == 'Z'): exited but not yet reaped by its parent. The
    /// process-collector loop matches on this variant explicitly and
    /// silently filters the PID out — zombies are not live workloads
    /// and surfacing them as rows produces the v1.1.5/v1.1.6 ghost-
    /// row pattern Inspector flagged in DISPATCH 31. Carries the PID
    /// for any downstream telemetry that wants to count filtered
    /// zombies (none currently).
    #[error("pid {0} is a zombie (filtered)")]
    ZombieFiltered(u32),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type PlatformResult<T> = Result<T, PlatformError>;

/// System-level metrics: CPU, memory, network I/O, uptime.
/// Snapshot at a single point in time.
#[derive(Debug, Clone, Serialize)]
pub struct SystemMetrics {
    /// Timestamp when this snapshot was taken.
    pub timestamp: DateTime<Utc>,
    /// Total memory available, in bytes.
    pub total_memory: u64,
    /// Used memory, in bytes.
    pub used_memory: u64,
    /// Available memory, in bytes.
    pub available_memory: u64,
    /// Number of CPUs.
    pub cpu_count: usize,
    /// Per-CPU load averages (1, 5, 15 min).
    /// Collected from /proc/loadavg.
    pub load_average: [f64; 3],
}

impl SystemMetrics {
    /// Returns memory usage as a percentage (0.0 to 100.0).
    pub fn memory_usage_percent(&self) -> f64 {
        if self.total_memory == 0 {
            0.0
        } else {
            (self.used_memory as f64 / self.total_memory as f64) * 100.0
        }
    }
}

/// Complete platform snapshot: system + processes + GPU metrics +
/// host-level vitals (v1.1.12 / CAR-22 — thermal zones).
///
/// v1.1.12 / DISPATCH 39: the `Serialize` derive was DROPPED because
/// (a) `ux_contract::host_vitals::HostVitals` is intentionally
/// zero-dep (no `serde` derives, same stance as
/// `ux_contract::activity::ActivityState`), and (b) `grep -rn
/// 'serde_json.*PlatformSnapshot\|serialize.*PlatformSnapshot' src/`
/// confirms nothing in the codebase serializes `PlatformSnapshot`
/// directly — the web wire serializes `WireSnapshot` (built from
/// `PlatformSnapshot` via `WireSnapshot::from_runtime_state`) and
/// `tracing` log emission uses `Debug` rather than `Serialize`.
/// Inspector DISPATCH 38 Q1 → option (a) "drop the derive" cleanly
/// applies; no serde shim needed.
#[derive(Debug, Clone)]
pub struct PlatformSnapshot {
    pub timestamp: DateTime<Utc>,
    pub system: SystemMetrics,
    pub processes: Vec<ProcessSample>,
    pub gpu: GpuSnapshot,
    /// v1.1.12 / CAR-22 — host-level vitals (per-zone thermal
    /// readings). Empty `thermal_zones` means "no zones discovered"
    /// per the contract; consumers hide the panel rather than
    /// rendering an empty section. Per-zone read errors are
    /// silently skipped inside `host_vitals::collect_host_vitals`
    /// — the snapshot itself never fails because of thermal.
    pub vitals: ux_contract::host_vitals::HostVitals,
}

/// Collects all running processes and system metrics from the platform.
/// Implements the main data collection loop for Module 2.
///
/// v1.1.8 ITEM 2 (DISPATCH 25) — takes a long-lived
/// `&mut sysinfo::System` owned by the runtime. Pre-v1.1.8 this
/// function built a fresh `System::new_all()` per tick + called
/// `refresh_all()`, which on Linux scans every PID in /proc and
/// allocates a whole `ProcessSample` per process AND a global
/// CPU usage update — both wasted (the platform layer's
/// `linux_proc::ProcessCollector` is the actual process source,
/// and we only read memory fields off the System anyway). The
/// long-lived `System` plus a targeted `sys.refresh_memory()`
/// inside `collect_system_metrics` eliminates that wasted work
/// (~22.6M alloc calls in 90s under the 10× ROS2-publisher
/// workload per DISPATCH 22 PHASE 0 → ~0).
pub fn collect_snapshot(sys: &mut sysinfo::System) -> PlatformResult<PlatformSnapshot> {
    let timestamp = Utc::now();
    let system = collect_system_metrics(sys)?;
    let processes = collect_all_processes()?;
    let gpu = collect_gpu_metrics()?;
    // v1.1.12 / CAR-22 — host-level vitals (thermal zones from
    // /sys/class/thermal/). One call, host-level, NOT in any per-PID
    // loop. Per-zone errors degrade silently inside
    // `collect_host_vitals` so a thermal read failure can't fail the
    // whole snapshot. INA3221 power deferred per DISPATCH 39 scope.
    let vitals = host_vitals::collect_host_vitals();

    Ok(PlatformSnapshot {
        timestamp,
        system,
        processes,
        gpu,
        vitals,
    })
}

/// v1.1.8 ITEM 2 — long-lived `sysinfo::System` constructor for
/// callers that need the targeted-refresh shape (only the memory
/// fields, no per-tick process map). Used by `Runtime::new()` and
/// the `collect_snapshot_succeeds` regression test.
pub fn new_system_for_metrics() -> sysinfo::System {
    use sysinfo::{MemoryRefreshKind, RefreshKind};
    sysinfo::System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
    )
}

/// Collects all running processes from the platform.
pub fn collect_all_processes() -> PlatformResult<Vec<ProcessSample>> {
    let collector = ProcessCollector::new()?;
    collector.collect()
}

/// Collects GPU metrics from all available NVIDIA devices.
pub fn collect_gpu_metrics() -> PlatformResult<GpuSnapshot> {
    let collector = GpuCollector::new()?;
    collector.collect()
}

/// Collects system-level metrics using a long-lived sysinfo
/// [`sysinfo::System`].
///
/// v1.1.8 ITEM 2 (DISPATCH 25):
///  - The caller (runtime tick or test) supplies the System; we
///    only refresh the memory fields per tick (`refresh_memory`),
///    NOT the process map (`linux_proc::ProcessCollector` owns
///    that) or CPU usage (we don't read it here).
///  - `cpu_count` is read from `std::thread::available_parallelism`
///    instead of `sys.cpus().len()`: the latter returns 0 unless
///    `refresh_cpu_*` has been called (and the cpu list is only
///    constructed on that refresh), and we don't want to pay for
///    a CPU-usage refresh just to count cores. `available_parallelism`
///    is the std-library answer to "how many logical CPUs can this
///    process use"; it falls back to 1 if the platform query fails.
fn collect_system_metrics(sys: &mut sysinfo::System) -> PlatformResult<SystemMetrics> {
    sys.refresh_memory();

    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let available_memory = sys.available_memory();
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    // Read load average from /proc/loadavg
    let load_average = read_load_average().unwrap_or([0.0, 0.0, 0.0]);

    Ok(SystemMetrics {
        timestamp: Utc::now(),
        total_memory,
        used_memory,
        available_memory,
        cpu_count,
        load_average,
    })
}

/// Reads load average from /proc/loadavg.
fn read_load_average() -> PlatformResult<[f64; 3]> {
    use std::fs;

    let content = fs::read_to_string("/proc/loadavg")
        .map_err(|e| PlatformError::SysInfo(format!("cannot read /proc/loadavg: {}", e)))?;

    let parts: Vec<&str> = content.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(PlatformError::SysInfo(
            "invalid /proc/loadavg format".to_string(),
        ));
    }

    let load_1 = parts[0]
        .parse::<f64>()
        .map_err(|_| PlatformError::SysInfo("cannot parse load 1".to_string()))?;
    let load_5 = parts[1]
        .parse::<f64>()
        .map_err(|_| PlatformError::SysInfo("cannot parse load 5".to_string()))?;
    let load_15 = parts[2]
        .parse::<f64>()
        .map_err(|_| PlatformError::SysInfo("cannot parse load 15".to_string()))?;

    Ok([load_1, load_5, load_15])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_snapshot_succeeds() {
        // v1.1.8 ITEM 2 — caller owns the long-lived System.
        let mut sys = new_system_for_metrics();
        let result = collect_snapshot(&mut sys);
        match result {
            Ok(snapshot) => {
                assert!(!snapshot.processes.is_empty());
                assert!(snapshot.system.total_memory > 0);
                // GPU snapshot may be empty on systems without NVIDIA GPU
            }
            Err(e) => {
                eprintln!("Platform collection not available: {}", e);
            }
        }
    }

    #[test]
    fn collect_system_metrics_succeeds() {
        let mut sys = new_system_for_metrics();
        let result = collect_system_metrics(&mut sys);
        match result {
            Ok(metrics) => {
                assert!(metrics.total_memory > 0);
                assert!(metrics.cpu_count > 0);
            }
            Err(e) => {
                eprintln!("System metrics collection failed: {}", e);
            }
        }
    }

    /// v1.1.8 ITEM 2 (DISPATCH 25) — the targeted-refresh shape
    /// must populate the memory fields. Pre-fix the function called
    /// `sys.refresh_all()` on a fresh `System::new_all()` every
    /// tick; post-fix it calls `sys.refresh_memory()` on a
    /// long-lived `System` built via `new_system_for_metrics`.
    /// Pin that the targeted shape still returns non-zero memory
    /// figures on the test host (which has memory).
    #[test]
    fn collect_system_metrics_with_targeted_refresh_populates_memory() {
        let mut sys = new_system_for_metrics();
        // Two consecutive refreshes via the same long-lived System
        // — proves the per-tick reuse pattern works.
        let a = collect_system_metrics(&mut sys).expect("first refresh");
        let b = collect_system_metrics(&mut sys).expect("second refresh");
        assert!(a.total_memory > 0, "total_memory must be populated");
        assert!(a.cpu_count > 0, "cpu_count from available_parallelism must be > 0");
        assert_eq!(
            a.total_memory, b.total_memory,
            "total_memory is hardware-fixed; consecutive refreshes \
             on the same System must agree",
        );
    }

    #[test]
    fn collect_gpu_metrics_returns_snapshot() {
        let result = collect_gpu_metrics();
        match result {
            Ok(snapshot) => {
                // Should always succeed; GPU snapshot may be empty
                let _ = snapshot.has_gpu();
            }
            Err(e) => {
                eprintln!("GPU collection failed: {}", e);
            }
        }
    }

    #[test]
    fn system_metrics_memory_percent() {
        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            total_memory: 1000,
            used_memory: 500,
            available_memory: 500,
            cpu_count: 4,
            load_average: [1.0, 1.0, 1.0],
        };
        assert_eq!(metrics.memory_usage_percent(), 50.0);
    }

    #[test]
    fn system_metrics_memory_percent_zero_total() {
        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            total_memory: 0,
            used_memory: 0,
            available_memory: 0,
            cpu_count: 4,
            load_average: [1.0, 1.0, 1.0],
        };
        assert_eq!(metrics.memory_usage_percent(), 0.0);
    }
}
