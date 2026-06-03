//! Sprint-4 FIX 1 — panel-adjacency spacer regression tests.
//!
//! Bundle-3 (044fe84) introduced the B7 spacer between Vitals and AI
//! Workloads. Sprint-4 extended the pattern to Workloads↔Top and
//! Top↔Activity in Standard/Wide tier, and Workloads↔Activity in
//! Narrow.
//!
//! These tests pin that the spacer ROWS render as blank cells in the
//! side slivers (cols 0 and last) regardless of whether a card
//! overlay is open. The user-reported "Vitals merges with adjacent
//! panel when a card opens" symptom didn't reproduce against
//! Vitals — the B7 spacer survived every card overlay in
//! `examples/layout_repro.rs` — but Workloads↔Top and Top↔Activity
//! had no spacer at all, so the dashboard slivers visible AROUND a
//! centered card showed those panels stacked border-to-border. This
//! suite locks the fix.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use edge_monitor::config::Config;
use edge_monitor::runtime::Runtime;
use edge_monitor::storage::run_store::ExitReason;
use edge_monitor::ui::app::App;
use edge_monitor::ui::panels;
use edge_monitor::ui::panels::kill_confirm::KillConfirmCard;
use edge_monitor::ui::panels::postmortem::{BaselineStatus, PostMortem, PostMortemCard};
use edge_monitor::ui::theme::current_theme;
use std::time::Instant;

/// Render a full dashboard at Standard 120×40 and read row `y` back
/// as a `Vec<String>` of cell symbols. Tests assert against specific
/// columns (0 = left sliver, 119 = right sliver) to verify the
/// spacer row renders blank OUTSIDE any overlay's column range.
fn rendered_row_cells(setup: impl FnOnce(&mut App), y: u16) -> Vec<String> {
    let theme = current_theme("dark");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
    let mut app = App::new();
    setup(&mut app);
    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, &theme, None, None))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..120)
        .map(|x| buffer.cell((x, y)).expect("cell").symbol().to_string())
        .collect()
}

/// Sprint-4 row map (Standard tier 120×40 with no alert banner):
///
///   row  0      §0 mission-line header
///   rows 1–7    Vitals panel (7)
///   row  8      B7 spacer (Vitals → Workloads)
///   rows 9–22   AI Workloads (Min=6 + flex, ~14 rows)
///   row  23     FIX-1 spacer (Workloads → Top processes)
///   rows 24–30  Top processes (7)
///   row  31     FIX-1 spacer (Top → Activity)
///   rows 32–38  Activity (7)
///   row  39     hint footer
const VITALS_WORKLOADS_SPACER_Y: u16 = 8;
const WORKLOADS_TOP_SPACER_Y: u16 = 23;
const TOP_ACTIVITY_SPACER_Y: u16 = 31;

fn assert_spacer_visible_in_slivers(cells: &[String], context: &str) {
    // Outside any centered overlay the leftmost and rightmost cells
    // must be blank. The exact cols here come from §12: a card with
    // CARD_WIDTH=64 centered in 120 cols leaves cols 0..28 and
    // cols 92..120 uncovered. Probing cols 0 and 119 is the
    // tightest guarantee both ends of the row render blank.
    assert_eq!(
        cells[0], " ",
        "{context}: spacer row should be blank at col 0, got {:?}",
        cells[0]
    );
    assert_eq!(
        cells[119], " ",
        "{context}: spacer row should be blank at col 119, got {:?}",
        cells[119]
    );
}

fn fake_postmortem_card() -> PostMortemCard {
    PostMortemCard {
        post_mortem: PostMortem {
            display_name: "phi3-mini".into(),
            duration_secs: 65,
            avg_cpu_pct: 38.4,
            peak_cpu_pct: 52.1,
            peak_rss_mb: 1024,
            peak_vram_mb: 4096,
            tokens_per_sec: Some(38.4),
            workload_category: None,
            exit_reason: ExitReason::CleanExit,
            stderr_tail: Vec::new(),
            baseline_status: BaselineStatus::NotAvailable,
        },
        shown_at: Instant::now(),
        pid: None,
    }
}

fn fake_kill_confirm_card() -> KillConfirmCard {
    KillConfirmCard::new(
        "phi3-mini".into(),
        4242,
        "LLM".into(),
        "Running".into(),
        42,
        17.0,
        512,
        None,
        false,
    )
}

// ── B7 — Vitals → Workloads spacer (Bundle-3) ──────────────────────

#[test]
fn vitals_workloads_spacer_visible_without_card() {
    let cells = rendered_row_cells(|_app| {}, VITALS_WORKLOADS_SPACER_Y);
    assert_spacer_visible_in_slivers(&cells, "no-card / Vitals→Workloads spacer");
}

#[test]
fn vitals_does_not_merge_when_kill_confirm_card_open() {
    let cells = rendered_row_cells(
        |app| {
            app.open_kill_confirm(fake_kill_confirm_card());
        },
        VITALS_WORKLOADS_SPACER_Y,
    );
    assert_spacer_visible_in_slivers(&cells, "kill_confirm / Vitals→Workloads spacer");
}

#[test]
fn vitals_does_not_merge_when_post_mortem_card_open() {
    let cells = rendered_row_cells(
        |app| {
            app.show_postmortem(fake_postmortem_card());
        },
        VITALS_WORKLOADS_SPACER_Y,
    );
    assert_spacer_visible_in_slivers(&cells, "post_mortem / Vitals→Workloads spacer");
}

#[test]
fn vitals_does_not_merge_when_history_overlay_open() {
    let cells = rendered_row_cells(
        |app| {
            app.open_history("phi3-mini".into(), Vec::new());
        },
        VITALS_WORKLOADS_SPACER_Y,
    );
    assert_spacer_visible_in_slivers(&cells, "history_overlay / Vitals→Workloads spacer");
}

// ── Sprint-4 — Workloads → Top spacer ──────────────────────────────

#[test]
fn workloads_top_spacer_visible_without_card() {
    let cells = rendered_row_cells(|_app| {}, WORKLOADS_TOP_SPACER_Y);
    assert_spacer_visible_in_slivers(&cells, "no-card / Workloads→Top spacer");
}

#[test]
fn workloads_top_spacer_survives_kill_confirm_card() {
    // FIX-1 — the new Workloads→Top spacer is what the user
    // actually saw "merging" when a card opened. Pin it under
    // every card kind so a future layout edit can't silently
    // re-stack the borders.
    let cells = rendered_row_cells(
        |app| {
            app.open_kill_confirm(fake_kill_confirm_card());
        },
        WORKLOADS_TOP_SPACER_Y,
    );
    assert_spacer_visible_in_slivers(&cells, "kill_confirm / Workloads→Top spacer");
}

// ── Sprint-4 — Top → Activity spacer ───────────────────────────────

#[test]
fn top_activity_spacer_visible_without_card() {
    let cells = rendered_row_cells(|_app| {}, TOP_ACTIVITY_SPACER_Y);
    assert_spacer_visible_in_slivers(&cells, "no-card / Top→Activity spacer");
}

#[test]
fn top_activity_spacer_survives_post_mortem_card() {
    let cells = rendered_row_cells(
        |app| {
            app.show_postmortem(fake_postmortem_card());
        },
        TOP_ACTIVITY_SPACER_Y,
    );
    assert_spacer_visible_in_slivers(&cells, "post_mortem / Top→Activity spacer");
}
