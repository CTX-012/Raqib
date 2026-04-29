//! Tier 3.4 — concurrent-request awareness.
//!
//! ## Why this exists
//!
//! Tokens-per-second alone hides server behaviour. A vLLM instance
//! serving 1 request at 158 tok/s and one serving 8 requests at 20.1
//! tok/s/req (≈161 tok/s aggregate) report nearly the same headline
//! number, but the second is doing 8× the user-visible work. The
//! difference matters for capacity planning and for catching "is my
//! server scaling well?" regressions.
//!
//! ## What this module owns
//!
//! A single, narrow primitive — [`TimeWeightedGauge`]. It folds a
//! stream of `(value, instant)` samples into a step-function integral
//! and answers two questions on demand: time-weighted average and
//! peak. The accumulator (`telemetry::accumulator`) holds one gauge
//! per metric per PID:
//!
//!   * `vllm:num_requests_running`  → "concurrent_requests"
//!   * `vllm:num_requests_waiting`  → "queue_depth"
//!
//! ## Spec interpretation (spec was thin — see latest.md Tier 3.4)
//!
//! latest.md says:
//!   "Pull from vLLM `vllm:num_requests_running` and
//!   `vllm:num_requests_waiting`. Track both peaks and time-weighted
//!   averages."
//!
//! The example test it gives is a step function "1 req for 10 s,
//! 8 for 50 s" — by the textbook integral, the average is
//! `(1·10 + 8·50) / 60 ≈ 6.833`. That is the integration semantics
//! this module implements: a sample at time t holds its value from t
//! until the *next* sample's instant. The final sample contributes
//! nothing extra (we don't know how long it'd have held). One-sample
//! runs therefore have `average() == None` even though `peak()` is
//! `Some(value)`. That's deliberate: averaging over zero elapsed time
//! is undefined, and reporting the single sample as the "average"
//! would silently double-count it against later samples that arrive
//! after a snapshot.
//!
//! ## Non-goals
//!
//! * Not async — pure value type, dispatcher feeds it.
//! * No moving window — the accumulator already caps PID lifetime at
//!   process exit, so memory is bounded.
//! * No serde — tracker state is internal to the accumulator and
//!   never persisted; only the `TimeWeightedSnapshot` lands in
//!   `RunMetrics` (via `f32` / `u32` fields).

use std::time::Instant;

/// Accumulates a stream of `(u32, Instant)` samples treated as a step
/// function. Cheap to construct, cheap to fold; intended to be cloned
/// into a `Default` PerPidStats and forgotten on PID retirement.
#[derive(Debug, Clone, Default)]
pub struct TimeWeightedGauge {
    last_value: Option<u32>,
    last_at: Option<Instant>,
    /// Σ (value_i · Δt_i) where Δt_i is the time between sample i
    /// and sample i+1. Stored as f64 because `u32 · seconds` can
    /// exceed f32 precision over long-running jobs.
    integral: f64,
    /// Total elapsed wall-clock time covered by the integral, i.e.
    /// `last_at − first_at` for the most recent contiguous stretch.
    total_time_secs: f64,
    /// Highest value seen across all samples.
    peak: u32,
    /// Number of samples observed; distinguishes "zero data" from
    /// "data, but the time-weighted denominator is still zero" so
    /// the accumulator can choose between `None` and `Some(0)`
    /// surfaces.
    samples: u32,
}

impl TimeWeightedGauge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one observation into the running integral. Caller passes
    /// the wall-clock `Instant` so tests can drive the gauge with
    /// synthetic times.
    ///
    /// Backwards-time samples (clock skew, pathological input) are
    /// clamped to `dt = 0` rather than triggering a panic; the gauge
    /// stays monotonically forward-progressing.
    pub fn record(&mut self, value: u32, at: Instant) {
        if let (Some(prev_v), Some(prev_t)) = (self.last_value, self.last_at) {
            let dt = at.saturating_duration_since(prev_t).as_secs_f64();
            self.integral += f64::from(prev_v) * dt;
            self.total_time_secs += dt;
        }
        self.last_value = Some(value);
        self.last_at = Some(at);
        if value > self.peak {
            self.peak = value;
        }
        self.samples = self.samples.saturating_add(1);
    }

    /// Time-weighted mean across all samples seen so far. Returns
    /// `None` when fewer than 2 samples have been recorded — a single
    /// sample has no Δt to weight against. Also returns `None` if all
    /// observed Δts were zero (every sample arrived at the same
    /// `Instant`) — division by zero is the only sane definition.
    pub fn average(&self) -> Option<f32> {
        if self.total_time_secs <= 0.0 {
            return None;
        }
        Some((self.integral / self.total_time_secs) as f32)
    }

    /// Highest sample value, or `None` if no samples were recorded.
    /// Distinct from average — peak is meaningful with one sample;
    /// average is not.
    pub fn peak(&self) -> Option<u32> {
        if self.samples == 0 {
            None
        } else {
            Some(self.peak)
        }
    }

    /// Number of samples observed. Exposed so the accumulator can tell
    /// "we never had data" apart from "we had data but every Δt was
    /// zero" — both surface the same `None` from `average()` but the
    /// peak should still report in the second case.
    pub fn samples(&self) -> u32 {
        self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    /// latest.md Tier 3.4 spec example: 1 req for 10 s, then 8 for
    /// 50 s. Time-weighted average = (1·10 + 8·50) / 60 ≈ 6.833.
    #[test]
    fn step_function_one_then_eight_matches_textbook_integral() {
        let t0 = Instant::now();
        let mut g = TimeWeightedGauge::new();
        g.record(1, at(t0, 0));
        g.record(8, at(t0, 10));
        g.record(8, at(t0, 60));
        let avg = g.average().expect("average defined for ≥2 samples spanning >0s");
        assert!(
            (avg - 6.833_333).abs() < 1e-3,
            "expected ≈6.833, got {avg}"
        );
        assert_eq!(g.peak(), Some(8));
    }

    /// Single sample: no Δt to weight against. Average must be None;
    /// peak still reports the value (it's the only data point). This
    /// is the boundary the doc-comment calls out.
    #[test]
    fn single_sample_has_no_average_but_has_peak() {
        let t0 = Instant::now();
        let mut g = TimeWeightedGauge::new();
        g.record(4, t0);
        assert_eq!(g.average(), None);
        assert_eq!(g.peak(), Some(4));
        assert_eq!(g.samples(), 1);
    }

    /// Zero samples: both queries return None.
    #[test]
    fn empty_gauge_returns_none() {
        let g = TimeWeightedGauge::new();
        assert_eq!(g.average(), None);
        assert_eq!(g.peak(), None);
        assert_eq!(g.samples(), 0);
    }

    /// Two samples at the same Instant: total_time_secs == 0, so
    /// average is None even though we have data. Peak still works.
    /// Guards against the spec's "division by zero when concurrent=0"
    /// concern by making the same path cover the degenerate timing.
    #[test]
    fn two_samples_zero_dt_returns_none_average() {
        let t0 = Instant::now();
        let mut g = TimeWeightedGauge::new();
        g.record(2, t0);
        g.record(5, t0);
        assert_eq!(g.average(), None);
        assert_eq!(g.peak(), Some(5));
    }

    /// All-zero values: average is 0.0 (legitimate result, not None),
    /// peak is 0 (we did observe data). Confirms the spec's
    /// "division-by-zero guarded when concurrent=0" requirement —
    /// the divisor is total_time_secs, not the sum of values.
    #[test]
    fn all_zero_values_average_is_zero_not_none() {
        let t0 = Instant::now();
        let mut g = TimeWeightedGauge::new();
        g.record(0, at(t0, 0));
        g.record(0, at(t0, 5));
        assert_eq!(g.average(), Some(0.0));
        assert_eq!(g.peak(), Some(0));
    }

    /// Backwards-time clamp: a sample with an instant before the
    /// previous one contributes Δt=0 (saturating_duration_since), so
    /// the prior interval still counts and the gauge does not panic.
    #[test]
    fn backwards_time_does_not_panic_or_corrupt() {
        let t0 = Instant::now();
        let mut g = TimeWeightedGauge::new();
        g.record(3, at(t0, 0));
        g.record(7, at(t0, 10));   // contributes 3 · 10 = 30
        g.record(9, t0);           // backwards: dt clamped to 0
        let avg = g.average().expect("average defined");
        // After 3 samples: integral = 30 + 7*0 = 30; total_time = 10 + 0 = 10
        // avg = 30/10 = 3.0
        assert!((avg - 3.0).abs() < 1e-3, "expected 3.0, got {avg}");
        assert_eq!(g.peak(), Some(9));
    }

    /// Long monotonically-rising values: the gauge's f64 integral
    /// stays accurate for 1000 samples at 1-second spacing. Sanity
    /// against precision drift if we had used f32 internally.
    #[test]
    fn one_thousand_step_samples_keep_precision() {
        let t0 = Instant::now();
        let mut g = TimeWeightedGauge::new();
        // Constant value 100 for 1000 seconds → avg should be 100.0.
        for i in 0..=1000 {
            g.record(100, at(t0, i));
        }
        let avg = g.average().expect("average defined");
        assert!((avg - 100.0).abs() < 1e-3, "got {avg}");
        assert_eq!(g.peak(), Some(100));
        assert_eq!(g.samples(), 1001);
    }
}
