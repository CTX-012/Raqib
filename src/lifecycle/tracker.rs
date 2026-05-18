use crate::classifier;
use crate::lifecycle::{LifecycleResult, LifecycleSnapshot, ProcessLifecycle};
use crate::model::ProcessSample;
use std::collections::HashMap;

/// Tracks process lifecycles across platform snapshots.
/// Detects new processes (spawns) and missing processes (exits).
pub struct LifecycleTracker {
    /// Previous snapshot of known PIDs and their lifecycles.
    previous: HashMap<u32, ProcessLifecycle>,
}

impl LifecycleTracker {
    /// Create new tracker with no history.
    pub fn new() -> Self {
        Self {
            previous: HashMap::new(),
        }
    }

    /// Process a current process list and generate lifecycle snapshot.
    /// Detects new processes (spawns) and missing processes (exits).
    pub fn update(
        &mut self,
        current_processes: &[ProcessSample],
    ) -> LifecycleResult<LifecycleSnapshot> {
        let mut snapshot = LifecycleSnapshot::new();
        let current_pids: std::collections::HashSet<u32> =
            current_processes.iter().map(|p| p.pid).collect();

        // Process all currently running processes
        for sample in current_processes {
            let lifecycle = if let Some(existing) = self.previous.remove(&sample.pid) {
                // PID was already tracked, but check if it previously exited
                if existing.is_exited() {
                    // PID was reused after process exit - treat as new process
                    let category = classifier::classify_process(sample).category_if_ai();
                    ProcessLifecycle::new(sample, category)
                } else {
                    // Process still running
                    existing
                }
            } else {
                // New process detected (spawn event)
                let category = classifier::classify_process(sample).category_if_ai();
                ProcessLifecycle::new(sample, category)
            };

            snapshot.processes.insert(sample.pid, lifecycle);
        }

        // Detect exited processes (PIDs no longer in current list)
        for (pid, mut lifecycle) in self.previous.drain() {
            if !current_pids.contains(&pid) && !lifecycle.is_exited() {
                // Process has exited
                lifecycle.mark_exit(None, None); // We don't have the actual exit code

                if let Some(summary) =
                    crate::lifecycle::LifecycleSummary::from_lifecycle(&lifecycle)
                {
                    snapshot.recent_exits.push(summary);
                }

                snapshot.processes.insert(pid, lifecycle);
            }
        }

        // Update state for next call
        self.previous = snapshot.processes.clone();

        Ok(snapshot)
    }

    /// Get count of currently tracked processes.
    pub fn tracked_count(&self) -> usize {
        self.previous.len()
    }

    /// Fold a per-tick resource reading into the tracked lifecycle for
    /// `pid`. Silently no-ops when the process has already exited or is
    /// unknown — callers iterate whatever the runtime observed, so we don't
    /// want to force them to pre-filter.
    pub fn record_sample(
        &mut self,
        pid: u32,
        cpu_pct: f32,
        rss_bytes: u64,
        vram_bytes: Option<u64>,
    ) {
        if let Some(lc) = self.previous.get_mut(&pid)
            && !lc.is_exited()
        {
            lc.record_sample(cpu_pct, rss_bytes, vram_bytes);
        }
    }

    /// B9 — cold-start variant of [`Self::record_sample`]: update RSS /
    /// VRAM peaks but DO NOT push a CPU sample into the rolling average.
    /// Called on the first tick a PID is observed, when
    /// `compute_cpu_pct` has no previous reading to delta against. The
    /// memory readings are absolute (not deltas) so the first-tick
    /// values are honest and contribute to peak tracking; the CPU
    /// reading is fabricated (0.0) and would otherwise pollute the
    /// per-run avg_cpu_pct, especially for short-lived AI processes.
    pub fn record_resource_peaks(
        &mut self,
        pid: u32,
        rss_bytes: u64,
        vram_bytes: Option<u64>,
    ) {
        if let Some(lc) = self.previous.get_mut(&pid)
            && !lc.is_exited()
        {
            lc.record_resource_peaks(rss_bytes, vram_bytes);
        }
    }

    /// Publish a known model-name for the process. Later calls with `None`
    /// do not clear it — the classifier can lose the signal mid-run (e.g.
    /// the script closed the file it loaded from), but the run summary
    /// should still name the model that was active at its peak.
    pub fn record_model_name(&mut self, pid: u32, name: Option<String>) {
        if let Some(lc) = self.previous.get_mut(&pid) {
            lc.set_model_name(name);
        }
    }
}

impl Default for LifecycleTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_sample(pid: u32, name: &str) -> ProcessSample {
        ProcessSample {
            pid,
            ppid: Some(1),
            name: name.to_string(),
            cmdline: vec![name.to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        }
    }

    #[test]
    fn lifecycle_tracker_new() {
        let tracker = LifecycleTracker::new();
        assert_eq!(tracker.tracked_count(), 0);
    }

    #[test]
    fn lifecycle_tracker_detects_spawn() {
        let mut tracker = LifecycleTracker::new();

        let processes = vec![make_sample(100, "new_proc")];
        let snapshot = tracker.update(&processes).unwrap();

        assert_eq!(snapshot.active_count(), 1);
        assert!(snapshot.processes.contains_key(&100));
    }

    #[test]
    fn lifecycle_tracker_detects_exit() {
        let mut tracker = LifecycleTracker::new();

        // First snapshot: process exists
        let processes1 = vec![make_sample(100, "proc")];
        let snapshot1 = tracker.update(&processes1).unwrap();
        assert_eq!(snapshot1.active_count(), 1);
        assert!(snapshot1.recent_exits.is_empty());

        // Second snapshot: process is gone
        let processes2 = vec![];
        let snapshot2 = tracker.update(&processes2).unwrap();
        assert_eq!(snapshot2.active_count(), 0);
        assert_eq!(snapshot2.recent_exits.len(), 1);
        assert_eq!(snapshot2.recent_exits[0].pid, 100);
    }

    #[test]
    fn lifecycle_tracker_multiple_processes() {
        let mut tracker = LifecycleTracker::new();

        // Snapshot 1: 3 processes
        let processes1 = vec![
            make_sample(100, "proc1"),
            make_sample(101, "proc2"),
            make_sample(102, "proc3"),
        ];
        let snapshot1 = tracker.update(&processes1).unwrap();
        assert_eq!(snapshot1.active_count(), 3);

        // Snapshot 2: 2 of them still running, 1 new, 1 exited
        let processes2 = vec![
            make_sample(100, "proc1"),
            make_sample(102, "proc3"),
            make_sample(103, "proc4"),
        ];
        let snapshot2 = tracker.update(&processes2).unwrap();
        assert_eq!(snapshot2.active_count(), 3);
        assert_eq!(snapshot2.recent_exits.len(), 1);
        assert_eq!(snapshot2.recent_exits[0].pid, 101);
    }

    #[test]
    fn lifecycle_tracker_rapid_churn() {
        let mut tracker = LifecycleTracker::new();

        // Snapshot 1: PIDs 100-110
        let processes1: Vec<_> = (100..=110).map(|pid| make_sample(pid, "proc")).collect();
        let snapshot1 = tracker.update(&processes1).unwrap();
        assert_eq!(snapshot1.active_count(), 11);

        // Snapshot 2: PIDs 105-120 (some exits, some new)
        let processes2: Vec<_> = (105..=120).map(|pid| make_sample(pid, "proc")).collect();
        let snapshot2 = tracker.update(&processes2).unwrap();
        assert_eq!(snapshot2.active_count(), 16);
        assert_eq!(snapshot2.recent_exits.len(), 5); // PIDs 100-104 exited
    }

    #[test]
    fn lifecycle_tracker_persistence_across_updates() {
        let mut tracker = LifecycleTracker::new();

        // First update
        let processes1 = vec![make_sample(100, "proc")];
        tracker.update(&processes1).unwrap();
        assert_eq!(tracker.tracked_count(), 1);

        // Second update (same process)
        let processes2 = vec![make_sample(100, "proc")];
        tracker.update(&processes2).unwrap();
        assert_eq!(tracker.tracked_count(), 1);

        // Process should still be marked as active, not exited
        let snapshot = tracker.update(&processes2).unwrap();
        assert_eq!(snapshot.recent_exits.len(), 0);
    }

    #[test]
    fn lifecycle_tracker_accumulates_resource_stats_into_summary() {
        // Spawn → two sample ticks with growing CPU/RSS/VRAM → exit tick.
        // Summary must see peaks at the highest observed values and the
        // correct sample count.
        let mut tracker = LifecycleTracker::new();
        let procs = vec![make_sample(500, "greedy")];
        tracker.update(&procs).unwrap();

        tracker.record_sample(500, 10.0, 100 * 1024 * 1024, Some(256 * 1024 * 1024));
        tracker.record_sample(500, 42.0, 400 * 1024 * 1024, Some(1024 * 1024 * 1024));

        // Tick with empty process list: 500 has exited.
        let snapshot = tracker.update(&[]).unwrap();
        assert_eq!(snapshot.recent_exits.len(), 1);
        let summary = &snapshot.recent_exits[0];
        assert_eq!(summary.pid, 500);
        assert_eq!(summary.samples, 2);
        assert!((summary.peak_cpu_pct - 42.0).abs() < 1e-6);
        assert!((summary.avg_cpu_pct - 26.0).abs() < 1e-6);
        assert_eq!(summary.peak_rss_mb, 400);
        assert_eq!(summary.peak_vram_mb, 1024);
    }

    // ── B9 (Sprint-2 investigation) — cold-start sample skip ───────
    //
    // The pre-fix runtime called `record_sample(pid, 0.0, …)` on the
    // first tick a PID was observed (compute_cpu_pct returned 0.0 for
    // cold start). For short-lived processes that 0.0 dominated
    // avg_cpu_pct — on-disk records routinely had `samples=1,
    // avg_cpu_pct=0.0`. The fix introduces `record_resource_peaks` so
    // the cold-start tick still updates RSS/VRAM peaks but does NOT
    // push a CPU sample.

    #[test]
    fn cold_start_tick_does_not_record_cpu_sample() {
        // Simulate a process where ONLY a cold-start "peaks-only"
        // update fires — no honest CPU sample. The resulting summary
        // must report `samples=0` and `avg_cpu_pct=0.0` from the
        // default branch, NOT samples=1 / avg=0 driven by a recorded
        // 0.0.
        let mut tracker = LifecycleTracker::new();
        tracker.update(&[make_sample(700, "shortlived")]).unwrap();
        tracker.record_resource_peaks(700, 50 * 1024 * 1024, Some(128 * 1024 * 1024));
        let snapshot = tracker.update(&[]).unwrap();
        let summary = &snapshot.recent_exits[0];
        assert_eq!(
            summary.samples, 0,
            "cold-start peaks-only update must not increment sample_count"
        );
        assert_eq!(summary.avg_cpu_pct, 0.0);
        // But peaks DID update — memory readings are absolute, not deltas.
        assert_eq!(summary.peak_rss_mb, 50);
        assert_eq!(summary.peak_vram_mb, 128);
    }

    #[test]
    fn two_tick_process_records_one_sample_at_real_value() {
        // Tick 1: cold start → record_resource_peaks (peaks-only).
        // Tick 2: real CPU reading → record_sample at the real value.
        // Pre-fix: avg_cpu_pct = (0.0 + 100.0) / 2 = 50.0 (half-truth).
        // Post-fix: avg_cpu_pct = 100.0 (just the honest sample).
        let mut tracker = LifecycleTracker::new();
        tracker.update(&[make_sample(701, "burner")]).unwrap();
        tracker.record_resource_peaks(701, 10 * 1024 * 1024, None);
        tracker.record_sample(701, 100.0, 20 * 1024 * 1024, None);
        let snapshot = tracker.update(&[]).unwrap();
        let summary = &snapshot.recent_exits[0];
        assert_eq!(summary.samples, 1);
        assert!(
            (summary.avg_cpu_pct - 100.0).abs() < 1e-6,
            "two-tick process must report the honest avg, not half of it; got {}",
            summary.avg_cpu_pct,
        );
        assert!((summary.peak_cpu_pct - 100.0).abs() < 1e-6);
        // Peaks across both ticks (tick-1 update bumped RSS to 10MB
        // before tick-2's 20MB beat it).
        assert_eq!(summary.peak_rss_mb, 20);
    }

    #[test]
    fn lifecycle_tracker_propagates_model_name_into_summary() {
        let mut tracker = LifecycleTracker::new();
        tracker.update(&[make_sample(600, "python3")]).unwrap();
        tracker.record_model_name(600, Some("yolov8n".into()));
        let snapshot = tracker.update(&[]).unwrap();
        assert_eq!(
            snapshot.recent_exits[0].model_name.as_deref(),
            Some("yolov8n")
        );
    }

    #[test]
    fn lifecycle_tracker_reuse_pid() {
        let mut tracker = LifecycleTracker::new();

        // Snapshot 1: PID 100 exists
        let processes1 = vec![make_sample(100, "proc_v1")];
        let snapshot1 = tracker.update(&processes1).unwrap();
        assert_eq!(snapshot1.active_count(), 1);

        // Snapshot 2: PID 100 gone
        let processes2 = vec![];
        let snapshot2 = tracker.update(&processes2).unwrap();
        assert_eq!(snapshot2.recent_exits.len(), 1);

        // Snapshot 3: PID 100 reused (new process with same PID)
        let processes3 = vec![make_sample(100, "proc_v2")];
        let snapshot3 = tracker.update(&processes3).unwrap();
        assert_eq!(snapshot3.active_count(), 1);
        // Previous PID 100 should not reappear in exits
        assert_eq!(snapshot3.recent_exits.len(), 0);
    }
}
