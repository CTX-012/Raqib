//! Sprint-4 B13 + B14 — history overlay scope + content tests.
//!
//! Pins two coupled invariants:
//!
//!   B13 — the history overlay shows ONLY completed/killed runs.
//!         Currently this is guaranteed by structure: `RunRecord` is
//!         built from `LifecycleSummary`, which always has a concrete
//!         `exit_time: DateTime<Utc>` (not `Option`). The dispatch
//!         hypothesised that active workloads might leak into the
//!         overlay through some other path; investigation found no
//!         such path. These tests serve as regression guards — if a
//!         future change ever adds a "pending" / "active" variant
//!         (e.g., `exit_time: Option<…>` or a `running` enum), the
//!         tests fail loudly and force a re-evaluation of the
//!         display contract.
//!
//!   B14 — per-run metric detail (AvgCPU, PeakRSS, PeakVRAM, the new
//!         Peak CPU) renders in the post-mortem card body, not as
//!         columns on the history overlay. The history overlay is
//!         a chronological list (When / Dur / Exit); the per-run
//!         detail card opens via Enter on the focused workload row.

use chrono::Utc;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use edge_monitor::config::Config;
use edge_monitor::lifecycle::LifecycleSummary;
use edge_monitor::model::AICategory;
use edge_monitor::runtime::Runtime;
use edge_monitor::storage::RunRecord;
use edge_monitor::storage::run_store::ExitReason;
use edge_monitor::ui::app::App;
use edge_monitor::ui::panels;
use edge_monitor::ui::panels::postmortem::{
    BaselineStatus, PostMortem, PostMortemCard, build_lines_themed,
};
use edge_monitor::ui::theme::current_theme;
use std::time::Instant;

// ── B13 — history scope: only completed/killed runs ─────────────────

fn fixture_summary(name: &str, model: &str, signal: Option<i32>) -> LifecycleSummary {
    LifecycleSummary {
        pid: 4242,
        name: name.to_string(),
        category: Some(AICategory::Inference),
        model_name: Some(model.to_string()),
        spawn_time: Utc::now(),
        // B13 — exit_time is `DateTime<Utc>` (not Option). Every
        // LifecycleSummary IS by structure a completed run. This
        // field is the regression-guard surface: if it ever becomes
        // optional, the next assertion line breaks.
        exit_time: Utc::now(),
        uptime_secs: 65,
        exit_code: if signal.is_some() { None } else { Some(0) },
        signal,
        avg_cpu_pct: 38.4,
        peak_cpu_pct: 52.1,
        peak_rss_mb: 1024,
        peak_vram_mb: 4096,
        samples: 60,
    }
}

#[test]
fn run_record_carries_concrete_exit_time_b13() {
    // The B13 invariant lives at the type level: LifecycleSummary's
    // exit_time is not Option, so every RunRecord that the history
    // overlay reads is an exit record by construction. No filter
    // needed in the overlay; the type system already guarantees it.
    let rec = RunRecord::from_summary(fixture_summary("python-1", "phi3-mini", None));
    // `_` to silence unused — the assertion is that the field is
    // present and concrete (compilation alone proves it). The
    // value access pins that we're reading the same `exit_time` the
    // overlay reads at render time.
    let _ts: chrono::DateTime<chrono::Utc> = rec.summary.exit_time;
}

#[test]
fn run_record_for_clean_exit_classifies_as_cleanexit_b13() {
    let rec = RunRecord::from_summary(fixture_summary("python-1", "phi3-mini", None));
    assert!(
        matches!(rec.exit_reason, ExitReason::CleanExit),
        "no signal + exit_code=0 should classify as CleanExit; got {:?}",
        rec.exit_reason,
    );
}

#[test]
fn run_record_for_killed_run_classifies_as_user_signal_b13() {
    // SIGTERM (15) is the governor's standard kill signal. Pin that
    // killed runs land in the UserSignal variant so the overlay's
    // `clean / governor / other` color-code mapping reads correctly.
    let rec = RunRecord::from_summary(fixture_summary("python-1", "phi3-mini", Some(15)));
    assert!(
        matches!(rec.exit_reason, ExitReason::UserSignal { signal: 15 }),
        "signal-terminated run should classify as UserSignal; got {:?}",
        rec.exit_reason,
    );
}

// ── B14 — history columns drop / post-mortem gains metrics ──────────

fn fixture_postmortem(display_name: &str) -> PostMortem {
    PostMortem {
        display_name: display_name.into(),
        duration_secs: 65,
        avg_cpu_pct: 38.4,
        peak_cpu_pct: 52.1,
        peak_rss_mb: 1024,
        peak_vram_mb: 4096,
        tokens_per_sec: Some(38.4),
        workload_category: Some(edge_monitor::model::WorkloadCategory::LLM),
        exit_reason: ExitReason::CleanExit,
        stderr_tail: Vec::new(),
        baseline_status: BaselineStatus::NotAvailable,
    }
}

fn fixture_postmortem_card() -> PostMortemCard {
    PostMortemCard {
        post_mortem: fixture_postmortem("phi3-mini"),
        shown_at: Instant::now(),
        pid: None,
    }
}

fn render_history_overlay() -> String {
    // Drive the full panels::render path with the history overlay
    // open; the overlay is private at the module level, but its
    // render call lives inside `panels::render` and lands on the
    // same buffer the integration tests already exercise.
    let theme = current_theme("dark");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default());
    let mut app = App::new();
    // Supply at least one record so the header_paragraph renders
    // the column legend (the empty-state path renders the
    // `ux_contract::empty::HISTORY` string instead).
    let rec = RunRecord::from_summary(fixture_summary("python-1", "phi3-mini", None));
    app.open_history("phi3-mini".into(), vec![rec]);
    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, &theme, None, None))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..40 {
        for x in 0..120 {
            out.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn history_overlay_columns_are_when_duration_exit_b14() {
    let rendered = render_history_overlay();
    // The post-B14 column legend reads exactly `# When  Dur  Exit`.
    // Pin the substring so a future "add column" change has to
    // walk through this regression guard.
    assert!(
        rendered.contains("# When  Dur  Exit"),
        "post-B14 history overlay must use # When / Dur / Exit columns:\n{rendered}",
    );
}

#[test]
fn history_overlay_does_not_show_avg_cpu_column_b14() {
    let rendered = render_history_overlay();
    assert!(
        !rendered.contains("AvgCPU"),
        "B14 — AvgCPU column moved into the post-mortem card body:\n{rendered}",
    );
}

#[test]
fn history_overlay_does_not_show_peak_rss_column_b14() {
    let rendered = render_history_overlay();
    assert!(
        !rendered.contains("PeakRSS"),
        "B14 — PeakRSS column moved into the post-mortem card body:\n{rendered}",
    );
}

#[test]
fn history_overlay_does_not_show_peak_vram_column_b14() {
    let rendered = render_history_overlay();
    assert!(
        !rendered.contains("PeakVRAM"),
        "B14 — PeakVRAM column moved into the post-mortem card body:\n{rendered}",
    );
}

#[test]
fn post_mortem_card_displays_avg_cpu_b14() {
    let theme = current_theme("dark");
    let lines = build_lines_themed(&fixture_postmortem_card(), &theme);
    let rendered: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Avg CPU:") && rendered.contains("38.4%"),
        "post-mortem card body must render Avg CPU per B14:\n{rendered}",
    );
}

#[test]
fn post_mortem_card_displays_peak_cpu_b14() {
    // B14 — new field: peak_cpu_pct plumbed from LifecycleSummary
    // into the card body. The history overlay used to surface this
    // as a column; B14 moves it where the rest of the per-run
    // metric detail already lived.
    let theme = current_theme("dark");
    let lines = build_lines_themed(&fixture_postmortem_card(), &theme);
    let rendered: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Peak CPU:") && rendered.contains("52.1%"),
        "post-mortem card body must render Peak CPU per B14:\n{rendered}",
    );
}

#[test]
fn post_mortem_card_displays_peak_rss_b14() {
    let theme = current_theme("dark");
    let lines = build_lines_themed(&fixture_postmortem_card(), &theme);
    let rendered: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Peak RAM:"),
        "post-mortem card body must render Peak RAM per B14:\n{rendered}",
    );
}

#[test]
fn post_mortem_card_displays_peak_vram_b14() {
    let theme = current_theme("dark");
    let lines = build_lines_themed(&fixture_postmortem_card(), &theme);
    let rendered: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Peak GPU memory:") && rendered.contains("4.0 GB"),
        "post-mortem card body must render Peak GPU memory per B14:\n{rendered}",
    );
}
