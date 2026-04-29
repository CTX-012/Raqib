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

use std::cmp::Ordering;
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::run_store::RunRecord;

/// How `BaselineMetrics::from_records_with` aggregates each per-metric
/// distribution into a single central-tendency value.
///
/// `Mean` is the historical default and what every Foundation-C call
/// site assumes. `Median` exists for adversarial baselines where one
/// bad run would otherwise pull the centre toward itself and mask a
/// real regression on every subsequent comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaselineStrategy {
    /// Arithmetic mean. Sensitive to outliers; right when baseline runs
    /// are tightly clustered and the user trusts every input.
    #[default]
    Mean,
    /// Per-metric median. Robust to a single bad run; right for noisy
    /// baselines (canary fleets, dev machines, freshly imported data).
    Median,
}

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

/// Per-metric central-tendency + dispersion. `mean` is the
/// historical field name and now carries either the arithmetic mean
/// or the median depending on `BaselineStrategy` — the comparator
/// reads it as "the centre to compare against" in both modes. `stddev`
/// is always the population stddev around the *arithmetic* mean (used
/// by the outlier detector); switching that to MAD or to a
/// median-anchored variance would change every existing call site's
/// numeric expectation, so it stays as-is until a future feature
/// introduces a deliberate change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeanStd {
    pub mean: f32,
    pub stddev: f32,
    pub n: u32,
}

impl BaselineMetrics {
    /// Roll up the per-metric stats from a slice of records using the
    /// historical default ([`BaselineStrategy::Mean`], outliers kept).
    /// Every Foundation-C call site that predates [`Self::from_records_with`]
    /// goes through this — kept for source-stable back-compat.
    pub fn from_records(records: &[RunRecord]) -> Self {
        Self::from_records_with(records, BaselineStrategy::Mean, false).0
    }

    /// Build per-metric stats with a chosen central-tendency strategy
    /// and an optional drop-outliers pass. Returns the metrics plus the
    /// list of `RunId`s flagged as outliers under the brief's
    /// "> 2 stddevs from the median" rule. The flag list is independent
    /// of `strategy` (the rule always probes against the median) so a
    /// caller comparing the two strategies side-by-side gets the same
    /// outlier set in both branches — only the centre moves.
    ///
    /// Outlier criterion (per metric, applied across records):
    /// 1. Sort the metric's values, compute the median.
    /// 2. Compute the population stddev around the *arithmetic* mean
    ///    of those values.
    /// 3. Any record with `|value − median| > 2·stddev` is an outlier
    ///    on this metric. A record flagged on *any* metric is in the
    ///    return list once.
    /// 4. Distributions with < 2 finite samples or with stddev below
    ///    `f32::EPSILON` are skipped — there is no signal there.
    ///
    /// When `drop_outliers` is true, the second-pass statistics are
    /// recomputed over the non-outlier subset. When false, the metrics
    /// reflect every input and the caller decides what to do with the
    /// flag list (warn, audit-log, mask in the UI, …).
    pub fn from_records_with(
        records: &[RunRecord],
        strategy: BaselineStrategy,
        drop_outliers: bool,
    ) -> (Self, Vec<Uuid>) {
        let outliers = identify_outliers(records);
        let kept: Vec<&RunRecord> = if drop_outliers && !outliers.is_empty() {
            let drop: HashSet<Uuid> = outliers.iter().copied().collect();
            records
                .iter()
                .filter(|r| !drop.contains(&r.run_id))
                .collect()
        } else {
            records.iter().collect()
        };
        let metrics = BaselineMetrics {
            avg_cpu_pct: stat(kept.iter().map(|r| Some(r.summary.avg_cpu_pct)), strategy),
            peak_cpu_pct: stat(kept.iter().map(|r| Some(r.summary.peak_cpu_pct)), strategy),
            peak_rss_mb: stat(
                kept.iter().map(|r| Some(r.summary.peak_rss_mb as f32)),
                strategy,
            ),
            peak_vram_mb: stat(
                kept.iter().map(|r| Some(r.summary.peak_vram_mb as f32)),
                strategy,
            ),
            uptime_secs: stat(
                kept.iter().map(|r| Some(r.summary.uptime_secs as f32)),
                strategy,
            ),
            tokens_per_sec_avg: stat(kept.iter().map(|r| r.metrics.tokens_per_sec_avg), strategy),
            fps_avg: stat(kept.iter().map(|r| r.metrics.fps_avg), strategy),
            gpu_watts_avg: stat(kept.iter().map(|r| r.metrics.gpu_watts_avg), strategy),
        };
        (metrics, outliers)
    }
}

/// `Some(MeanStd)` over the present values, or `None` if no values.
/// `mean` carries the chosen central tendency (arithmetic mean or
/// median); `stddev` is always the population stddev around the
/// arithmetic mean — see [`MeanStd`] field doc.
fn stat(it: impl Iterator<Item = Option<f32>>, strategy: BaselineStrategy) -> Option<MeanStd> {
    let values: Vec<f32> = it.flatten().filter(|v| v.is_finite()).collect();
    if values.is_empty() {
        return None;
    }
    let n = values.len() as f32;
    let arithmetic_mean = values.iter().sum::<f32>() / n;
    let centre = match strategy {
        BaselineStrategy::Mean => arithmetic_mean,
        BaselineStrategy::Median => {
            let mut sorted = values.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            median_of_sorted(&sorted)
        }
    };
    let var = values
        .iter()
        .map(|v| (v - arithmetic_mean).powi(2))
        .sum::<f32>()
        / n;
    Some(MeanStd {
        mean: centre,
        stddev: var.sqrt(),
        n: values.len() as u32,
    })
}

/// Median of a *sorted* slice. Even-length slices get the mean of the
/// two middle elements (the only convention that makes the test
/// fixture's [20, 100, 100, 100, 100, 100] resolve to 100, not 50).
fn median_of_sorted(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}

/// Sorted, deduplicated list of `RunId`s flagged on at least one
/// metric. See [`BaselineMetrics::from_records_with`] for the rule.
fn identify_outliers(records: &[RunRecord]) -> Vec<Uuid> {
    type Extractor = fn(&RunRecord) -> Option<f32>;
    let extractors: &[Extractor] = &[
        |r| Some(r.summary.avg_cpu_pct),
        |r| Some(r.summary.peak_cpu_pct),
        |r| Some(r.summary.peak_rss_mb as f32),
        |r| Some(r.summary.peak_vram_mb as f32),
        |r| Some(r.summary.uptime_secs as f32),
        |r| r.metrics.tokens_per_sec_avg,
        |r| r.metrics.fps_avg,
        |r| r.metrics.gpu_watts_avg,
    ];

    let mut flagged: HashSet<Uuid> = HashSet::new();
    for extractor in extractors {
        let values: Vec<(Uuid, f32)> = records
            .iter()
            .filter_map(|r| {
                extractor(r)
                    .filter(|v| v.is_finite())
                    .map(|v| (r.run_id, v))
            })
            .collect();
        if values.len() < 2 {
            continue;
        }

        let mut sorted: Vec<f32> = values.iter().map(|(_, v)| *v).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let median = median_of_sorted(&sorted);
        let n = sorted.len() as f32;
        let arithmetic_mean = sorted.iter().sum::<f32>() / n;
        let var = sorted
            .iter()
            .map(|v| (v - arithmetic_mean).powi(2))
            .sum::<f32>()
            / n;
        let stddev = var.sqrt();
        if stddev < f32::EPSILON {
            continue;
        }
        let threshold = 2.0 * stddev;
        for (id, v) in values {
            if (v - median).abs() > threshold {
                flagged.insert(id);
            }
        }
    }

    let mut sorted: Vec<Uuid> = flagged.into_iter().collect();
    sorted.sort();
    sorted
}

/// Computed baseline for a single model. `sample_size` is the number
/// of records that contributed to the per-metric rollup. The two
/// trailing fields default to "Mean strategy, no outliers" so older
/// JSON serialisations of `Baseline` still parse — they were the only
/// possible answer before this struct grew the fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub model: String,
    pub sample_size: usize,
    pub metrics: BaselineMetrics,
    pub computed_at: DateTime<Utc>,
    /// Records that triggered the outlier rule on at least one metric
    /// during this baseline's computation. Empty when no run was more
    /// than 2 stddevs from the per-metric median, or when the input
    /// distribution had < 2 finite samples on every metric. The list
    /// is informational regardless of whether the metrics were
    /// recomputed without the outliers — see
    /// [`BaselineMetrics::from_records_with`].
    #[serde(default)]
    pub outlier_run_ids: Vec<Uuid>,
    /// Strategy used to compute the per-metric centre. Persisted so a
    /// downstream tool comparing two stored baselines knows whether
    /// they're directly comparable or were aggregated differently.
    #[serde(default)]
    pub strategy: BaselineStrategy,
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
        // Silent return is the right behaviour (tiny baselines must
        // not flag regressions) but operators wondering "why didn't
        // I get an alert?" have nowhere to look. Debug-level keeps
        // it out of the default log stream while making it
        // discoverable with `--log-level debug` — DESIGN_HANDOFF
        // Gap 14 / Principle 6 (empty states teach the product).
        tracing::debug!(
            target: "regression",
            model = %baseline.model,
            sample_size = baseline.sample_size,
            min_baseline_samples = cfg.min_baseline_samples,
            "baseline below the minimum sample size; \
             no regressions emitted (this is by design — a baseline \
             of fewer than min_baseline_samples runs cannot \
             distinguish regression from noise)"
        );
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
            outlier_run_ids: Vec::new(),
            strategy: BaselineStrategy::default(),
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
            (
                "20.01% (just at/above critical)",
                79.99,
                Some(Severity::Critical),
            ),
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

    /// F.3.8 — robust baseline strategy + outlier flag. The brief's
    /// scenario: 5 baseline runs at 100 tok/s and one bad run at 20
    /// tok/s. With the historical Mean strategy the baseline lands at
    /// ~86.67 tok/s, biased toward the outlier; with Median it stays at
    /// 100 (the value 5/6 of the runs actually hit). The 20 tok/s run
    /// gets flagged in the outlier list under both strategies because
    /// the rule (> 2 stddev from the median) is independent of the
    /// centre choice.
    ///
    /// Numeric expectations:
    /// * mean of [100, 100, 100, 100, 100, 20] = 86.667
    /// * median (even n=6) = average of pos 2 and 3 of the sorted list
    ///   = (100 + 100) / 2 = 100
    /// * stddev around the mean ≈ 29.81
    /// * 2 × stddev ≈ 59.63; |20 − 100| = 80 → outlier ✓
    /// * |100 − 100| = 0 → not an outlier
    ///
    /// `drop_outliers = true` recomputes everything over the kept set:
    /// n drops from 6 to 5, the median is still 100, and the stddev
    /// collapses to 0 because every kept value is identical.
    #[test]
    fn robust_baseline_median_unaffected_by_outlier() {
        let records: Vec<RunRecord> = (0..5)
            .map(|_| record_with_tps(100.0))
            .chain(std::iter::once(record_with_tps(20.0)))
            .collect();

        // Mean strategy → biased toward 20.
        let (mean_metrics, mean_outliers) =
            BaselineMetrics::from_records_with(&records, BaselineStrategy::Mean, false);
        let mean_centre = mean_metrics
            .tokens_per_sec_avg
            .expect("tokens_per_sec_avg present")
            .mean;
        assert!(
            (mean_centre - 86.667).abs() < 0.5,
            "mean strategy centre should be ~86.67, got {mean_centre}"
        );

        // Median strategy → unaffected by the single bad run.
        let (median_metrics, median_outliers) =
            BaselineMetrics::from_records_with(&records, BaselineStrategy::Median, false);
        let median_centre = median_metrics
            .tokens_per_sec_avg
            .expect("tokens_per_sec_avg present")
            .mean;
        assert!(
            (median_centre - 100.0).abs() < 0.01,
            "median strategy centre should be 100, got {median_centre}"
        );

        // The 20 tok/s record is the only one that should be flagged.
        let bad_record = records
            .iter()
            .find(|r| r.metrics.tokens_per_sec_avg == Some(20.0))
            .expect("fixture has a 20 tok/s record");
        let good_ids: Vec<Uuid> = records
            .iter()
            .filter(|r| r.run_id != bad_record.run_id)
            .map(|r| r.run_id)
            .collect();
        for outliers in [&mean_outliers, &median_outliers] {
            assert!(
                outliers.contains(&bad_record.run_id),
                "20 tok/s run should be flagged as outlier, got {outliers:?}"
            );
            for id in &good_ids {
                assert!(
                    !outliers.contains(id),
                    "100 tok/s runs should not be flagged: {id}"
                );
            }
        }

        // drop_outliers: median is still 100, but n is now 5.
        let (drop_metrics, drop_outliers) =
            BaselineMetrics::from_records_with(&records, BaselineStrategy::Median, true);
        let dropped_centre = drop_metrics
            .tokens_per_sec_avg
            .expect("tokens_per_sec_avg present")
            .mean;
        assert!(
            (dropped_centre - 100.0).abs() < 0.01,
            "median after dropping outlier should be 100, got {dropped_centre}"
        );
        assert_eq!(
            drop_metrics.tokens_per_sec_avg.unwrap().n,
            5,
            "n should be 5 after dropping the outlier"
        );
        // And the outlier list returned is still the original one — the
        // caller asked us to drop, not to forget which record we dropped.
        assert!(drop_outliers.contains(&bad_record.run_id));

        // Sanity: when no outliers exist, both strategies agree and the
        // outlier list is empty.
        let clean: Vec<RunRecord> = (0..6).map(|_| record_with_tps(50.0)).collect();
        let (mc, mo) = BaselineMetrics::from_records_with(&clean, BaselineStrategy::Mean, false);
        let (med_c, med_o) =
            BaselineMetrics::from_records_with(&clean, BaselineStrategy::Median, false);
        assert!(mo.is_empty() && med_o.is_empty());
        assert_eq!(mc.tokens_per_sec_avg.unwrap().mean, 50.0);
        assert_eq!(med_c.tokens_per_sec_avg.unwrap().mean, 50.0);
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
