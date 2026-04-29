//! Baseline + regression detection for completed runs.
//!
//! Foundation C of latest.md. Takes a slice of `RunRecord`s, computes
//! mean/stddev for each tracked numeric metric, and flags new runs that
//! deviate beyond configurable thresholds.
//!
//! Design choices:
//!
//! * **Refuses tiny baselines.** With fewer than `MIN_BASELINE_SAMPLES`
//!   records (default 3) we return no regressions even when the values
//!   look bad — three points isn't a baseline, it's coincidence.
//! * **Direction matters.** A faster run is never a regression. The
//!   comparator knows for each metric whether higher or lower is better
//!   (CPU peaks: lower is better; tokens/sec: higher is better; etc.).
//! * **Thresholds are configurable.** Defaults: warn at >10% worse,
//!   critical at >25% worse. Caller can override via `RegressionConfig`.
//! * **Foundation A scope.** Only the four `LifecycleSummary`-derived
//!   numeric fields are baselined today. Telemetry-driven metrics
//!   (tokens/sec, fps, watts, …) wire in once Tier 1.2 / 2.x land.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::storage::run_store::RunRecord;

/// Default minimum sample size for a useful baseline. Below this we
/// emit no regressions regardless of the input deltas — the spec calls
/// this out explicitly so we encode it as a constant.
pub const MIN_BASELINE_SAMPLES: usize = 3;

/// Severity bands. The numeric ordering matters: callers filter with
/// `severity >= Warn` to drop Info-level noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

/// Per-metric mean + stddev. Fields are `Option<f32>` because a metric
/// is only baselined when at least one record carried a value. Records
/// where the metric is `None` are skipped, not treated as zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BaselineMetrics {
    pub avg_cpu_pct: Option<MeanStd>,
    pub peak_cpu_pct: Option<MeanStd>,
    pub peak_rss_mb: Option<MeanStd>,
    pub peak_vram_mb: Option<MeanStd>,
    pub uptime_secs: Option<MeanStd>,

    // Telemetry-driven; populated once Tier 1.2/2.x runs.
    pub tokens_per_sec_avg: Option<MeanStd>,
    pub fps_avg: Option<MeanStd>,
    pub gpu_watts_avg: Option<MeanStd>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeanStd {
    pub mean: f32,
    pub stddev: f32,
    pub n: u32,
}

impl BaselineMetrics {
    /// Roll up the per-metric stats from a slice of records. Each
    /// metric is computed independently from whichever records carry it
    /// — a baseline can have n=10 for `peak_cpu_pct` and n=2 for
    /// `tokens_per_sec_avg` if telemetry only fired on two runs.
    pub fn from_records(records: &[RunRecord]) -> Self {
        BaselineMetrics {
            avg_cpu_pct: mean_std(records.iter().map(|r| Some(r.summary.avg_cpu_pct))),
            peak_cpu_pct: mean_std(records.iter().map(|r| Some(r.summary.peak_cpu_pct))),
            peak_rss_mb: mean_std(records.iter().map(|r| Some(r.summary.peak_rss_mb as f32))),
            peak_vram_mb: mean_std(records.iter().map(|r| Some(r.summary.peak_vram_mb as f32))),
            uptime_secs: mean_std(records.iter().map(|r| Some(r.summary.uptime_secs as f32))),
            tokens_per_sec_avg: mean_std(records.iter().map(|r| r.metrics.tokens_per_sec_avg)),
            fps_avg: mean_std(records.iter().map(|r| r.metrics.fps_avg)),
            gpu_watts_avg: mean_std(records.iter().map(|r| r.metrics.gpu_watts_avg)),
        }
    }
}

/// `Some(MeanStd)` over the present values, or `None` if no values.
fn mean_std(it: impl Iterator<Item = Option<f32>>) -> Option<MeanStd> {
    let values: Vec<f32> = it.flatten().filter(|v| v.is_finite()).collect();
    if values.is_empty() {
        return None;
    }
    let n = values.len() as f32;
    let mean = values.iter().sum::<f32>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n;
    Some(MeanStd {
        mean,
        stddev: var.sqrt(),
        n: values.len() as u32,
    })
}

/// Computed baseline for a single model. `sample_size` is the number
/// of records that contributed to the per-metric rollup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub model: String,
    pub sample_size: usize,
    pub metrics: BaselineMetrics,
    pub computed_at: DateTime<Utc>,
}

/// One detected regression. `delta_pct` is positive when the metric is
/// worse than baseline, negative when better — the comparator does the
/// direction-flip per-metric so the sign always means the same thing.
///
/// `metric` is `String` (not `&'static str`) so the struct can derive
/// Deserialize — needed for the JSONL audit log Tier 1.3 writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regression {
    pub metric: String,
    pub baseline: f32,
    pub current: f32,
    pub delta_pct: f32,
    pub severity: Severity,
}

/// Time-stamped envelope around a [`Regression`] for the audit ring
/// buffer. Keeps the regression event self-describing when it lands in
/// the TUI panel (model name + when it fired) without forcing the
/// caller to re-derive that context from the `RunRecord`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionEvent {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub baseline_size: usize,
    pub regression: Regression,
}

/// Thresholds + sample-size gate. Defaults match latest.md's text.
#[derive(Debug, Clone, Copy)]
pub struct RegressionConfig {
    pub warn_pct: f32,
    pub critical_pct: f32,
    pub min_baseline_samples: usize,
}

impl Default for RegressionConfig {
    fn default() -> Self {
        Self {
            warn_pct: 10.0,
            critical_pct: 25.0,
            min_baseline_samples: MIN_BASELINE_SAMPLES,
        }
    }
}

/// Compare a record against a baseline using default thresholds.
/// `Regression::severity` is `Info` for sub-warn-threshold deltas — the
/// caller can choose to drop those.
pub fn detect_regressions(record: &RunRecord, baseline: &Baseline) -> Vec<Regression> {
    detect_regressions_with(record, baseline, &RegressionConfig::default())
}

/// Same as [`detect_regressions`] with explicit thresholds.
pub fn detect_regressions_with(
    record: &RunRecord,
    baseline: &Baseline,
    cfg: &RegressionConfig,
) -> Vec<Regression> {
    if baseline.sample_size < cfg.min_baseline_samples {
        return Vec::new();
    }
    let mut out = Vec::new();

    // (metric_name, baseline-mean, current-value, lower_is_better).
    // Names are stored as `String` on the emitted `Regression` per the
    // serde-Deserialize requirement; static slices keep the table
    // copy-only here.
    let probes: &[(&str, Option<f32>, Option<f32>, bool)] = &[
        (
            "avg_cpu_pct",
            baseline.metrics.avg_cpu_pct.map(|m| m.mean),
            Some(record.summary.avg_cpu_pct),
            true,
        ),
        (
            "peak_cpu_pct",
            baseline.metrics.peak_cpu_pct.map(|m| m.mean),
            Some(record.summary.peak_cpu_pct),
            true,
        ),
        (
            "peak_rss_mb",
            baseline.metrics.peak_rss_mb.map(|m| m.mean),
            Some(record.summary.peak_rss_mb as f32),
            true,
        ),
        (
            "peak_vram_mb",
            baseline.metrics.peak_vram_mb.map(|m| m.mean),
            Some(record.summary.peak_vram_mb as f32),
            true,
        ),
        (
            "tokens_per_sec_avg",
            baseline.metrics.tokens_per_sec_avg.map(|m| m.mean),
            record.metrics.tokens_per_sec_avg,
            false, // higher is better
        ),
        (
            "fps_avg",
            baseline.metrics.fps_avg.map(|m| m.mean),
            record.metrics.fps_avg,
            false,
        ),
        (
            "gpu_watts_avg",
            baseline.metrics.gpu_watts_avg.map(|m| m.mean),
            record.metrics.gpu_watts_avg,
            true, // lower watts is better
        ),
    ];

    for (name, base, cur, lower_is_better) in probes {
        let (Some(base), Some(cur)) = (*base, *cur) else {
            continue;
        };
        if !base.is_finite() || base.abs() < f32::EPSILON {
            continue;
        }
        // raw_pct: percent change from baseline (signed).
        let raw_pct = (cur - base) / base * 100.0;
        // normalised "worse-is-positive" delta.
        let delta_pct = if *lower_is_better { raw_pct } else { -raw_pct };

        if delta_pct < cfg.warn_pct {
            // Better, equal, or only marginally worse — not flagged
            // even at Info, to keep the output focused on regressions.
            continue;
        }
        let severity = if delta_pct >= cfg.critical_pct {
            Severity::Critical
        } else {
            Severity::Warn
        };
        out.push(Regression {
            metric: name.to_string(),
            baseline: base,
            current: cur,
            delta_pct,
            severity,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecycleSummary;
    use crate::model::AICategory;
    use crate::storage::run_store::{RunMetrics, RunRecord};
    use chrono::Utc;

    fn record_with_tps(tps: f32) -> RunRecord {
        let summary = LifecycleSummary {
            pid: 1,
            name: "vllm".into(),
            category: Some(AICategory::Inference),
            model_name: Some("phi3-mini".into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 60,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 50.0,
            peak_cpu_pct: 80.0,
            peak_rss_mb: 1024,
            peak_vram_mb: 0,
            samples: 60,
        };
        let mut r = RunRecord::from_summary(summary);
        r.metrics = RunMetrics {
            tokens_per_sec_avg: Some(tps),
            ..RunMetrics::default()
        };
        r
    }

    fn baseline_from(n: usize, tps: f32) -> Baseline {
        let records: Vec<RunRecord> = (0..n).map(|_| record_with_tps(tps)).collect();
        Baseline {
            model: "phi3-mini".into(),
            sample_size: records.len(),
            metrics: BaselineMetrics::from_records(&records),
            computed_at: Utc::now(),
        }
    }

    /// Spec: stable baseline + matching record = no regressions.
    #[test]
    fn matching_record_no_regressions() {
        let baseline = baseline_from(10, 40.0);
        let current = record_with_tps(40.0);
        let regs = detect_regressions(&current, &baseline);
        assert!(regs.is_empty(), "got: {regs:?}");
    }

    /// Spec: 10-run baseline at 40 tok/s, new run at 28 tok/s → critical.
    #[test]
    fn slow_run_is_critical() {
        let baseline = baseline_from(10, 40.0);
        let current = record_with_tps(28.0);
        let regs = detect_regressions(&current, &baseline);
        let r = regs
            .iter()
            .find(|r| r.metric == "tokens_per_sec_avg")
            .expect("expected tokens_per_sec_avg regression");
        assert_eq!(r.severity, Severity::Critical);
        // 28 vs 40 is 30% slower → delta_pct ≈ 30.
        assert!((r.delta_pct - 30.0).abs() < 0.5, "delta={}", r.delta_pct);
    }

    /// Spec: 10-run baseline at 40 tok/s, new run at 41 tok/s → improvement, no regression.
    #[test]
    fn faster_run_is_not_a_regression() {
        let baseline = baseline_from(10, 40.0);
        let current = record_with_tps(41.0);
        let regs = detect_regressions(&current, &baseline);
        assert!(
            regs.iter().all(|r| r.metric != "tokens_per_sec_avg"),
            "improvement was flagged: {regs:?}"
        );
    }

    /// Spec: 2-run baseline → no regressions (sample too small).
    #[test]
    fn tiny_baseline_emits_no_regressions() {
        let baseline = baseline_from(2, 40.0);
        let current = record_with_tps(20.0);
        let regs = detect_regressions(&current, &baseline);
        assert!(regs.is_empty(), "got: {regs:?}");
    }

    /// Spec: a 30%-worse RSS run with a 10-record baseline → critical.
    /// Exercises a "lower is better" metric path so both directions are
    /// covered.
    #[test]
    fn higher_rss_is_a_regression() {
        // 10 baseline records at 1024 MB peak RSS; current at 1500 MB.
        // 1500 vs 1024 = +46.4% worse → critical.
        let baseline = baseline_from(10, 40.0);
        let mut current = record_with_tps(40.0);
        current.summary.peak_rss_mb = 1500;
        let regs = detect_regressions(&current, &baseline);
        let r = regs
            .iter()
            .find(|r| r.metric == "peak_rss_mb")
            .expect("expected peak_rss_mb regression");
        assert_eq!(r.severity, Severity::Critical);
    }

    /// F.3.4 — a 12% drop on `tokens_per_sec_avg` against a stable
    /// 10-record baseline at 40 tok/s lands as Warn (not Critical, not
    /// silently dropped). The default thresholds are 10% warn / 25%
    /// critical so 12% is unambiguously the warn band.
    #[test]
    fn twelve_percent_drop_is_warn_not_critical() {
        let baseline = baseline_from(10, 40.0);
        // 12% slower than 40 = 35.2.
        let current = record_with_tps(35.2);
        let regs = detect_regressions(&current, &baseline);
        let r = regs
            .iter()
            .find(|r| r.metric == "tokens_per_sec_avg")
            .expect("expected tokens_per_sec_avg regression at 12% drop");
        assert_eq!(
            r.severity,
            Severity::Warn,
            "12% drop should be Warn, got {:?} (delta_pct={})",
            r.severity,
            r.delta_pct
        );
        assert!(
            (r.delta_pct - 12.0).abs() < 0.5,
            "delta_pct should be ~12, got {}",
            r.delta_pct
        );
    }

    /// F.3.4 boundary battery. Five cases at 9.99% / 10.01% / 19.99% /
    /// 20.01% (and the 12% mid-band) hold the comparator's classification
    /// boundaries to the warn = 10% / critical = 20% thresholds the
    /// brief specifies. Boundaries catch off-by-one (`>` vs `>=`) in the
    /// banding logic; with the default thresholds (warn = 10 / critical
    /// = 25) the 19.99 and 20.01 cases would both come out Warn and we
    /// would learn nothing, so a one-off `RegressionConfig` overrides
    /// the upper bound to 20% for this test.
    ///
    /// Boundary semantics under the comparator:
    ///
    /// * `delta_pct < warn_pct` → no regression emitted (Matching).
    /// * `warn_pct ≤ delta_pct < critical_pct` → Warn.
    /// * `delta_pct ≥ critical_pct` → Critical.
    ///
    /// `9.99 < 10` and `19.99 < 20` are honoured by `<` checks, not
    /// `<=`; flipping either to `<=` (or the equivalent `>` to `>=`)
    /// in the implementation would break exactly one of these cases.
    #[test]
    fn warn_critical_boundary_matrix() {
        let cfg = RegressionConfig {
            warn_pct: 10.0,
            critical_pct: 20.0,
            min_baseline_samples: MIN_BASELINE_SAMPLES,
        };
        let baseline = baseline_from(10, 100.0);

        // (slowdown_pct, current_tps, expected) — 100 tok/s baseline
        // makes the arithmetic exact in f32 enough to land on the
        // intended side of each boundary.
        // Expected = None means "no regression emitted" (Matching).
        let cases: &[(&str, f32, Option<Severity>)] = &[
            ("9.99% (just below warn)", 90.01, None),
            ("10.01% (just above warn)", 89.99, Some(Severity::Warn)),
            ("12% (mid warn band)", 88.0, Some(Severity::Warn)),
            ("19.99% (just below critical)", 80.01, Some(Severity::Warn)),
            ("20.01% (just at/above critical)", 79.99, Some(Severity::Critical)),
        ];

        for (label, current_tps, expected) in cases {
            let current = record_with_tps(*current_tps);
            let regs = detect_regressions_with(&current, &baseline, &cfg);
            let found = regs.iter().find(|r| r.metric == "tokens_per_sec_avg");
            match (expected, found) {
                (None, None) => { /* matching — correct */ }
                (None, Some(r)) => panic!(
                    "{label}: expected no regression, got {:?} delta_pct={}",
                    r.severity, r.delta_pct
                ),
                (Some(_), None) => panic!(
                    "{label}: expected regression, got none. baseline={} current={}",
                    100.0, current_tps
                ),
                (Some(exp), Some(r)) => assert_eq!(
                    *exp, r.severity,
                    "{label}: expected {:?}, got {:?} delta_pct={}",
                    exp, r.severity, r.delta_pct
                ),
            }
        }
    }

    /// `BaselineMetrics::from_records` yields per-metric n based on
    /// presence — `tokens_per_sec_avg` should have n=2 even though the
    /// record set has 5 entries with 3 telemetry-less ones.
    #[test]
    fn baseline_metrics_per_metric_n() {
        let mut records: Vec<RunRecord> = (0..3).map(|_| record_with_tps(40.0)).collect();
        // Two records without telemetry: clear the tokens field.
        records.push({
            let mut r = record_with_tps(0.0);
            r.metrics.tokens_per_sec_avg = None;
            r
        });
        records.push({
            let mut r = record_with_tps(0.0);
            r.metrics.tokens_per_sec_avg = None;
            r
        });
        let bm = BaselineMetrics::from_records(&records);
        assert_eq!(bm.tokens_per_sec_avg.unwrap().n, 3);
        assert_eq!(bm.peak_cpu_pct.unwrap().n, 5);
    }
}
