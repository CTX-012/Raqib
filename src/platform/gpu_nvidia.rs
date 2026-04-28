use crate::platform::{PlatformError, PlatformResult};
use nvml_wrapper::Nvml;
use serde::Serialize;
use std::collections::HashMap;

/// GPU device metrics snapshot (per-device).
#[derive(Debug, Clone, Serialize)]
pub struct GpuDeviceMetrics {
    /// Device index (0-based)
    pub device_id: u32,
    /// Device name (e.g., "NVIDIA A100")
    pub device_name: String,
    /// Total VRAM in bytes
    pub total_vram: u64,
    /// Used VRAM in bytes (all processes)
    pub used_vram: u64,
    /// Free VRAM in bytes
    pub free_vram: u64,
    /// Per-process GPU memory: pid → (process_name, memory_string)
    pub per_process_vram: HashMap<u32, (String, String)>,
}

impl GpuDeviceMetrics {
    /// Returns VRAM usage as a percentage (0.0 to 100.0).
    pub fn vram_usage_percent(&self) -> f64 {
        if self.total_vram == 0 {
            0.0
        } else {
            (self.used_vram as f64 / self.total_vram as f64) * 100.0
        }
    }
}

/// Collection of all GPU devices on the system.
#[derive(Debug, Clone, Serialize)]
pub struct GpuSnapshot {
    /// All GPU devices (empty on systems without NVIDIA GPU)
    pub devices: Vec<GpuDeviceMetrics>,
}

impl GpuSnapshot {
    /// Returns true if any GPU devices are available.
    pub fn has_gpu(&self) -> bool {
        !self.devices.is_empty()
    }

    /// Total VRAM across all devices.
    pub fn total_vram_all_devices(&self) -> u64 {
        self.devices.iter().map(|d| d.total_vram).sum()
    }

    /// Total used VRAM across all devices.
    pub fn used_vram_all_devices(&self) -> u64 {
        self.devices.iter().map(|d| d.used_vram).sum()
    }
}

/// GPU collector using NVIDIA Management Library (NVML).
pub struct GpuCollector {
    nvml: Option<Nvml>,
}

impl GpuCollector {
    /// Creates a new GPU collector.
    /// Returns Ok even on systems without NVIDIA GPU; check has_gpu() on snapshot.
    pub fn new() -> PlatformResult<Self> {
        let nvml = match Nvml::init() {
            Ok(n) => Some(n),
            Err(e) => {
                // NVML initialization can fail on systems without NVIDIA GPU or drivers
                // This is not an error condition; just log and continue
                tracing::debug!("NVML init failed: {}, GPU metrics unavailable", e);
                None
            }
        };

        Ok(GpuCollector { nvml })
    }

    /// Collects GPU metrics from all available devices.
    pub fn collect(&self) -> PlatformResult<GpuSnapshot> {
        let nvml = match &self.nvml {
            Some(n) => n,
            None => {
                // NVML unavailable; return empty snapshot
                return Ok(GpuSnapshot {
                    devices: Vec::new(),
                });
            }
        };

        let mut devices = Vec::new();

        let device_count = nvml
            .device_count()
            .map_err(|e| PlatformError::SysInfo(format!("cannot get GPU count: {}", e)))?;

        for device_id in 0..device_count {
            match self.read_device_metrics(nvml, device_id) {
                Ok(metrics) => devices.push(metrics),
                Err(e) => {
                    tracing::debug!(
                        device = device_id,
                        error = %e,
                        "skipped GPU device"
                    );
                }
            }
        }

        Ok(GpuSnapshot { devices })
    }

    /// Reads metrics for a single GPU device.
    fn read_device_metrics(&self, nvml: &Nvml, device_id: u32) -> PlatformResult<GpuDeviceMetrics> {
        let device = nvml.device_by_index(device_id).map_err(|e| {
            PlatformError::SysInfo(format!("cannot get GPU device {}: {}", device_id, e))
        })?;

        let device_name = device
            .name()
            .unwrap_or_else(|_| format!("GPU {}", device_id));

        let (total_vram, free_vram) = device
            .memory_info()
            .map_err(|e| {
                PlatformError::SysInfo(format!(
                    "cannot get VRAM info for device {}: {}",
                    device_id, e
                ))
            })
            .map(|info| (info.total, info.free))?;

        let used_vram = total_vram - free_vram;

        // Collect per-process VRAM usage
        let mut per_process_vram = HashMap::new();

        // Note: NVML per-process memory tracking requires elevated privileges
        // on most systems. We attempt to collect but don't fail if unavailable.
        match device.running_graphics_processes() {
            Ok(processes) => {
                for process in processes {
                    let name = format!("pid_{}", process.pid);
                    // Store memory as string (format varies by NVML version)
                    let memory_str = format!("{:?}", process.used_gpu_memory);
                    per_process_vram.insert(process.pid, (name, memory_str));
                }
            }
            Err(e) => {
                tracing::debug!(
                    device = device_id,
                    error = %e,
                    "cannot read per-process GPU memory (may require elevated privileges)"
                );
            }
        }

        Ok(GpuDeviceMetrics {
            device_id,
            device_name,
            total_vram,
            used_vram,
            free_vram,
            per_process_vram,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_gpu_collector_always_succeeds() {
        // Should not error even without NVIDIA GPU
        let result = GpuCollector::new();
        assert!(result.is_ok());
    }

    #[test]
    fn collect_gpu_metrics_returns_snapshot() {
        let collector = match GpuCollector::new() {
            Ok(c) => c,
            Err(_) => return, // Skip on systems without GPU support
        };

        let result = collector.collect();
        assert!(result.is_ok());
        let snapshot = result.unwrap();

        // On systems without NVIDIA GPU, snapshot should be empty but valid
        if !snapshot.has_gpu() {
            assert!(snapshot.devices.is_empty());
        }
    }

    #[test]
    fn gpu_device_metrics_vram_percent() {
        let metrics = GpuDeviceMetrics {
            device_id: 0,
            device_name: "NVIDIA A100".to_string(),
            total_vram: 80 * 1024 * 1024 * 1024, // 80 GB
            used_vram: 40 * 1024 * 1024 * 1024,  // 40 GB
            free_vram: 40 * 1024 * 1024 * 1024,  // 40 GB
            per_process_vram: HashMap::new(),
        };

        assert_eq!(metrics.vram_usage_percent(), 50.0);
    }

    #[test]
    fn gpu_device_metrics_vram_percent_zero_total() {
        let metrics = GpuDeviceMetrics {
            device_id: 0,
            device_name: "NVIDIA A100".to_string(),
            total_vram: 0,
            used_vram: 0,
            free_vram: 0,
            per_process_vram: HashMap::new(),
        };

        assert_eq!(metrics.vram_usage_percent(), 0.0);
    }

    #[test]
    fn gpu_snapshot_aggregates_metrics() {
        let device1 = GpuDeviceMetrics {
            device_id: 0,
            device_name: "GPU0".to_string(),
            total_vram: 80 * 1024 * 1024 * 1024,
            used_vram: 40 * 1024 * 1024 * 1024,
            free_vram: 40 * 1024 * 1024 * 1024,
            per_process_vram: HashMap::new(),
        };

        let device2 = GpuDeviceMetrics {
            device_id: 1,
            device_name: "GPU1".to_string(),
            total_vram: 80 * 1024 * 1024 * 1024,
            used_vram: 20 * 1024 * 1024 * 1024,
            free_vram: 60 * 1024 * 1024 * 1024,
            per_process_vram: HashMap::new(),
        };

        let snapshot = GpuSnapshot {
            devices: vec![device1, device2],
        };

        assert!(snapshot.has_gpu());
        assert_eq!(snapshot.total_vram_all_devices(), 160 * 1024 * 1024 * 1024);
        assert_eq!(snapshot.used_vram_all_devices(), 60 * 1024 * 1024 * 1024);
    }

    #[test]
    fn empty_gpu_snapshot_has_no_gpu() {
        let snapshot = GpuSnapshot {
            devices: Vec::new(),
        };

        assert!(!snapshot.has_gpu());
        assert_eq!(snapshot.total_vram_all_devices(), 0);
        assert_eq!(snapshot.used_vram_all_devices(), 0);
    }
}
