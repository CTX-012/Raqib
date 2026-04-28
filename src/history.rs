//! `edge_monitor history [MODEL] [--limit N] [--json]` — Tier 1.1 CLI
//! per latest.md.
//!
//! Reads from `RunStore`. Two modes:
//!
//! * **No model:** prints a model-summary table — one row per model with
//!   run count, last run timestamp, last exit status. Quick "what models
//!   have I run lately" overview.
//! * **With model:** prints the most recent N runs for that model with
//!   peak metrics. The default limit (20) matches latest.md's example
//!   output.
//!
//! `--json` emits structured output for scripting; the JSON shape is
//! `Vec<RunRecord>` (with model) or `Vec<ModelSummary>` (without model).
//! Both shapes are serde-derived from the live structs, so any future
//! field added to `RunRecord` shows up automatically.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::storage::run_store::{ExitReason, RunRecord, RunStore};

/// Entry point invoked from `main.rs` when the user types
/// `edge_monitor history [...]`. Writes to stdout.
pub fn run_history(
    model: Option<String>,
    limit: usize,
    json: bool,
    config: &Config,
) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    run_history_to(model, limit, json, config, &mut w)
}

/// Same as [`run_history`] with an explicit writer. Public so integration
/// tests (and downstream tooling) can capture the output without
/// shelling out to the binary.
pub fn run_history_to<W: Write>(
    model: Option<String>,
    limit: usize,
    json: bool,
    config: &Config,
    w: &mut W,
) -> anyhow::Result<()> {
    let path = config
        .storage
        .run_store()
        .ok_or_else(|| anyhow!("storage.run_store_path is empty; nothing to read"))?;

    let store = open_or_explain(&path)?;

    match model {
        Some(m) => {
            let records = store.recent(&m, limit);
            if json {
                serde_json::to_writer_pretty(&mut *w, &records)?;
                writeln!(w)?;
            } else {
                render_runs(w, &m, &records)?;
            }
        }
        None => {
            let summaries = build_model_summaries(&store);
            if json {
                serde_json::to_writer_pretty(&mut *w, &summaries)?;
                writeln!(w)?;
            } else {
                render_models(w, &summaries)?;
            }
        }
    }
    Ok(())
}

/// Empty store is not an error. The user probably hasn't run anything
/// yet — print a hint and exit 0.
fn open_or_explain(path: &Path) -> anyhow::Result<RunStore> {
    RunStore::open(path).with_context(|| format!("opening run store at {}", path.display()))
}

/// One row in the no-model summary table. Public-via-serde so the
/// `--json` output is well-typed for scripting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSummary {
    pub model: String,
    pub run_count: usize,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

/// Build one [`ModelSummary`] per known model. `run_count` walks the
/// per-model index slice (cheap), and `last_run_at` / `last_status`
/// come from one record-file read per model — fine for the typical
/// handful-of-models case the CLI is invoked on.
pub fn build_model_summaries(store: &RunStore) -> Vec<ModelSummary> {
    store
        .list_models()
        .into_iter()
        .map(|model| {
            let recent = store.recent(&model, 1);
            let (last_run_at, last_status) = match recent.first() {
                Some(r) => (
                    Some(r.summary.exit_time),
                    Some(format_exit_short(&r.exit_reason)),
                ),
                None => (None, None),
            };
            let run_count = store.recent(&model, usize::MAX).len();
            ModelSummary {
                model,
                run_count,
                last_run_at,
                last_status,
            }
        })
        .collect()
}

fn render_models(w: &mut impl Write, summaries: &[ModelSummary]) -> std::io::Result<()> {
    if summaries.is_empty() {
        writeln!(w, "no run history yet — run an AI workload first")?;
        return Ok(());
    }
    writeln!(
        w,
        "{:<40}  {:>5}  {:<19}  Last status",
        "Model", "Runs", "Last run (UTC)"
    )?;
    writeln!(w, "{}", "─".repeat(85))?;
    for s in summaries {
        let when = s
            .last_run_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".into());
        let status = s.last_status.as_deref().unwrap_or("-");
        writeln!(
            w,
            "{:<40}  {:>5}  {:<19}  {}",
            truncate(&s.model, 40),
            s.run_count,
            when,
            status
        )?;
    }
    Ok(())
}

fn render_runs(w: &mut impl Write, model: &str, records: &[RunRecord]) -> std::io::Result<()> {
    if records.is_empty() {
        writeln!(w, "no runs found for model: {}", model)?;
        return Ok(());
    }
    writeln!(
        w,
        "History: {} (showing {} most-recent runs)",
        model,
        records.len()
    )?;
    writeln!(w)?;
    writeln!(
        w,
        "{:>3}  {:<19}  {:>6}  {:>9}  {:>9}  {:>10}  Exit",
        "#", "When (UTC)", "Dur", "Avg CPU", "Peak RSS", "Peak VRAM"
    )?;
    writeln!(w, "{}", "─".repeat(85))?;
    // Records arrive newest-first from RunStore::recent.
    for (i, r) in records.iter().enumerate() {
        let idx = records.len() - i;
        let when = r.summary.exit_time.format("%Y-%m-%d %H:%M:%S");
        let dur = format_duration_short(r.summary.uptime_secs);
        writeln!(
            w,
            "{:>3}  {:<19}  {:>6}  {:>8.0}%  {:>7}MB  {:>8}MB  {}",
            idx,
            when,
            dur,
            r.summary.avg_cpu_pct,
            r.summary.peak_rss_mb,
            r.summary.peak_vram_mb,
            format_exit_short(&r.exit_reason)
        )?;
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Take by char boundary so multi-byte UTF-8 doesn't panic.
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

fn format_duration_short(secs: i64) -> String {
    if secs < 0 {
        return "?".into();
    }
    let s = secs as u64;
    if s < 60 {
        return format!("{}s", s);
    }
    let m = s / 60;
    let r = s % 60;
    if m < 60 {
        return format!("{}m{:02}s", m, r);
    }
    let h = m / 60;
    let mr = m % 60;
    format!("{}h{:02}m", h, mr)
}

/// Compact one-token exit status for tables.
pub fn format_exit_short(reason: &ExitReason) -> String {
    match reason {
        ExitReason::CleanExit => "clean".into(),
        ExitReason::UserSignal { signal } => format!("signal({})", signal),
        ExitReason::GovernorKill { .. } => "governor".into(),
        ExitReason::Crash { exit_code } => format!("crash({})", exit_code),
        ExitReason::Unknown => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecycleSummary;
    use crate::model::AICategory;
    use chrono::Utc;

    fn record_for(model: &str, signal: Option<i32>) -> RunRecord {
        let summary = LifecycleSummary {
            pid: 1,
            name: "python".into(),
            category: Some(AICategory::Inference),
            model_name: Some(model.into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 42,
            exit_code: if signal.is_some() { None } else { Some(0) },
            signal,
            avg_cpu_pct: 50.0,
            peak_cpu_pct: 80.0,
            peak_rss_mb: 512,
            peak_vram_mb: 0,
            samples: 42,
        };
        RunRecord::from_summary(summary)
    }

    /// Spec test: history command with no runs prints "no history" / 0 exit.
    #[test]
    fn empty_store_prints_no_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::open(dir.path()).unwrap();
        let summaries = build_model_summaries(&store);
        let mut buf = Vec::new();
        render_models(&mut buf, &summaries).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("no run history yet"), "got: {out}");
    }

    #[test]
    fn no_runs_for_model_prints_message() {
        let mut buf = Vec::new();
        render_runs(&mut buf, "phi3-mini", &[]).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("no runs found for model: phi3-mini"));
    }

    /// Spec test: `--json` output validates as well-formed JSON for both
    /// model-summary and per-model record list shapes.
    #[test]
    fn json_output_round_trips_for_records() {
        let recs = vec![
            record_for("phi3-mini", None),
            record_for("phi3-mini", Some(15)),
        ];
        let json = serde_json::to_string(&recs).unwrap();
        let parsed: Vec<RunRecord> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].summary.name, "python");
    }

    #[test]
    fn json_output_round_trips_for_model_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        store.append(record_for("phi3-mini", None)).unwrap();
        store.append(record_for("phi3-mini", Some(15))).unwrap();
        store.append(record_for("yolov8n", None)).unwrap();
        let summaries = build_model_summaries(&store);
        let json = serde_json::to_string(&summaries).unwrap();
        let parsed: Vec<ModelSummary> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        let phi = parsed.iter().find(|s| s.model == "phi3-mini").unwrap();
        assert_eq!(phi.run_count, 2);
    }

    #[test]
    fn run_table_includes_index_count_and_status() {
        let recs = vec![
            record_for("phi3-mini", None),
            record_for("phi3-mini", Some(9)),
        ];
        let mut buf = Vec::new();
        render_runs(&mut buf, "phi3-mini", &recs).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("phi3-mini"));
        assert!(out.contains("clean"));
        assert!(out.contains("signal(9)"));
        assert!(out.contains("# 2") || out.contains("  2"));
    }

    #[test]
    fn duration_formatting_handles_seconds_minutes_hours() {
        assert_eq!(format_duration_short(0), "0s");
        assert_eq!(format_duration_short(45), "45s");
        assert_eq!(format_duration_short(125), "2m05s");
        assert_eq!(format_duration_short(3725), "1h02m");
        assert_eq!(format_duration_short(-1), "?");
    }

    #[test]
    fn exit_short_covers_all_variants() {
        assert_eq!(format_exit_short(&ExitReason::CleanExit), "clean");
        assert_eq!(
            format_exit_short(&ExitReason::UserSignal { signal: 15 }),
            "signal(15)"
        );
        assert_eq!(
            format_exit_short(&ExitReason::GovernorKill { reason: "x".into() }),
            "governor"
        );
        assert_eq!(
            format_exit_short(&ExitReason::Crash { exit_code: 139 }),
            "crash(139)"
        );
        assert_eq!(format_exit_short(&ExitReason::Unknown), "unknown");
    }
}
