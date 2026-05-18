use crate::model::{AICategory, ProcessSample};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

pub mod tracker;

#[derive(Debug, Clone, Error)]
pub enum LifecycleError {
    #[error("lifecycle tracking error: {0}")]
    TrackingError(String),
}

pub type LifecycleResult<T> = Result<T, LifecycleError>;

/// Rolling per-process resource stats. Accumulated once per tick via
/// `ProcessLifecycle::record_sample` so the run summary can report meaningful
/// averages and peaks without the runtime having to keep full sample history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceStats {
    pub cpu_sum_pct: f32,
    pub cpu_peak_pct: f32,
    pub rss_peak_bytes: u64,
    pub vram_peak_bytes: u64,
    pub sample_count: u32,
}

impl ResourceStats {
    pub fn record(&mut self, cpu_pct: f32, rss_bytes: u64, vram_bytes: Option<u64>) {
        self.cpu_sum_pct += cpu_pct.max(0.0);
        if cpu_pct > self.cpu_peak_pct {
            self.cpu_peak_pct = cpu_pct;
        }
        self.record_peaks(rss_bytes, vram_bytes);
        self.sample_count = self.sample_count.saturating_add(1);
    }

    /// B9 — peak-only update: refreshes RSS / VRAM peaks WITHOUT
    /// touching `cpu_sum_pct` / `cpu_peak_pct` / `sample_count`. Used
    /// on the cold-start tick where the runtime has no honest CPU
    /// reading yet but the memory readings are absolute and worth
    /// retaining for peak tracking.
    pub fn record_peaks(&mut self, rss_bytes: u64, vram_bytes: Option<u64>) {
        if rss_bytes > self.rss_peak_bytes {
            self.rss_peak_bytes = rss_bytes;
        }
        if let Some(vram) = vram_bytes
            && vram > self.vram_peak_bytes
        {
            self.vram_peak_bytes = vram;
        }
    }

    pub fn avg_cpu_pct(&self) -> f32 {
        if self.sample_count == 0 {
            0.0
        } else {
            self.cpu_sum_pct / self.sample_count as f32
        }
    }
}

/// Represents a single process's lifecycle: spawn, runtime, and termination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessLifecycle {
    pub pid: u32,
    pub name: String,
    pub spawn_time: DateTime<Utc>,
    pub exit_time: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub category: Option<AICategory>,
    /// Short display model name when the classifier resolved one; survives
    /// exit so the run summary can name *which* model the process hosted.
    pub model_name: Option<String>,
    #[serde(default)]
    pub resources: ResourceStats,
}

impl ProcessLifecycle {
    /// Create a new lifecycle record when a process is first observed.
    pub fn new(sample: &ProcessSample, category: Option<AICategory>) -> Self {
        Self {
            pid: sample.pid,
            name: sample.name.clone(),
            spawn_time: Utc::now(),
            exit_time: None,
            exit_code: None,
            signal: None,
            category,
            model_name: None,
            resources: ResourceStats::default(),
        }
    }

    /// Mark process as exited with optional exit code and signal.
    pub fn mark_exit(&mut self, exit_code: Option<i32>, signal: Option<i32>) {
        self.exit_time = Some(Utc::now());
        self.exit_code = exit_code;
        self.signal = signal;
    }

    /// Called once per tick by the runtime to fold this tick's readings into
    /// the rolling stats. Negative cpu_pct (clock skew) is treated as zero.
    pub fn record_sample(&mut self, cpu_pct: f32, rss_bytes: u64, vram_bytes: Option<u64>) {
        self.resources.record(cpu_pct, rss_bytes, vram_bytes);
    }

    /// B9 — cold-start variant: refresh RSS / VRAM peaks only, without
    /// touching the CPU rolling-average or the sample counter. See
    /// `ResourceStats::record_peaks` for the rationale.
    pub fn record_resource_peaks(&mut self, rss_bytes: u64, vram_bytes: Option<u64>) {
        self.resources.record_peaks(rss_bytes, vram_bytes);
    }

    pub fn set_model_name(&mut self, name: Option<String>) {
        // Only overwrite when we have new information. Once a process's model
        // has been resolved we trust it; subsequent ticks that lose the signal
        // (e.g. exec'd into a wrapper) shouldn't erase the known name.
        if name.is_some() {
            self.model_name = name;
        }
    }

    /// Check if process has exited.
    pub fn is_exited(&self) -> bool {
        self.exit_time.is_some()
    }

    /// Get uptime in seconds — `Some(secs)` only for exited
    /// lifecycles, `None` while the process is still running.
    ///
    /// B3 (Sprint-2 investigation) — pre-fix this returned a live
    /// `now() - spawn_time` value for non-exited lifecycles via
    /// `unwrap_or_else(Utc::now)`. The single in-tree caller
    /// (`LifecycleSummary::from_lifecycle`) always pre-checked
    /// `is_exited()` so the live branch was technically dead code,
    /// but a future caller that forgot the gate would silently get a
    /// live-incrementing duration leaking onto the post-mortem card.
    /// Returning `Option` makes the "must be exited" contract a
    /// type-level invariant rather than a documentation invariant.
    pub fn uptime_secs(&self) -> Option<i64> {
        let end_time = self.exit_time?;
        Some((end_time - self.spawn_time).num_seconds())
    }
}

/// Summary generated when a process exits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleSummary {
    pub pid: u32,
    pub name: String,
    pub category: Option<AICategory>,
    pub model_name: Option<String>,
    pub spawn_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub uptime_secs: i64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Resource footprint rolled up from per-tick readings. Defaults to zero
    /// when no resource samples were recorded (short-lived processes that
    /// appeared and vanished inside one tick).
    #[serde(default)]
    pub avg_cpu_pct: f32,
    #[serde(default)]
    pub peak_cpu_pct: f32,
    #[serde(default)]
    pub peak_rss_mb: u64,
    #[serde(default)]
    pub peak_vram_mb: u64,
    #[serde(default)]
    pub samples: u32,
}

impl LifecycleSummary {
    /// Create summary from a lifecycle record.
    pub fn from_lifecycle(lifecycle: &ProcessLifecycle) -> Option<Self> {
        // B3 — uptime_secs() now returns Option<i64> and is the
        // gating signal for "process has exited." The pre-fix code
        // separately checked `is_exited()` + `exit_time.map(...)` +
        // `lifecycle.uptime_secs()` (which had its own `unwrap_or_else
        // (Utc::now)` fallback that could leak live time). Collapsing
        // all three to one `?` makes the not-exited fast-path explicit.
        let uptime_secs = lifecycle.uptime_secs()?;
        let exit_time = lifecycle.exit_time?;
        Some(Self {
            pid: lifecycle.pid,
            name: lifecycle.name.clone(),
            category: lifecycle.category,
            model_name: lifecycle.model_name.clone(),
            spawn_time: lifecycle.spawn_time,
            exit_time,
            uptime_secs,
            exit_code: lifecycle.exit_code,
            signal: lifecycle.signal,
            avg_cpu_pct: lifecycle.resources.avg_cpu_pct(),
            peak_cpu_pct: lifecycle.resources.cpu_peak_pct,
            peak_rss_mb: lifecycle.resources.rss_peak_bytes / (1024 * 1024),
            peak_vram_mb: lifecycle.resources.vram_peak_bytes / (1024 * 1024),
            samples: lifecycle.resources.sample_count,
        })
    }

    /// Format summary as human-readable string.
    pub fn to_string_detailed(&self) -> String {
        let mut details = format!(
            "[{}] {} (PID {}): {}s uptime\n",
            if self.category.is_some() { "AI" } else { "STD" },
            self.name,
            self.pid,
            self.uptime_secs
        );

        if let Some(cat) = self.category {
            details.push_str(&format!("  Category: {:?}\n", cat));
        }

        if let Some(model) = &self.model_name {
            details.push_str(&format!("  Model: {}\n", model));
        }

        details.push_str(&format!(
            "  CPU: avg {:.1}% peak {:.1}% | RSS peak: {}M | VRAM peak: {}M | samples: {}\n",
            self.avg_cpu_pct, self.peak_cpu_pct, self.peak_rss_mb, self.peak_vram_mb, self.samples,
        ));

        if let Some(code) = self.exit_code {
            details.push_str(&format!("  Exit code: {}\n", code));
        }

        if let Some(sig) = self.signal {
            details.push_str(&format!("  Signal: {}\n", sig));
        }

        details
    }
}

/// Snapshot of all tracked process lifecycles at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleSnapshot {
    pub timestamp: DateTime<Utc>,
    /// Map of PID → ProcessLifecycle for all known processes.
    pub processes: HashMap<u32, ProcessLifecycle>,
    /// Recently exited processes (since last snapshot).
    pub recent_exits: Vec<LifecycleSummary>,
}

impl LifecycleSnapshot {
    /// Create empty snapshot at current time.
    pub fn new() -> Self {
        Self {
            timestamp: Utc::now(),
            processes: HashMap::new(),
            recent_exits: Vec::new(),
        }
    }

    /// Count active (non-exited) processes.
    pub fn active_count(&self) -> usize {
        self.processes.values().filter(|lc| !lc.is_exited()).count()
    }

    /// Count exited processes.
    pub fn exited_count(&self) -> usize {
        self.processes.values().filter(|lc| lc.is_exited()).count()
    }
}

impl Default for LifecycleSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_lifecycle_new() {
        let sample = ProcessSample {
            pid: 1234,
            ppid: Some(1),
            name: "test_proc".to_string(),
            cmdline: vec!["test_proc".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let lc = ProcessLifecycle::new(&sample, None);
        assert_eq!(lc.pid, 1234);
        assert_eq!(lc.name, "test_proc");
        assert!(!lc.is_exited());
        assert_eq!(lc.exit_code, None);
        assert_eq!(lc.signal, None);
    }

    #[test]
    fn process_lifecycle_mark_exit() {
        let sample = ProcessSample {
            pid: 1234,
            ppid: Some(1),
            name: "test_proc".to_string(),
            cmdline: vec!["test_proc".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let mut lc = ProcessLifecycle::new(&sample, Some(AICategory::Inference));
        assert!(!lc.is_exited());

        lc.mark_exit(Some(0), None);
        assert!(lc.is_exited());
        assert_eq!(lc.exit_code, Some(0));
        assert_eq!(lc.exit_time, Some(lc.exit_time.unwrap()));
    }

    #[test]
    fn uptime_secs_returns_none_for_non_exited_lifecycle() {
        // B3 defensive narrowing: a process that hasn't exited yet
        // must NOT report a live-incrementing duration. Returning
        // None forces every caller to handle the "not done yet" case
        // explicitly, eliminating the silent live-time leak that the
        // pre-fix `unwrap_or_else(Utc::now)` had as dead-but-loaded
        // code.
        let sample = ProcessSample {
            pid: 1234,
            ppid: Some(1),
            name: "test_proc".to_string(),
            cmdline: vec!["test_proc".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };
        let mut lc = ProcessLifecycle::new(&sample, None);
        assert_eq!(
            lc.uptime_secs(),
            None,
            "non-exited lifecycle must return None, never a live duration"
        );
        lc.mark_exit(Some(0), None);
        let uptime_after = lc.uptime_secs();
        assert!(
            uptime_after.is_some(),
            "post-exit lifecycle must return Some(secs)"
        );
        assert!(uptime_after.unwrap() >= 0);
    }

    #[test]
    fn lifecycle_summary_from_lifecycle() {
        let sample = ProcessSample {
            pid: 5678,
            ppid: Some(1),
            name: "ai_process".to_string(),
            cmdline: vec!["ai_process".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let mut lc = ProcessLifecycle::new(&sample, Some(AICategory::Training));
        let summary_before = LifecycleSummary::from_lifecycle(&lc);
        assert!(summary_before.is_none());

        lc.mark_exit(Some(0), None);
        let summary_after = LifecycleSummary::from_lifecycle(&lc);
        assert!(summary_after.is_some());

        let summary = summary_after.unwrap();
        assert_eq!(summary.pid, 5678);
        assert_eq!(summary.name, "ai_process");
        assert_eq!(summary.category, Some(AICategory::Training));
        assert_eq!(summary.exit_code, Some(0));
    }

    #[test]
    fn lifecycle_summary_format() {
        let sample = ProcessSample {
            pid: 9999,
            ppid: Some(1),
            name: "test".to_string(),
            cmdline: vec!["test".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let mut lc = ProcessLifecycle::new(&sample, Some(AICategory::Inference));
        lc.mark_exit(Some(1), Some(15)); // SIGTERM

        let summary = LifecycleSummary::from_lifecycle(&lc).unwrap();
        let formatted = summary.to_string_detailed();

        assert!(formatted.contains("test"));
        assert!(formatted.contains("9999"));
        assert!(formatted.contains("Inference"));
        assert!(formatted.contains("Exit code: 1"));
        assert!(formatted.contains("Signal: 15"));
    }

    #[test]
    fn lifecycle_snapshot_counts() {
        let mut snapshot = LifecycleSnapshot::new();

        let sample1 = ProcessSample {
            pid: 100,
            ppid: Some(1),
            name: "proc1".to_string(),
            cmdline: vec!["proc1".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let sample2 = ProcessSample {
            pid: 101,
            ppid: Some(1),
            name: "proc2".to_string(),
            cmdline: vec!["proc2".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let lc1 = ProcessLifecycle::new(&sample1, None);
        let mut lc2 = ProcessLifecycle::new(&sample2, None);
        lc2.mark_exit(Some(0), None);

        snapshot.processes.insert(100, lc1);
        snapshot.processes.insert(101, lc2);

        assert_eq!(snapshot.active_count(), 1);
        assert_eq!(snapshot.exited_count(), 1);
    }

    #[test]
    fn lifecycle_snapshot_recent_exits() {
        let mut snapshot = LifecycleSnapshot::new();

        let sample = ProcessSample {
            pid: 200,
            ppid: Some(1),
            name: "exited_proc".to_string(),
            cmdline: vec!["exited_proc".to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let mut lc = ProcessLifecycle::new(&sample, Some(AICategory::ModelDownload));
        lc.mark_exit(Some(0), None);

        if let Some(summary) = LifecycleSummary::from_lifecycle(&lc) {
            snapshot.recent_exits.push(summary);
        }

        assert_eq!(snapshot.recent_exits.len(), 1);
        assert_eq!(snapshot.recent_exits[0].pid, 200);
    }
}
