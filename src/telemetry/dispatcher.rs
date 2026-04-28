//! Telemetry dispatcher — drives [`TelemetrySource`]s against AI
//! processes on a Tokio runtime, collects frames into a
//! [`TelemetryAccumulator`], and projects per-PID metrics back onto
//! `RunRecord` at exit. Closes the loop opened by Foundation B.
//!
//! Concurrency model:
//!
//! * One Tokio multi-threaded runtime owned by the dispatcher (2
//!   worker threads — enough for HTTP + spawned local servers).
//! * Each [`TelemetrySource`] is wrapped in `Arc<tokio::sync::Mutex>`
//!   so multiple sample tasks for different PIDs serialise per-source
//!   (samplers cache state per PID; concurrent calls would race the
//!   `endpoint_cache` HashMaps).
//! * Frames flow back through an unbounded mpsc channel to the
//!   tick-loop thread, which drains them into the accumulator at the
//!   start of every tick.
//! * Sample tasks have an outer timeout (configurable, default 1s)
//!   so a hung HTTP scrape can't pile up forever.
//!
//! Crash isolation: a panicking sample task is caught by Tokio's
//! join error and logged once; the dispatcher keeps running. Spec'd
//! by Foundation B.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc};

use crate::platform::GpuSnapshot;
use crate::storage::run_store::{ColdStartStats, RunMetrics};
use crate::telemetry::accumulator::TelemetryAccumulator;
use crate::telemetry::cold_load::{ColdLoadTracker, read_bytes_for};
use crate::telemetry::exporter::{self, MetricsSnapshot, SnapshotHandle};
use crate::telemetry::rapl::RaplReader;
use crate::telemetry::source::{ProcessSnapshot, TelemetryFrame, TelemetrySource};

/// Default per-sample timeout. Long enough for a slow vLLM scrape
/// (default reqwest timeout is 500 ms) plus jitter; short enough that
/// a hung sampler doesn't fall many ticks behind.
const DEFAULT_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);

/// Aliases keep Clippy happy and document intent.
type SharedSource = Arc<Mutex<Box<dyn TelemetrySource>>>;

pub struct Dispatcher {
    sources: Vec<SharedSource>,
    runtime: tokio::runtime::Runtime,
    frame_tx: mpsc::UnboundedSender<TelemetryFrame>,
    frame_rx: mpsc::UnboundedReceiver<TelemetryFrame>,
    accumulator: TelemetryAccumulator,
    sample_timeout: Duration,
    /// Tier 2.1 — RAPL package-power reader. Stateful across ticks
    /// to compute Δ-based wattage.
    rapl: RaplReader,
    /// Tier 2.2 — cold-load disk I/O detector. Stateful per-PID.
    cold_load: ColdLoadTracker,
    /// Tier 2.3 — shared snapshot the Prometheus exporter reads.
    /// `None` when the exporter is not bound (default behaviour).
    exporter_snapshot: Option<SnapshotHandle>,
    /// Live exporter task handle (kept for clean shutdown). Aborted
    /// when the dispatcher drops.
    exporter_task: Option<tokio::task::JoinHandle<()>>,
}

impl Dispatcher {
    /// Build a dispatcher around the supplied sources. Returns an
    /// `io::Result` because spawning the Tokio runtime can fail when
    /// the kernel refuses to create worker threads (rare).
    pub fn new(sources: Vec<Box<dyn TelemetrySource>>) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("edge_monitor-telemetry")
            .enable_all()
            .build()?;
        let (frame_tx, frame_rx) = mpsc::unbounded_channel();
        Ok(Self {
            sources: sources
                .into_iter()
                .map(|s| Arc::new(Mutex::new(s)))
                .collect(),
            runtime,
            frame_tx,
            frame_rx,
            accumulator: TelemetryAccumulator::new(),
            sample_timeout: DEFAULT_SAMPLE_TIMEOUT,
            rapl: RaplReader::new(),
            cold_load: ColdLoadTracker::new(),
            exporter_snapshot: None,
            exporter_task: None,
        })
    }

    /// Tier 2.3 — start the Prometheus exporter on `bind` (e.g.
    /// `127.0.0.1:9472`). Empty string is a no-op. Returns Ok even on
    /// `bind` errors (the bind happens inside the spawned task and
    /// logs to tracing if it fails); this matches the rest of the
    /// runtime's "best-effort, never fatal" telemetry policy.
    pub fn enable_exporter(&mut self, bind: &str) -> std::io::Result<()> {
        if bind.is_empty() {
            return Ok(());
        }
        let snapshot = Arc::new(Mutex::new(MetricsSnapshot::new()));
        let handle = exporter::spawn(&self.runtime, bind, snapshot.clone())?;
        self.exporter_snapshot = Some(snapshot);
        self.exporter_task = handle;
        Ok(())
    }

    /// Update the exporter's shared snapshot. Called by the runtime
    /// after each tick; no-op when the exporter isn't bound.
    pub fn publish_metrics(&self, snap: MetricsSnapshot) {
        if let Some(handle) = &self.exporter_snapshot {
            // try_lock first to avoid blocking the tick loop on a
            // long-running scrape; if contended, just drop this
            // update — the next tick replaces it.
            if let Ok(mut guard) = handle.try_lock() {
                *guard = snap;
            }
        }
    }

    pub fn with_sample_timeout(mut self, t: Duration) -> Self {
        self.sample_timeout = t;
        self
    }

    /// Run one tick: drain completed frames into the accumulator,
    /// then schedule new samples for `processes`. Non-blocking;
    /// returns as soon as tasks are queued.
    pub fn tick(&mut self, processes: &[ProcessSnapshot]) {
        // 1. Drain already-completed frames.
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.accumulator.record(frame);
        }

        // 2. Queue new samples. We don't pre-filter with applies_to on
        //    the dispatcher thread because applies_to may inspect the
        //    sampler's mutable state (e.g. cached endpoint poisoning).
        //    The spawned task does the check after acquiring the lock.
        for proc in processes {
            for source in &self.sources {
                let source = source.clone();
                let proc = proc.clone();
                let tx = self.frame_tx.clone();
                let timeout = self.sample_timeout;
                self.runtime.spawn(async move {
                    let mut guard = source.lock().await;
                    if !guard.applies_to(&proc) {
                        return;
                    }
                    let name = guard.name().to_string();
                    let pid = proc.pid;
                    let fut = guard.sample(&proc);
                    match tokio::time::timeout(timeout, fut).await {
                        Ok(Ok(frame)) => {
                            let _ = tx.send(frame);
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(
                                sampler = %name,
                                pid = pid,
                                error = %e,
                                "telemetry sample failed"
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                sampler = %name,
                                pid = pid,
                                timeout_ms = timeout.as_millis() as u64,
                                "telemetry sample timed out"
                            );
                        }
                    }
                });
            }
        }
    }

    /// Tier 2.1 — synthesize per-PID power frames from system-level
    /// readings (NVML for GPU, RAPL for CPU package). Called once per
    /// runtime tick after `tick(processes)` so the per-PID accumulator
    /// gets a power reading for each AI process.
    ///
    /// **Attribution policy.** v1 divides total system watts by the
    /// count of AI processes — pragmatic but not honest if multiple
    /// AI workloads share the box. Caller decides whether to surface
    /// this or fall back to None on shared boxes. Tier 3.x will add
    /// VRAM-weighted attribution for GPU power.
    pub fn record_system_power(&mut self, processes: &[ProcessSnapshot], gpu: &GpuSnapshot) {
        if processes.is_empty() {
            return;
        }
        let n = processes.len() as f32;

        let gpu_watts_total: f32 = gpu
            .devices
            .iter()
            .filter_map(|d| d.power_watts)
            .filter(|w| w.is_finite() && *w > 0.0)
            .sum();
        let gpu_temp_max: Option<f32> = gpu
            .devices
            .iter()
            .filter_map(|d| d.temp_c)
            .fold(None, |acc, t| Some(acc.map_or(t, |a: f32| a.max(t))));
        let cpu_watts_total = self.rapl.read_watts();

        let per_proc_gpu = if gpu_watts_total > 0.0 {
            Some(gpu_watts_total / n)
        } else {
            None
        };
        let per_proc_cpu = cpu_watts_total.map(|w| w / n);

        // Skip if neither reading produced anything; saves the
        // accumulator a no-op record per process.
        if per_proc_gpu.is_none() && per_proc_cpu.is_none() && gpu_temp_max.is_none() {
            return;
        }

        for proc in processes {
            self.accumulator.record(TelemetryFrame {
                pid: proc.pid,
                gpu_watts: per_proc_gpu,
                gpu_temp_c: gpu_temp_max,
                cpu_watts: per_proc_cpu,
                ..TelemetryFrame::new(proc.pid)
            });
        }
    }

    /// Tier 2.2 — sample `/proc/<pid>/io` for each AI process and
    /// fold into the cold-load tracker. Caller passes the same
    /// `processes` slice as `tick`. Reads silently skip PIDs whose
    /// `/proc/<pid>/io` is unreadable (permission, race-with-exit).
    pub fn record_disk_io(&mut self, processes: &[ProcessSnapshot]) {
        for proc in processes {
            if let Some(read_bytes) = read_bytes_for(proc.pid) {
                let _ = self.cold_load.record(proc.pid, read_bytes);
            }
        }
    }

    /// Cold-load stats for `pid` if the load completed (or hit the
    /// hard timeout). `None` when still in progress or never started.
    pub fn cold_start_for(&self, pid: u32) -> Option<ColdStartStats> {
        self.cold_load.stats(pid)
    }

    /// `RunMetrics` rolled up from telemetry frames seen for `pid`.
    /// Returned `None` when no telemetry frames were ever recorded
    /// for that PID — caller should leave the metric fields on the
    /// `RunRecord` as `None` rather than zeros.
    pub fn metrics_for(&self, pid: u32) -> Option<RunMetrics> {
        self.accumulator.snapshot(pid)
    }

    /// Authoritative model-name observed via a runtime API
    /// (Tier 1.2c — Ollama). Caller can promote this onto
    /// `RunRecord.summary.model_name`.
    pub fn model_name_hint_for(&self, pid: u32) -> Option<String> {
        self.accumulator.model_name_hint_for(pid).map(String::from)
    }

    /// Drop accumulator state for `pid` after persisting the record.
    /// Prevents stale data leaking into a recycled PID.
    pub fn forget(&mut self, pid: u32) {
        self.accumulator.forget(pid);
        self.cold_load.forget(pid);
    }

    /// Read-only handle to the accumulator (UI may want this for the
    /// live registry panel).
    pub fn accumulator(&self) -> &TelemetryAccumulator {
        &self.accumulator
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        // Stop the exporter loop before the runtime tears down its
        // worker threads — abort() is async-task-cancel which Tokio
        // already cleans up on Runtime::drop, but explicit is
        // clearer than implicit.
        if let Some(task) = self.exporter_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::source::{
        ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration as StdDuration;

    fn snap(pid: u32) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: "x".into(),
            cmdline: vec!["x".into()],
            environ: HashMap::new(),
            model_name: None,
        }
    }

    struct AlwaysApplies {
        called: Arc<AtomicU32>,
        tps: f32,
    }
    #[async_trait]
    impl TelemetrySource for AlwaysApplies {
        fn name(&self) -> &str {
            "always"
        }
        fn applies_to(&self, _: &ProcessSnapshot) -> bool {
            true
        }
        async fn sample(&mut self, p: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
            self.called.fetch_add(1, Ordering::SeqCst);
            Ok(TelemetryFrame {
                pid: p.pid,
                tokens_per_sec: Some(self.tps),
                ..TelemetryFrame::new(p.pid)
            })
        }
    }

    struct NeverApplies;
    #[async_trait]
    impl TelemetrySource for NeverApplies {
        fn name(&self) -> &str {
            "never"
        }
        fn applies_to(&self, _: &ProcessSnapshot) -> bool {
            false
        }
        async fn sample(&mut self, p: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
            // Should never be called.
            panic!("NeverApplies::sample called for pid {}", p.pid);
        }
    }

    struct SlowSampler;
    #[async_trait]
    impl TelemetrySource for SlowSampler {
        fn name(&self) -> &str {
            "slow"
        }
        fn applies_to(&self, _: &ProcessSnapshot) -> bool {
            true
        }
        async fn sample(&mut self, _: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
            tokio::time::sleep(StdDuration::from_millis(500)).await;
            Err(SourceError::Transient(
                "never finishes within timeout".into(),
            ))
        }
    }

    struct PanickingSampler;
    #[async_trait]
    impl TelemetrySource for PanickingSampler {
        fn name(&self) -> &str {
            "panic"
        }
        fn applies_to(&self, _: &ProcessSnapshot) -> bool {
            true
        }
        async fn sample(&mut self, _: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
            panic!("intentional panic for crash-isolation test");
        }
    }

    /// Wait for a frame to land in the accumulator (helper for the
    /// async-into-sync seam — frames flow through a channel).
    fn wait_for_frame(d: &mut Dispatcher, pid: u32, timeout: StdDuration) -> Option<RunMetrics> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            d.tick(&[]); // drain pending frames
            if let Some(m) = d.metrics_for(pid) {
                return Some(m);
            }
            std::thread::sleep(StdDuration::from_millis(20));
        }
        None
    }

    #[test]
    fn dispatcher_records_frames_from_applicable_source() {
        let called = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(vec![Box::new(AlwaysApplies {
            called: called.clone(),
            tps: 42.0,
        })])
        .unwrap();
        d.tick(&[snap(1)]);
        let m = wait_for_frame(&mut d, 1, StdDuration::from_secs(2)).unwrap();
        assert!((m.tokens_per_sec_avg.unwrap() - 42.0).abs() < 1e-3);
        assert!(called.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn dispatcher_skips_non_applicable_source() {
        // NeverApplies::sample panics if called. If applies_to is
        // honoured, no panic surfaces. We schedule, drain, observe
        // no metrics for the PID.
        let mut d = Dispatcher::new(vec![Box::new(NeverApplies)]).unwrap();
        d.tick(&[snap(1)]);
        std::thread::sleep(StdDuration::from_millis(100));
        d.tick(&[]);
        assert!(d.metrics_for(1).is_none());
    }

    #[test]
    fn dispatcher_timeout_protects_against_slow_samplers() {
        let mut d = Dispatcher::new(vec![Box::new(SlowSampler)])
            .unwrap()
            .with_sample_timeout(StdDuration::from_millis(50));
        d.tick(&[snap(1)]);
        std::thread::sleep(StdDuration::from_millis(150));
        d.tick(&[]);
        // Timed out → no frame recorded.
        assert!(d.metrics_for(1).is_none());
    }

    #[test]
    fn dispatcher_survives_panicking_sampler() {
        // A panicking sampler must not bring down the runtime —
        // Tokio task-local panic + JoinError is caught by the
        // executor. Other (well-behaved) samplers continue working.
        let called = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(vec![
            Box::new(PanickingSampler),
            Box::new(AlwaysApplies {
                called: called.clone(),
                tps: 7.0,
            }),
        ])
        .unwrap();
        d.tick(&[snap(1)]);
        let m = wait_for_frame(&mut d, 1, StdDuration::from_secs(2)).unwrap();
        // The good sampler still produced a frame.
        assert!((m.tokens_per_sec_avg.unwrap() - 7.0).abs() < 1e-3);
    }

    #[test]
    fn forget_drops_per_pid_state() {
        let called = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(vec![Box::new(AlwaysApplies { called, tps: 1.0 })]).unwrap();
        d.tick(&[snap(1)]);
        wait_for_frame(&mut d, 1, StdDuration::from_secs(2)).unwrap();
        d.forget(1);
        assert!(d.metrics_for(1).is_none());
    }
}
