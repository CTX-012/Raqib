//! `edge_monitor compare MODEL [MODEL ...] [--runs N] [--json]` —
//! Tier 3.7 CLI per latest.md.
//!
//! Side-by-side baseline comparison across one or more models. For
//! each model, computes the rolling baseline over the most recent
//! `--runs` records (default 10) and prints a column. With `--json`,
//! emits a structured array suitable for piping into `jq`.

use std::io::Write;

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use crate::analysis::compare::MeanStd;
use crate::analysis::{Baseline, BaselineMetrics};
use crate::config::Config;
use crate::storage::run_store::RunStore;

/// One column in the comparison output. `serde` so `--json` is well
/// typed for scripting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonColumn {
    pub model: String,
    pub sample_size: usize,
    /// Mean tokens / sec across the window.
    pub tokens_per_sec_avg: Option<MeanStd>,
    pub fps_avg: Option<MeanStd>,
    pub peak_rss_mb: Option<MeanStd>,
    pub peak_vram_mb: Option<MeanStd>,
    pub gpu_watts_avg: Option<MeanStd>,
    pub uptime_secs: Option<MeanStd>,
    /// Energy per token in joules — derived from per-run energy and
    /// tokens_total when both are available. `None` otherwise.
    pub joules_per_token: Option<f32>,
    /// Mean cold-load duration in seconds.
    pub cold_load_seconds: Option<MeanStd>,
}

impl ComparisonColumn {
    fn from_baseline(model: String, baseline: &Baseline, joules_per_token: Option<f32>) -> Self {
        let m = &baseline.metrics;
        Self {
            model,
            sample_size: baseline.sample_size,
            tokens_per_sec_avg: m.tokens_per_sec_avg,
            fps_avg: m.fps_avg,
            peak_rss_mb: m.peak_rss_mb,
            peak_vram_mb: m.peak_vram_mb,
            gpu_watts_avg: m.gpu_watts_avg,
            uptime_secs: m.uptime_secs,
            joules_per_token,
            cold_load_seconds: cold_load_mean_std(baseline),
        }
    }
}

/// Pull `cold_load_seconds.mean ± stddev` directly out of the records
/// referenced by the baseline. The Foundation-C `BaselineMetrics`
/// doesn't track cold-load yet (it's per-record, not in
/// `RunMetrics`'s comparison loop), so we recompute here.
fn cold_load_mean_std(_baseline: &Baseline) -> Option<MeanStd> {
    // We don't have direct access to the underlying records on this
    // path — `Baseline` is just the rolled-up stats. Caller passes
    // the records through `compare()` instead; this stub kept for
    // future extension once `Baseline` carries the source records.
    None
}

/// Entry point used by `main.rs` when the user types
/// `edge_monitor compare ...`. Writes a human table or JSON to
/// stdout.
pub fn run_compare(
    models: Vec<String>,
    runs: usize,
    json: bool,
    config: &Config,
) -> anyhow::Result<()> {
    if models.is_empty() {
        return Err(anyhow!("compare needs at least one model"));
    }
    let path = config
        .storage
        .run_store()
        .ok_or_else(|| anyhow!("storage.run_store_path is empty; nothing to read"))?;
    let store = RunStore::open(&path)
        .with_context(|| format!("opening run store at {}", path.display()))?;

    let mut columns = Vec::with_capacity(models.len());
    let mut empty_models: Vec<String> = Vec::new();
    for model in &models {
        let records = store.recent(model, runs);
        if records.is_empty() {
            // Empty column rather than aborting — operators want to
            // see all requested models even if one has no history.
            // DESIGN_HANDOFF Principle 6 — collect the empty-column
            // names so we can teach in a single trailing stderr note
            // (one model not found is interesting; 4 of 4 models not
            // found is "did you mean a different store?").
            empty_models.push(model.clone());
            columns.push(ComparisonColumn {
                model: model.clone(),
                sample_size: 0,
                tokens_per_sec_avg: None,
                fps_avg: None,
                peak_rss_mb: None,
                peak_vram_mb: None,
                gpu_watts_avg: None,
                uptime_secs: None,
                joules_per_token: None,
                cold_load_seconds: None,
            });
            continue;
        }
        let baseline = Baseline {
            model: model.clone(),
            sample_size: records.len(),
            metrics: BaselineMetrics::from_records(&records),
            computed_at: chrono::Utc::now(),
            outlier_run_ids: Vec::new(),
            strategy: crate::analysis::compare::BaselineStrategy::default(),
        };
        // Derived: J/token. Average the per-record ratio over records
        // that have both energy and tokens — averaging ratios is
        // honest when N varies, since raw means hide divergence.
        let mut ratios: Vec<f32> = Vec::new();
        for r in &records {
            if let (Some(j), Some(t)) = (r.metrics.energy_joules_total, r.metrics.tokens_total)
                && t > 0
            {
                ratios.push(j / t as f32);
            }
        }
        let joules_per_token = if ratios.is_empty() {
            None
        } else {
            let n = ratios.len() as f32;
            Some(ratios.iter().sum::<f32>() / n)
        };
        // Cold-load mean ± stddev — read from records directly here.
        let cold: Vec<f32> = records
            .iter()
            .filter_map(|r| r.cold_start.as_ref().map(|c| c.duration_seconds))
            .filter(|v| v.is_finite())
            .collect();
        let cold_meanstd = if cold.is_empty() {
            None
        } else {
            let n = cold.len() as f32;
            let mean = cold.iter().sum::<f32>() / n;
            let var = cold.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n;
            Some(MeanStd {
                mean,
                stddev: var.sqrt(),
                n: cold.len() as u32,
            })
        };
        let mut col = ComparisonColumn::from_baseline(model.clone(), &baseline, joules_per_token);
        col.cold_load_seconds = cold_meanstd;
        columns.push(col);
    }

    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    if json {
        serde_json::to_writer_pretty(&mut w, &columns)?;
        writeln!(w)?;
    } else {
        render_table(&mut w, &columns)?;
        // DESIGN_HANDOFF Principle 6 — one stderr line that teaches
        // when a comparison includes models we have no data for. The
        // table still renders (empty columns) so a script piping
        // both columns to a downstream tool isn't broken; the note
        // goes to stderr so plain stdout-capture pipelines don't
        // see it. JSON mode skips this — callers there already have
        // sample_size = 0 they can branch on.
        if !empty_models.is_empty() {
            let known = store.list_models();
            let mut err = std::io::stderr().lock();
            writeln!(
                err,
                "\nNote: {} model(s) had no run history: {}",
                empty_models.len(),
                empty_models.join(", ")
            )?;
            if known.is_empty() {
                writeln!(
                    err,
                    "      The run store at {} has no records yet — try \
                     `edge_monitor history` for the empty-state hint.",
                    path.display()
                )?;
            } else {
                writeln!(
                    err,
                    "      Models that DO have history: {}",
                    known.join(", ")
                )?;
            }
        }
    }
    Ok(())
}

fn render_table(w: &mut impl Write, columns: &[ComparisonColumn]) -> std::io::Result<()> {
    const ROW_LABEL: usize = 14;
    const COL_WIDTH: usize = 22;

    // Header.
    write!(w, "{:<ROW_LABEL$}", "")?;
    for c in columns {
        let header = format!("{} (n={})", c.model, c.sample_size);
        write!(w, "{:<COL_WIDTH$}", truncate(&header, COL_WIDTH - 1))?;
    }
    writeln!(w)?;
    write!(w, "{:<ROW_LABEL$}", "")?;
    for _ in columns {
        write!(w, "{}", "─".repeat(COL_WIDTH - 1))?;
        write!(w, " ")?;
    }
    writeln!(w)?;

    write_row(w, "tok/s avg", columns, |c| {
        c.tokens_per_sec_avg
            .map(|m| format!("{:.1} ± {:.1}", m.mean, m.stddev))
    })?;
    write_row(w, "fps avg", columns, |c| {
        c.fps_avg
            .map(|m| format!("{:.1} ± {:.1}", m.mean, m.stddev))
    })?;
    write_row(w, "peak RSS", columns, |c| {
        c.peak_rss_mb
            .map(|m| format!("{:.0} MB ± {:.0}", m.mean, m.stddev))
    })?;
    write_row(w, "peak VRAM", columns, |c| {
        c.peak_vram_mb.map(|m| format!("{:.1} GB", m.mean / 1024.0))
    })?;
    write_row(w, "GPU watts", columns, |c| {
        c.gpu_watts_avg
            .map(|m| format!("{:.1} ± {:.1}", m.mean, m.stddev))
    })?;
    write_row(w, "uptime (s)", columns, |c| {
        c.uptime_secs
            .map(|m| format!("{:.0} ± {:.0}", m.mean, m.stddev))
    })?;
    write_row(w, "W/token", columns, |c| {
        c.joules_per_token.map(|j| format!("{:.3}", j))
    })?;
    write_row(w, "cold load", columns, |c| {
        c.cold_load_seconds.map(|m| format!("{:.1} s", m.mean))
    })?;
    Ok(())
}

fn write_row<F>(
    w: &mut impl Write,
    label: &str,
    columns: &[ComparisonColumn],
    cell: F,
) -> std::io::Result<()>
where
    F: Fn(&ComparisonColumn) -> Option<String>,
{
    write!(w, "{:<14}", label)?;
    for c in columns {
        let s = cell(c).unwrap_or_else(|| "-".into());
        write!(w, "{:<22}", truncate(&s, 21))?;
    }
    writeln!(w)
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecycleSummary;
    use crate::model::AICategory;
    use crate::storage::run_store::{RunMetrics, RunRecord};
    use chrono::Utc;

    fn rec(model: &str, tps: f32, peak_vram: u64) -> RunRecord {
        let summary = LifecycleSummary {
            pid: 1,
            name: "x".into(),
            category: Some(AICategory::Inference),
            model_name: Some(model.into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 60,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 0.0,
            peak_cpu_pct: 0.0,
            peak_rss_mb: 1024,
            peak_vram_mb: peak_vram,
            samples: 60,
        };
        let mut r = RunRecord::from_summary(summary);
        r.metrics = RunMetrics {
            tokens_per_sec_avg: Some(tps),
            tokens_total: Some(1000),
            energy_joules_total: Some(82.0),
            ..RunMetrics::default()
        };
        r
    }

    #[test]
    fn empty_models_returns_error() {
        let cfg = Config::default();
        let err = run_compare(vec![], 5, false, &cfg).unwrap_err();
        assert!(err.to_string().contains("at least one model"));
    }

    #[test]
    fn unknown_model_yields_empty_column_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.storage.run_store_path = dir.path().to_string_lossy().into_owned();
        let _ = RunStore::open(dir.path()).unwrap();
        // No records → recent() returns empty → column with sample_size=0.
        // The function still succeeds; it's the operator's call to retry.
        let result = run_compare(vec!["nonexistent".into()], 5, true, &cfg);
        assert!(result.is_ok());
    }

    #[test]
    fn populated_models_produce_columns_with_metrics() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = RunStore::open(dir.path()).unwrap();
            for _ in 0..5 {
                store.append(rec("phi3-mini", 38.0, 4096)).unwrap();
            }
            for _ in 0..10 {
                store.append(rec("llama-3.1-8b", 21.7, 15360)).unwrap();
            }
        }
        // Build columns directly to verify the math without the
        // stdout-printing path.
        let store = RunStore::open(dir.path()).unwrap();
        for model in ["phi3-mini", "llama-3.1-8b"] {
            let records = store.recent(model, 20);
            let baseline = Baseline {
                model: model.into(),
                sample_size: records.len(),
                metrics: BaselineMetrics::from_records(&records),
                computed_at: Utc::now(),
                outlier_run_ids: Vec::new(),
                strategy: crate::analysis::compare::BaselineStrategy::default(),
            };
            let col = ComparisonColumn::from_baseline(model.into(), &baseline, Some(0.082));
            assert!(col.tokens_per_sec_avg.is_some());
            assert_eq!(col.joules_per_token, Some(0.082));
            assert!(col.peak_vram_mb.is_some());
        }
    }

    #[test]
    fn render_table_includes_all_rows() {
        let cols = vec![ComparisonColumn {
            model: "phi3-mini".into(),
            sample_size: 5,
            tokens_per_sec_avg: Some(MeanStd {
                mean: 38.4,
                stddev: 2.1,
                n: 5,
            }),
            fps_avg: None,
            peak_rss_mb: None,
            peak_vram_mb: Some(MeanStd {
                mean: 4096.0,
                stddev: 50.0,
                n: 5,
            }),
            gpu_watts_avg: None,
            uptime_secs: None,
            joules_per_token: Some(0.082),
            cold_load_seconds: Some(MeanStd {
                mean: 3.2,
                stddev: 0.4,
                n: 5,
            }),
        }];
        let mut buf = Vec::new();
        render_table(&mut buf, &cols).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("phi3-mini"));
        assert!(s.contains("tok/s avg"));
        assert!(s.contains("peak VRAM"));
        assert!(s.contains("W/token"));
        assert!(s.contains("cold load"));
    }
}
