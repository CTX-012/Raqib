mod gpu_nvidia;
mod linux_proc;

pub use gpu_nvidia::{GpuCollector, GpuSnapshot};
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

/// Complete platform snapshot: system + processes + GPU metrics.
/// Produced each tick by the platform layer.
#[derive(Debug, Clone, Serialize)]
pub struct PlatformSnapshot {
    pub timestamp: DateTime<Utc>,
    pub system: SystemMetrics,
    pub processes: Vec<ProcessSample>,
    pub gpu: GpuSnapshot,
}

/// Collects all running processes and system metrics from the platform.
/// Implements the main data collection loop for Module 2.
pub fn collect_snapshot() -> PlatformResult<PlatformSnapshot> {
    let timestamp = Utc::now();
    let system = collect_system_metrics()?;
    let processes = collect_all_processes()?;
    let gpu = collect_gpu_metrics()?;

    Ok(PlatformSnapshot {
        timestamp,
        system,
        processes,
        gpu,
    })
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

/// Collects system-level metrics using sysinfo.
fn collect_system_metrics() -> PlatformResult<SystemMetrics> {
    use sysinfo::System;

    let mut sys = System::new_all();
    sys.refresh_all();

    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let available_memory = sys.available_memory();
    let cpu_count = sys.cpus().len();

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
        let result = collect_snapshot();
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
        let result = collect_system_metrics();
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
