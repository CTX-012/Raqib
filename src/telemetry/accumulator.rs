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
    /// Wall-clock instant of the first sample, used to integrate
    /// energy from per-frame watts (energy = ∫ watts dt).
    first_sample_at: Option<std::time::Instant>,
    last_sample_at: Option<std::time::Instant>,

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

    // Power & thermals (Tier 2.1). Sums for averages, peaks for the
    // tail, and a running joules counter computed by trapezoidal
    // integration as samples land.
    gpu_watts_sum: f32,
    gpu_watts_peak: f32,
    gpu_watts_samples: u32,
    gpu_watts_last: Option<f32>,
    cpu_watts_sum: f32,
    cpu_watts_samples: u32,
    cpu_watts_last: Option<f32>,
    energy_joules: f32,

    // Authoritative model name from a runtime API.
    model_name_hint: Option<String>,
}

impl PerPidStats {
    fn record(&mut self, f: &TelemetryFrame) {
        self.samples = self.samples.saturating_add(1);

        // Track sample window for energy integration.
        let now = std::time::Instant::now();
        if self.first_sample_at.is_none() {
            self.first_sample_at = Some(now);
        }
        let prev_at = self.last_sample_at.replace(now);

        // Reject negative + non-finite token rates (S3 in TEST.md F.2.6).
        // The strict policy: drop the value rather than store noise.
        if let Some(tps) = f.tokens_per_sec
            && tps.is_finite()
            && (0.0..=1.0e6).contains(&tps)
        {
            self.tps_sum += tps;
            self.tps_samples += 1;
            if tps > self.tps_peak {
                self.tps_peak = tps;
            }
        }
        if let Some(fps) = f.fps
            && fps.is_finite()
            && (0.0..=1.0e6).contains(&fps)
        {
            self.fps_sum += fps;
            self.fps_samples += 1;
        }
        if let Some(lat) = f.latency_ms
            && lat.is_finite()
            && (0.0..=1.0e6).contains(&lat)
        {
            self.latency_sum_ms += lat;
            self.latency_samples += 1;
            if self.latency_window.len() < 4096 {
                self.latency_window.push(lat);
            }
        }
        if let Some(kv) = f.kv_cache_pct
            && kv.is_finite()
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

        // Power: sum-of-readings + peak; integrate energy by
        // trapezoidal rule against the previous reading (so a sudden
        // spike between samples gets averaged, not full-counted).
        if let Some(w) = f.gpu_watts
            && w.is_finite()
            && (0.0..=10_000.0).contains(&w)
        {
            self.gpu_watts_sum += w;
            self.gpu_watts_samples += 1;
            if w > self.gpu_watts_peak {
                self.gpu_watts_peak = w;
            }
            if let (Some(prev_w), Some(prev_t)) = (self.gpu_watts_last, prev_at) {
                let dt = now.saturating_duration_since(prev_t).as_secs_f32();
                self.energy_joules += 0.5 * (prev_w + w) * dt;
            }
            self.gpu_watts_last = Some(w);
        }
        if let Some(w) = f.cpu_watts
            && w.is_finite()
            && (0.0..=10_000.0).contains(&w)
        {
            self.cpu_watts_sum += w;
            self.cpu_watts_samples += 1;
            if let (Some(prev_w), Some(prev_t)) = (self.cpu_watts_last, prev_at) {
                let dt = now.saturating_duration_since(prev_t).as_secs_f32();
                self.energy_joules += 0.5 * (prev_w + w) * dt;
            }
            self.cpu_watts_last = Some(w);
        }

        // Latest model-name hint wins (Ollama can switch between calls).
        if let Some(name) = f.model_name_hint.as_ref()
            && !name.is_empty()
        {
            self.model_name_hint = Some(name.clone());
        }
    }

    /// Authoritative model name observed via a runtime API (Tier 1.2c).
    /// Dispatcher promotes this onto `RunRecord.summary.model_name`
    /// when present.
    pub fn model_name_hint(&self) -> Option<&str> {
        self.model_name_hint.as_deref()
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

            // Power & thermals (Tier 2.1).
            gpu_watts_avg: avg(self.gpu_watts_sum, self.gpu_watts_samples),
            gpu_watts_peak: opt_finite(self.gpu_watts_peak, self.gpu_watts_samples > 0),
            cpu_watts_avg: avg(self.cpu_watts_sum, self.cpu_watts_samples),
            energy_joules_total: if self.energy_joules > 0.0 {
                Some(self.energy_joules)
            } else {
                None
            },

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

    /// Authoritative model name from a runtime API for `pid`, or
    /// `None` when no API source ever reported one. Tier 1.2c.
    pub fn model_name_hint_for(&self, pid: u32) -> Option<&str> {
        self.by_pid.get(&pid).and_then(|s| s.model_name_hint())
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

    /// Hardening for TEST.md F.2.6 / F.2.7 — negative and absurd
    /// values must be dropped, not pass through to RunMetrics.
    #[test]
    fn negative_tps_is_rejected() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(frame(1, Some(-10.0)));
        assert!(acc.snapshot(1).unwrap().tokens_per_sec_avg.is_none());
    }

    #[test]
    fn impossibly_large_tps_is_rejected() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(frame(1, Some(1.0e18)));
        assert!(acc.snapshot(1).unwrap().tokens_per_sec_avg.is_none());
    }

    /// Power: average + peak rolled up correctly.
    #[test]
    fn gpu_watts_average_and_peak() {
        let mut acc = TelemetryAccumulator::new();
        for w in [50.0, 100.0, 75.0_f32] {
            acc.record(TelemetryFrame {
                pid: 1,
                gpu_watts: Some(w),
                ..TelemetryFrame::new(1)
            });
        }
        let m = acc.snapshot(1).unwrap();
        assert!((m.gpu_watts_avg.unwrap() - 75.0).abs() < 1e-3);
        assert!((m.gpu_watts_peak.unwrap() - 100.0).abs() < 1e-3);
    }

    /// Energy integration: 0.5 J trapezoidal rule per pair of samples.
    /// Frames recorded back-to-back accumulate energy proportional to
    /// the wall-clock delay between them; a single sample produces 0 J.
    #[test]
    fn energy_zero_for_single_sample() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(TelemetryFrame {
            pid: 1,
            gpu_watts: Some(100.0),
            ..TelemetryFrame::new(1)
        });
        // No second sample → no integration → no joules.
        assert!(acc.snapshot(1).unwrap().energy_joules_total.is_none());
    }

    /// Authoritative model name from Ollama is stamped onto stats.
    #[test]
    fn model_name_hint_propagates() {
        let mut acc = TelemetryAccumulator::new();
        acc.record(TelemetryFrame {
            pid: 1,
            model_name_hint: Some("llama3:8b".into()),
            ..TelemetryFrame::new(1)
        });
        let stats = acc.by_pid.get(&1).unwrap();
        assert_eq!(stats.model_name_hint(), Some("llama3:8b"));
    }
}
