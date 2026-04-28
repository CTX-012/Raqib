//! Per-PID rolling accumulator. Folds sequential `TelemetryFrame`s
//! into the `RunMetrics` shape that `RunRecord` carries on exit.
//!
//! Single-writer: the dispatcher serialises updates per PID. The
//! accumulator itself holds no synchronisation primitive — keep it
//! cheap and inert.

use std::collections::HashMap;

use crate::storage::run_store::RunMetrics;
use crate::telemetry::source::TelemetryFrame;

/// Running aggregates for one PID. Reset on PID reuse (the dispatcher
/// notices a fresh spawn and instantiates a new accumulator).
#[derive(Debug, Clone, Default)]
pub struct PerPidStats {
    samples: u32,

    // Token throughput.
    tps_sum: f32,
    tps_peak: f32,
    tps_samples: u32,

    // Vision.
    fps_sum: f32,
    fps_samples: u32,
    latency_sum_ms: f32,
    latency_samples: u32,
    /// Raw per-frame latency values for percentile rollup at exit.
    /// Capped at 4096 to avoid unbounded growth — long-running servers
    /// hit a clean rolling window rather than OOM.
    latency_window: Vec<f32>,

    // KV cache.
    kv_pct_peak: f32,
    kv_pct_samples: u32,

    // Concurrent requests.
    concurrent_peak: u32,
}

impl PerPidStats {
    fn record(&mut self, f: &TelemetryFrame) {
        self.samples = self.samples.saturating_add(1);

        if let Some(tps) = f.tokens_per_sec
            && tps.is_finite()
        {
            self.tps_sum += tps;
            self.tps_samples += 1;
            if tps > self.tps_peak {
                self.tps_peak = tps;
            }
        }
        if let Some(fps) = f.fps
            && fps.is_finite()
        {
            self.fps_sum += fps;
            self.fps_samples += 1;
        }
        if let Some(lat) = f.latency_ms
            && lat.is_finite()
        {
            self.latency_sum_ms += lat;
            self.latency_samples += 1;
            if self.latency_window.len() < 4096 {
                self.latency_window.push(lat);
            }
        }
        if let Some(kv) = f.kv_cache_pct
            && kv > self.kv_pct_peak
        {
            self.kv_pct_peak = kv;
            self.kv_pct_samples += 1;
        }
        if let Some(c) = f.concurrent_requests
            && c > self.concurrent_peak
        {
            self.concurrent_peak = c;
        }
    }

    /// Project the accumulated stats onto the `RunMetrics` slots that
    /// `RunRecord::from_summary` leaves as `None` for non-telemetry
    /// runs. Caller merges this onto the record before persisting.
    pub fn to_run_metrics(&self) -> RunMetrics {
        RunMetrics {
            tokens_total: None, // Tier 2.x — vLLM `_total` counters.
            tokens_per_sec_avg: avg(self.tps_sum, self.tps_samples),
            tokens_per_sec_peak: opt_finite(self.tps_peak, self.tps_samples > 0),

            kv_cache_peak_pct: opt_finite(self.kv_pct_peak, self.kv_pct_samples > 0),
            concurrent_requests_peak: if self.concurrent_peak > 0 {
                Some(self.concurrent_peak)
            } else {
                None
            },

            frames_total: None,
            fps_avg: avg(self.fps_sum, self.fps_samples),
            inference_latency_ms_avg: avg(self.latency_sum_ms, self.latency_samples),
            inference_latency_ms_p99: percentile(&self.latency_window, 0.99),

            ..RunMetrics::default()
        }
    }
}

fn avg(sum: f32, n: u32) -> Option<f32> {
    if n == 0 { None } else { Some(sum / n as f32) }
}
fn opt_finite(v: f32, present: bool) -> Option<f32> {
    if present && v.is_finite() {
        Some(v)
    } else {
        None
    }
}
/// Standard "nearest-rank" percentile: `idx = floor(n * pct)`, clamped
/// to `[0, n-1]`. Surfaces the tail — for n=100, p=0.99 returns the
/// 100th-ranked value (last after sort), so a single 1000ms outlier in
/// a sea of 10ms latencies will pop in `inference_latency_ms_p99`,
/// which is what the metric is for.
fn percentile(values: &[f32], pct: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f32) * pct) as usize;
    let idx = idx.min(sorted.len() - 1);
    Some(sorted[idx])
}

/// Multi-PID telemetry accumulator. Owned by the dispatcher, queried
/// by the lifecycle layer at exit time.
#[derive(Debug, Clone, Default)]
pub struct TelemetryAccumulator {
    by_pid: HashMap<u32, PerPidStats>,
}

impl TelemetryAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one frame into the per-PID stats. The frame's `pid` field
    /// is the routing key; the dispatcher must set it correctly.
    pub fn record(&mut self, frame: TelemetryFrame) {
        self.by_pid.entry(frame.pid).or_default().record(&frame);
    }

    /// Snapshot of the accumulated stats for `pid`. Returns `None`
    /// when no frames were recorded — caller should leave the
    /// `RunRecord` metrics fields as `None` rather than synthesising
    /// zeros.
    pub fn snapshot(&self, pid: u32) -> Option<RunMetrics> {
        self.by_pid.get(&pid).map(|s| s.to_run_metrics())
    }

    /// Drop the per-PID stats. Called when a process exits and its
    /// metrics have been folded into the `RunRecord`.
    pub fn forget(&mut self, pid: u32) {
        self.by_pid.remove(&pid);
    }

    pub fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.by_pid.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::source::TelemetryFrame;

    fn frame(pid: u32, tps: Option<f32>) -> TelemetryFrame {
        TelemetryFrame {
            pid,
            tokens_per_sec: tps,
            ..TelemetryFrame::new(pid)
        }
    }

    #[test]
    fn empty_pid_returns_none() {
        let acc = TelemetryAccumulator::new();
        assert!(acc.snapshot(7).is_none());
    }

    #[test]
    fn average_and_peak_tokens_per_sec_are_correct() {
        let mut acc = TelemetryAccumulator::new();
        for tps in [10.0, 20.0, 30.0_f32] {
            acc.record(frame(1, Some(tps)));
        }
        let m = acc.snapshot(1).unwrap();
        assert_eq!(m.tokens_per_sec_avg, Some(20.0));
        assert_eq!(m.tokens_per_sec_peak, Some(30.0));
    }

    #[test]
    fn pids_do_not_cross_contaminate() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(frame(1, Some(10.0)));
        acc.record(frame(2, Some(99.0)));
        let m1 = acc.snapshot(1).unwrap();
        let m2 = acc.snapshot(2).unwrap();
        assert_eq!(m1.tokens_per_sec_avg, Some(10.0));
        assert_eq!(m2.tokens_per_sec_avg, Some(99.0));
    }

    #[test]
    fn forget_removes_stats() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(frame(1, Some(10.0)));
        assert!(acc.snapshot(1).is_some());
        acc.forget(1);
        assert!(acc.snapshot(1).is_none());
    }

    #[test]
    fn latency_p99_handles_skewed_distribution() {
        let mut acc = TelemetryAccumulator::new();
        // 99 samples at 10ms, 1 sample at 1000ms — p99 should land near 1000.
        for _ in 0..99 {
            acc.record(TelemetryFrame {
                pid: 1,
                latency_ms: Some(10.0),
                ..TelemetryFrame::new(1)
            });
        }
        acc.record(TelemetryFrame {
            pid: 1,
            latency_ms: Some(1000.0),
            ..TelemetryFrame::new(1)
        });
        let m = acc.snapshot(1).unwrap();
        let p99 = m.inference_latency_ms_p99.unwrap();
        assert!(p99 >= 100.0, "p99={p99}, expected near the tail");
    }

    #[test]
    fn nan_tps_is_skipped_not_recorded() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(frame(1, Some(f32::NAN)));
        // Nothing recorded → snapshot still has None for tps avg.
        let m = acc.snapshot(1).unwrap();
        assert!(m.tokens_per_sec_avg.is_none());
    }
}
