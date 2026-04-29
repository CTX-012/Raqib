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
            } else if records.is_empty() {
                // DESIGN_HANDOFF Principle 6 — the "no runs found"
                // line on its own strands a user with a typo. Listing
                // the models that *do* have runs makes the next step
                // obvious. JSON mode skips this branch on purpose
                // (callers already get an empty array, which is
                // unambiguous and easy to test against).
                let known = store.list_models();
                render_unknown_model(w, &m, &known)?;
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

/// Print a "no runs for <model>" message that lists the models we *do*
/// have, so a typo or model-name drift surfaces immediately. If the
/// store is genuinely empty, fall back to the same "try this" hint
/// the no-args path would have shown.
fn render_unknown_model(
    w: &mut impl Write,
    model: &str,
    known: &[String],
) -> std::io::Result<()> {
    if known.is_empty() {
        writeln!(w, "No runs found for model: {}", model)?;
        writeln!(w, "(In fact, no runs at all yet.)")?;
        writeln!(w)?;
        writeln!(
            w,
            "Try one of these in another terminal to populate history:"
        )?;
        writeln!(w, "    ollama run llama3 'hello'")?;
        writeln!(w, "    vllm serve <model>")?;
        writeln!(w, "    yolo predict model=yolov8n.pt source=...")?;
        return Ok(());
    }
    writeln!(w, "No runs found for model: {}", model)?;
    writeln!(w)?;
    writeln!(
        w,
        "Models with run history (run `edge_monitor history` for a summary):"
    )?;
    for name in known {
        writeln!(w, "    {}", name)?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "If the model you wanted isn't listed, the classifier may have \
         recorded it under a different name — check `edge_monitor history` \
         (no model) for the canonical labels."
    )?;
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
        // DESIGN_HANDOFF Principle 6 — empty states teach the
        // product. The blank "no run history yet" line was technically
        // correct and operationally useless: a first-time user sees
        // it and has no idea what to do next. Three concrete examples
        // (one LLM CLI, one server runtime, one vision runtime) plus
        // the `exec` wrapper cover the common starts.
        writeln!(w, "No run history yet.")?;
        writeln!(w)?;
        writeln!(
            w,
            "Try one of these in another terminal — edge_monitor will"
        )?;
        writeln!(w, "detect the workload automatically:")?;
        writeln!(w)?;
        writeln!(w, "    ollama run llama3 'hello'")?;
        writeln!(w, "    vllm serve <model>")?;
        writeln!(w, "    yolo predict model=yolov8n.pt source=...")?;
        writeln!(w)?;
        writeln!(
            w,
            "Or wrap an existing command so we capture stdout metrics too:"
        )?;
        writeln!(w, "    edge_monitor exec -- <your command>")?;
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
        // Now mostly unreachable: `run_history_to` routes empty
        // results through `render_unknown_model` which lists the
        // models that DO have runs. Leaving a sane fallback here so
        // direct callers (tests, future tooling) still get a clear
        // line instead of a blank one.
        writeln!(w, "No runs found for model: {}", model)?;
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
        if let Some(line) = format_concurrent_line(r) {
            writeln!(w, "     {}", line)?;
        }
    }
    Ok(())
}

/// Tier 3.4 — second-line annotation per latest.md spec example
/// `serving 8 concurrent (peak)  →  20.1 tok/s/req · 161 tok/s aggregate`.
///
/// Returns `None` for runs that never observed concurrency data (Ollama,
/// llama.cpp without busy-slot exposure, vision workloads, etc) so the
/// table stays compact for non-LLM history.
///
/// The "tok/s/req" arithmetic guards `concurrent_avg > 0`: dividing the
/// aggregate throughput by the time-weighted average concurrency yields
/// per-request throughput. When `concurrent_avg` is missing or zero we
/// fall back to the peak — the spec example uses `(peak)` annotation for
/// that case, so we print whichever divisor we used.
pub(crate) fn format_concurrent_line(r: &RunRecord) -> Option<String> {
    let peak = r.metrics.concurrent_requests_peak?;
    let aggregate_tps = r.metrics.tokens_per_sec_avg?;
    let avg = r.metrics.concurrent_requests_avg;

    let (per_req, divisor_label) = match avg {
        Some(a) if a > 0.0 => (Some(aggregate_tps / a), format!("{a:.1} avg")),
        _ if peak > 0 => (
            Some(aggregate_tps / peak as f32),
            format!("{peak} peak"),
        ),
        _ => (None, "0".into()),
    };
    let waiting_suffix = match r.metrics.concurrent_requests_waiting_peak {
        Some(w) if w > 0 => format!(" · queue peak {w}"),
        _ => String::new(),
    };
    match per_req {
        Some(per) => Some(format!(
            "serving {peak} concurrent (peak; {divisor_label})  →  \
             {per:.1} tok/s/req · {aggregate_tps:.1} tok/s aggregate{waiting_suffix}"
        )),
        None => Some(format!(
            "serving {peak} concurrent (peak)  →  \
             {aggregate_tps:.1} tok/s aggregate{waiting_suffix}"
        )),
    }
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
        ExitReason::Segfault => "segfault".into(),
        ExitReason::OutOfMemory { ram, vram } => match (ram, vram) {
            (true, true) => "oom(ram+vram)".into(),
            (true, false) => "oom(ram)".into(),
            (false, true) => "oom(vram)".into(),
            (false, false) => "oom".into(),
        },
        ExitReason::CudaError { .. } => "cuda_error".into(),
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

    /// Empty-store empty-state per DESIGN_HANDOFF Principle 6 — the
    /// banner line plus at least one example command point a
    /// first-time user at the next concrete thing to try.
    #[test]
    fn empty_store_teaches_with_example_commands() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::open(dir.path()).unwrap();
        let summaries = build_model_summaries(&store);
        let mut buf = Vec::new();
        render_models(&mut buf, &summaries).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("No run history yet"),
            "expected the banner line; got: {out}"
        );
        assert!(
            out.contains("ollama run") || out.contains("vllm serve"),
            "empty-state must include at least one example command; got: {out}"
        );
        assert!(
            out.contains("edge_monitor exec"),
            "empty-state must mention the exec wrapper; got: {out}"
        );
    }

    /// Direct-`render_runs` empty-state fallback. The CLI surface
    /// routes empty results through `render_unknown_model` instead;
    /// this test exists for direct callers (future tooling, in-tree
    /// tests).
    #[test]
    fn render_runs_empty_falls_back_cleanly() {
        let mut buf = Vec::new();
        render_runs(&mut buf, "phi3-mini", &[]).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("No runs found for model: phi3-mini"),
            "got: {out}"
        );
    }

    /// Unknown-model empty-state with a populated store: list the
    /// models that DO have history so the user can spot a typo or
    /// model-name drift.
    #[test]
    fn unknown_model_lists_known_models() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        store.append(record_for("phi3-mini", None)).unwrap();
        store.append(record_for("llama-3.1-8b", None)).unwrap();
        let known = store.list_models();
        let mut buf = Vec::new();
        render_unknown_model(&mut buf, "phi3-min", &known).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("No runs found for model: phi3-min"));
        // Both real model names appear so a typo's correct match is
        // visible at a glance.
        assert!(out.contains("phi3-mini"), "got: {out}");
        assert!(out.contains("llama-3.1-8b"), "got: {out}");
        // And the line that points at `history` (no model) for the
        // canonical labels.
        assert!(out.contains("edge_monitor history"), "got: {out}");
    }

    /// Unknown-model empty-state with an empty store: list nothing
    /// (because there's nothing to list) and instead repeat the
    /// "try this" hint from the no-args empty path.
    #[test]
    fn unknown_model_with_empty_store_shows_try_this_hint() {
        let mut buf = Vec::new();
        render_unknown_model(&mut buf, "phi3-mini", &[]).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("No runs found for model: phi3-mini"));
        assert!(
            out.contains("ollama run") || out.contains("vllm serve"),
            "with no models known, fall back to launch examples; got: {out}"
        );
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

    /// Helper to build a record with concurrency telemetry populated —
    /// used by the Tier 3.4 history-rendering tests below.
    fn record_with_concurrency(
        peak: Option<u32>,
        avg: Option<f32>,
        tps: Option<f32>,
        waiting_peak: Option<u32>,
    ) -> RunRecord {
        let mut r = record_for("phi3-mini", None);
        r.metrics.concurrent_requests_peak = peak;
        r.metrics.concurrent_requests_avg = avg;
        r.metrics.tokens_per_sec_avg = tps;
        r.metrics.concurrent_requests_waiting_peak = waiting_peak;
        r
    }

    /// Tier 3.4 — spec example numbers exactly:
    /// "1 req for 10 s, 8 for 50 s" → time-weighted avg ≈ 6.833,
    /// aggregate 161 tok/s ÷ 6.833 avg ≈ 23.6 tok/s/req.
    /// Spec headline phrasing also requires the words "concurrent"
    /// "tok/s/req" and "tok/s aggregate" to appear.
    #[test]
    fn tier_34_format_concurrent_line_uses_avg_when_present() {
        let r = record_with_concurrency(Some(8), Some(6.833), Some(161.0), None);
        let line = format_concurrent_line(&r).expect("concurrency populated");
        assert!(line.contains("8 concurrent (peak"), "got: {line}");
        assert!(line.contains("avg"), "got: {line}"); // names the divisor
        assert!(line.contains("tok/s/req"), "got: {line}");
        assert!(line.contains("tok/s aggregate"), "got: {line}");
        // 161.0 / 6.833 ≈ 23.6
        assert!(line.contains("23.6"), "got: {line}");
        assert!(line.contains("161.0"), "got: {line}");
    }

    /// When the time-weighted avg is missing we fall back to peak as
    /// the divisor — but the line must say so explicitly so a reader
    /// doesn't confuse "8 (avg)" with "8 (peak)".
    #[test]
    fn tier_34_format_concurrent_line_falls_back_to_peak_divisor() {
        let r = record_with_concurrency(Some(1), None, Some(158.0), None);
        let line = format_concurrent_line(&r).unwrap();
        assert!(line.contains("1 peak"), "got: {line}");
        // 158 / 1 = 158.0 tok/s/req
        assert!(line.contains("158.0 tok/s/req"), "got: {line}");
    }

    /// Concurrency=0 with non-null peak (idle vLLM scraped at the
    /// wrong moment): we still show `0 concurrent` but skip the
    /// per-request divisor — a tok/s/req of "inf" or "NaN" would be
    /// misleading.
    #[test]
    fn tier_34_zero_peak_skips_per_request_divisor() {
        let r = record_with_concurrency(Some(0), Some(0.0), Some(12.0), None);
        let line = format_concurrent_line(&r).unwrap();
        assert!(line.contains("0 concurrent"), "got: {line}");
        assert!(!line.contains("tok/s/req"), "must not divide by zero: {line}");
        assert!(line.contains("12.0 tok/s aggregate"), "got: {line}");
    }

    /// No concurrency telemetry → no second line. Vision / Ollama /
    /// llama.cpp without busy-slot exposure end up here. The whole
    /// idea is that the table stays compact for non-LLM history.
    #[test]
    fn tier_34_no_telemetry_renders_no_extra_line() {
        let r = record_with_concurrency(None, None, None, None);
        assert!(format_concurrent_line(&r).is_none());
    }

    /// Queue depth surfaces only when non-zero, and only as a suffix
    /// on the existing line (not a separate row).
    #[test]
    fn tier_34_waiting_peak_appended_when_nonzero() {
        let r = record_with_concurrency(Some(8), Some(6.833), Some(161.0), Some(4));
        let line = format_concurrent_line(&r).unwrap();
        assert!(line.contains("queue peak 4"), "got: {line}");

        let r2 = record_with_concurrency(Some(8), Some(6.833), Some(161.0), Some(0));
        let line2 = format_concurrent_line(&r2).unwrap();
        assert!(!line2.contains("queue peak"), "0 must be suppressed: {line2}");
    }

    /// End-to-end: render_runs emits the second line below the row.
    #[test]
    fn tier_34_render_runs_emits_concurrent_line_under_row() {
        let r = record_with_concurrency(Some(8), Some(6.833), Some(161.0), Some(2));
        let mut buf = Vec::new();
        render_runs(&mut buf, "phi3-mini", &[r]).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // The second line is indented under the row.
        assert!(out.contains("8 concurrent (peak"), "got:\n{out}");
        assert!(out.contains("23.6 tok/s/req"), "got:\n{out}");
        assert!(out.contains("queue peak 2"), "got:\n{out}");
    }
}
