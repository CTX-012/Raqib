//! L11b / UX_CONTRACT.md §1 region 4 — workloads panel.
//!
//! Renames + replaces the Phase-0 `registry.rs`. Groups AI processes
//! by `WorkloadCategory` (LLM → Vision → ROS2 → Embeddings →
//! Unknown), sorts within each group by status priority then PID,
//! renders one row per workload with:
//!
//!   `<status-dot> <name> <primary-metric> <secondary metrics>`
//!
//! Status dot routes through `app.symbol_set()` so the §15 ASCII
//! fallback applies. Primary metric varies by `WorkloadCategory` per
//! §2; the `Loading` `WorkloadStatus` overrides the type-specific
//! metric with `"cold-loading"`.
//!
//! ## Contract const adoption
//!
//! L11c (this row) consumes ux_contract v0.3.4 where Contract
//! shipped CAR-7 (`status::COLD_LOADING`) and CAR-8
//! (`workload_category::GROUP_HEADER_*`). The Loading-state primary
//! metric and the per-category group headers now route through the
//! contract const, not local literals. `WorkloadCategory` itself
//! stays a local enum — Contract refined CAR-8 to const-only and
//! the migration of the enum into the contract is filed as v1.1+
//! per BACKLOG.md.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use ux_contract::WorkloadStatus;
use ux_contract::status::COLD_LOADING;
use ux_contract::workload_category::{
    GROUP_HEADER_EMBEDDINGS, GROUP_HEADER_LLM, GROUP_HEADER_ROS2, GROUP_HEADER_UNKNOWN,
    GROUP_HEADER_VISION,
};

use crate::model::WorkloadCategory;
use crate::runtime::{
    AnnotatedProcess, RuntimeState, WorkloadStatusInputs, compute_workload_status,
};

use super::super::app::App;
use super::panel_block;

/// Local placeholder for the no-data primary metric — non-LLM
/// workloads without their type-specific metric stream show this.
/// Matches the contract's "(no metrics)" fallback in §2 but no
/// Contract const exists for it yet (filed in BACKLOG as a future
/// CAR; low priority since only one render site uses it).
const NO_METRICS: &str = "(no metrics)";

/// L11c — map the local `WorkloadCategory` enum to the v0.3.4
/// contract group-header const. Contract refined CAR-8 to
/// const-only headers; the enum stays local per the orchestrator's
/// "KEEP CONST-ONLY for v1.0" decision (the
/// WorkloadCategory-to-contract migration is v1.1+ per BACKLOG.md).
fn category_header(category: WorkloadCategory) -> &'static str {
    match category {
        WorkloadCategory::LLM => GROUP_HEADER_LLM,
        WorkloadCategory::Vision => GROUP_HEADER_VISION,
        WorkloadCategory::ROS2 => GROUP_HEADER_ROS2,
        WorkloadCategory::Embeddings => GROUP_HEADER_EMBEDDINGS,
        WorkloadCategory::Unknown => GROUP_HEADER_UNKNOWN,
    }
}

/// L11b — assemble alert/status inputs for one workload from the
/// runtime snapshot. Pure: takes everything it needs by reference.
/// Mirrors the L6 alert-observation logic so a single workload's
/// status dot and pressure alerts agree on the same numbers.
fn build_status_inputs(
    proc: &AnnotatedProcess,
    state: &RuntimeState,
    armed_pid: Option<u32>,
    now: Instant,
) -> WorkloadStatusInputs {
    let total_vram = state
        .last_snapshot
        .as_ref()
        .map(|s| s.gpu.total_vram_all_devices())
        .filter(|&v| v > 0);
    let vram_pct = match (total_vram, proc.vram_bytes) {
        (Some(total), Some(used)) => Some((used as f64 / total as f64) * 100.0),
        _ => None,
    };
    let ram_pct = state
        .last_snapshot
        .as_ref()
        .map(|s| s.system.memory_usage_percent());
    let kv_cache_pct = state
        .live_telemetry
        .get(&proc.pid)
        .and_then(|lt| lt.kv_cache_peak_pct.map(|v| v as f64));
    let governor_armed = armed_pid == Some(proc.pid);

    WorkloadStatusInputs {
        vram_pct,
        ram_pct,
        kv_cache_pct,
        // Throughput-vs-baseline isn't tracked for live workloads in
        // v0.3 (post-mortem captures it on exit via
        // `build_baseline_status`). Leave None until a future row
        // wires live baseline tracking.
        throughput_vs_baseline: None,
        governor_armed,
        // OOM is exit-driven (L8); doesn't apply to live workloads
        // that haven't exited.
        oom_detected: false,
        telemetry_age: now.saturating_duration_since(proc.first_observed_at),
    }
}

/// Sort key within a category. Critical < Attention < Healthy <
/// Loading; ties broken by PID ascending. Lower number = renders
/// first (top of group).
fn status_priority(status: WorkloadStatus) -> u8 {
    match status {
        WorkloadStatus::Critical => 0,
        WorkloadStatus::Attention => 1,
        WorkloadStatus::Healthy => 2,
        WorkloadStatus::Loading => 3,
    }
}

/// One ready-to-render row. Building it once at sort time and
/// reusing for both ordering + render keeps the categorisation +
/// status compute on a single pass.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub pid: u32,
    pub name: String,
    pub category: WorkloadCategory,
    pub status: WorkloadStatus,
    pub cpu_pct: f32,
    pub rss_mb: u64,
    pub vram_bytes: Option<u64>,
    pub kv_cache_pct: Option<f32>,
    /// L12 — VRAM percent of device total, captured at status-
    /// compute time so the degraded-row expansion renders the
    /// same number that drove the status dot.
    pub vram_pct: Option<f64>,
    /// L12 — host RAM utilisation. System-wide, so identical for
    /// every row in a tick; cached on each row for the same
    /// "expansion text matches status compute" invariant.
    pub ram_pct: Option<f64>,
    /// L12 — true when the manual-kill arm targets this PID. The
    /// degraded-line surfaces it as a discrete trigger so the
    /// operator can see why a Critical dot fired even when no
    /// numeric metric is breaching.
    pub governor_armed: bool,
}

/// Build the in-render-order row list. Crate-public for tests + the
/// app's selection logic so navigation stays in lock-step with the
/// rendered order.
pub(crate) fn ordered_rows(state: &RuntimeState, app: &App) -> Vec<Row> {
    let now = Instant::now();
    let armed = app.armed_kill_pid();
    let mut rows: Vec<Row> = state
        .ai_processes()
        .map(|p| {
            let inputs = build_status_inputs(p, state, armed, now);
            let status = compute_workload_status(&inputs);
            let kv_cache_pct = state
                .live_telemetry
                .get(&p.pid)
                .and_then(|lt| lt.kv_cache_peak_pct);
            Row {
                pid: p.pid,
                name: p.name.clone(),
                category: p.workload_category,
                status,
                cpu_pct: p.cpu_pct,
                rss_mb: p.rss_mb,
                vram_bytes: p.vram_bytes,
                kv_cache_pct,
                vram_pct: inputs.vram_pct,
                ram_pct: inputs.ram_pct,
                governor_armed: inputs.governor_armed,
            }
        })
        .collect();
    rows.sort_by_key(|r| {
        (
            r.category.display_order(),
            status_priority(r.status),
            r.pid,
        )
    });
    rows
}

/// Render-order PID list. The app's selection logic reads this so
/// `j`/`K` navigate through workloads in the same order they appear
/// on screen.
pub fn ordered_pids(state: &RuntimeState, app: &App) -> Vec<u32> {
    ordered_rows(state, app)
        .into_iter()
        .map(|r| r.pid)
        .collect()
}

/// L12 — expansion-line text for a degraded row.
///
/// Returns `Some(text)` only when the row's status is `Attention`
/// or `Critical`; `Healthy` and `Loading` return `None` (no
/// expansion). Text is a `·`-separated list of every breach
/// condition currently triggering — VRAM%, RAM%, KV%, plus the
/// instant-fire flags (`governor armed`, `OOM detected`).
///
/// Content-light placeholder vs UX_CONTRACT.md §2's locked
/// per-category schema (see CAR-9 in BACKLOG.md): the §2 schema
/// includes queue depth, p99 latency, live throughput, and live
/// baseline (with `±delta%`) — none of which the v1.0 data layer
/// tracks for live workloads yet. Once Contract ships
/// `degraded_line::*` const and the relevant telemetry features
/// land, a follow-up row swaps this helper for the contract
/// templates with the new fields populated.
pub(crate) fn degraded_line(row: &Row) -> Option<String> {
    use ux_contract::thresholds::{KV_ATTENTION_PCT, RAM_ATTENTION_PCT, VRAM_ATTENTION_PCT};

    if matches!(row.status, WorkloadStatus::Healthy | WorkloadStatus::Loading) {
        return None;
    }

    let mut triggers: Vec<String> = Vec::new();
    if row.governor_armed {
        triggers.push("governor armed".to_string());
    }
    if let Some(p) = row.vram_pct
        && p >= VRAM_ATTENTION_PCT
    {
        triggers.push(format!("VRAM {p:.0}%"));
    }
    if let Some(p) = row.ram_pct
        && p >= RAM_ATTENTION_PCT
    {
        triggers.push(format!("RAM {p:.0}%"));
    }
    if let Some(p) = row.kv_cache_pct
        && (p as f64) >= KV_ATTENTION_PCT
    {
        triggers.push(format!("KV {p:.0}%"));
    }

    if triggers.is_empty() {
        // Defensive: a row in Attention/Critical state without any
        // numeric trigger means a non-numeric signal escalated it
        // (post-L12: only `governor_armed` does this; future rows
        // may add others). Surface it honestly rather than render
        // an empty expansion line that looks like a bug.
        return Some("status elevated — no specific metric trigger".to_string());
    }
    Some(triggers.join(" · "))
}

/// Format the primary metric for a row. Returns `"cold-loading"`
/// when the row is in `Loading` state regardless of category;
/// otherwise category-specific (or `"(no metrics)"` when the
/// telemetry isn't available).
fn primary_metric(row: &Row) -> String {
    if matches!(row.status, WorkloadStatus::Loading) {
        return COLD_LOADING.to_string();
    }
    match row.category {
        // L11b doesn't yet wire live tok/s / fps / emb/s — those
        // come from `live_telemetry` once the samplers expose them
        // per category. Until then non-LLM categories report
        // `(no metrics)` and LLM reports KV cache when available.
        WorkloadCategory::LLM => match row.kv_cache_pct {
            Some(kv) => format!("KV {kv:>4.0}%"),
            None => NO_METRICS.to_string(),
        },
        WorkloadCategory::Vision
        | WorkloadCategory::ROS2
        | WorkloadCategory::Embeddings
        | WorkloadCategory::Unknown => NO_METRICS.to_string(),
    }
}

/// Render the whole panel: group headers (skipped when empty) + one
/// row per workload, in `ordered_rows` order.
pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let rows = ordered_rows(state, app);
    let block = panel_block("AI Workloads", true);

    if rows.is_empty() {
        // Total empty — render the contract's locked empty-state
        // copy. (`registry.rs`'s rich onboarding paragraph is
        // intentionally retired in L11b; the contract is the
        // source of truth for empty-state text.)
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", ux_contract::empty::WORKLOADS),
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ];
        let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    // Walk categories in contract order; emit a header line per
    // non-empty category, then its rows. Track which list-index
    // corresponds to each workload row so `selected_index` (which
    // counts only workloads, not headers) maps cleanly back into
    // the rendered list.
    let symbols = app.symbol_set();
    let mut items: Vec<ListItem<'_>> = Vec::with_capacity(rows.len() + 5);
    let mut row_index_to_list_index: Vec<usize> = Vec::with_capacity(rows.len());

    for category in WorkloadCategory::all_in_order() {
        let group: Vec<&Row> = rows.iter().filter(|r| r.category == category).collect();
        if group.is_empty() {
            // §1 region 4 — empty categories render no header.
            continue;
        }
        items.push(ListItem::new(Line::from(Span::styled(
            // L11c — contract const from `ux_contract::
            // workload_category::GROUP_HEADER_*` (v0.3.4). Theme
            // colour mapping still pending L21.
            category_header(category),
            Style::default().fg(Color::DarkGray),
        ))));

        for row in group {
            row_index_to_list_index.push(items.len());

            let dot = symbols.workload_status(row.status);
            let vram_label = match row.vram_bytes {
                Some(b) if b > 0 => format!("{:>4}M", b / (1024 * 1024)),
                _ => "    ".into(),
            };
            let primary = format!(
                "{} {:<24} {:<14} cpu {:>5.1}% rss {:>5}M {}",
                dot,
                truncate(&row.name, 24),
                primary_metric(row),
                row.cpu_pct,
                row.rss_mb,
                vram_label,
            );
            // L12 — combine primary + expansion into a single
            // ListItem so the highlight (selection bg) covers both
            // when this row is selected. The expansion's `Option`
            // shape keeps Healthy / Loading rows at their
            // pre-L12 single-line layout exactly.
            let mut lines = vec![Line::from(Span::raw(primary))];
            if let Some(expansion) = degraded_line(row) {
                lines.push(Line::from(Span::styled(
                    format!("    {expansion}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            items.push(ListItem::new(lines));
        }
    }

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    if !row_index_to_list_index.is_empty() {
        let selected_row = app
            .selected_index()
            .min(row_index_to_list_index.len() - 1);
        list_state.select(Some(row_index_to_list_index[selected_row]));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AICategory;
    use crate::runtime::AnnotatedProcess;
    use std::time::Duration;

    fn make_proc(
        pid: u32,
        name: &str,
        category: WorkloadCategory,
        first_observed_age: Duration,
    ) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: AICategory::Inference,
            workload_category: category,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            // Build first_observed_at backwards from "now" so the
            // panel's `Instant::now()` minus this lands at
            // `first_observed_age`.
            first_observed_at: Instant::now() - first_observed_age,
        }
    }

    fn make_row(category: WorkloadCategory, status: WorkloadStatus) -> Row {
        Row {
            pid: 1,
            name: "phi3".into(),
            category,
            status,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            kv_cache_pct: None,
            vram_pct: None,
            ram_pct: None,
            governor_armed: false,
        }
    }

    fn state_with(procs: Vec<AnnotatedProcess>) -> RuntimeState {
        RuntimeState {
            annotated: procs,
            ..Default::default()
        }
    }

    /// Long enough that the warmup gate is satisfied and the row
    /// resolves to a non-Loading status.
    fn warm() -> Duration {
        Duration::from_secs(120)
    }

    #[test]
    fn workloads_grouped_by_category_in_contract_order() {
        let app = App::new();
        let s = state_with(vec![
            make_proc(30, "phi3", WorkloadCategory::LLM, warm()),
            make_proc(10, "yolo", WorkloadCategory::Vision, warm()),
            make_proc(20, "bge", WorkloadCategory::Embeddings, warm()),
            make_proc(40, "????", WorkloadCategory::Unknown, warm()),
        ]);
        let cats: Vec<WorkloadCategory> =
            ordered_rows(&s, &app).iter().map(|r| r.category).collect();
        // Categories in contract order LLM → Vision → (ROS2 empty) →
        // Embeddings → Unknown.
        assert_eq!(
            cats,
            vec![
                WorkloadCategory::LLM,
                WorkloadCategory::Vision,
                WorkloadCategory::Embeddings,
                WorkloadCategory::Unknown,
            ]
        );
    }

    #[test]
    fn workloads_within_category_tiebreak_by_pid_asc() {
        let app = App::new();
        let s = state_with(vec![
            make_proc(50, "vllm-2", WorkloadCategory::LLM, warm()),
            make_proc(10, "vllm-1", WorkloadCategory::LLM, warm()),
            make_proc(30, "vllm-3", WorkloadCategory::LLM, warm()),
        ]);
        let pids: Vec<u32> = ordered_rows(&s, &app)
            .into_iter()
            .map(|r| r.pid)
            .collect();
        assert_eq!(pids, vec![10, 30, 50]);
    }

    #[test]
    fn workloads_within_category_sorted_by_status_priority() {
        // Two LLM rows, both warm. Arming the higher-PID row
        // escalates its status to Critical via `governor_armed`.
        let mut app = App::new();
        let s = state_with(vec![
            make_proc(10, "calm", WorkloadCategory::LLM, warm()),
            make_proc(99, "armed", WorkloadCategory::LLM, warm()),
        ]);
        app.arm_kill(crate::ui::panels::armed_banner::ArmedKill {
            pid: 99,
            name: "armed".into(),
            allowlisted: false,
            armed_at: Instant::now(),
        });
        let pids: Vec<u32> = ordered_rows(&s, &app)
            .into_iter()
            .map(|r| r.pid)
            .collect();
        assert_eq!(pids[0], 99, "Critical (armed) sorts above Healthy");
        assert_eq!(pids[1], 10);
    }

    #[test]
    fn loading_state_renders_cold_loading_metric_instead_of_type_specific() {
        // The Loading status overrides the category-specific
        // primary metric across all categories.
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Loading);
        row.kv_cache_pct = Some(45.0);
        assert_eq!(primary_metric(&row), COLD_LOADING);
        let vision = Row {
            category: WorkloadCategory::Vision,
            ..row.clone()
        };
        assert_eq!(primary_metric(&vision), COLD_LOADING);
    }

    #[test]
    fn warmup_gate_produces_loading_status_for_young_pids() {
        // End-to-end via compute_workload_status: a fresh PID with
        // telemetry_age < BASELINE_WARMUP_SECS resolves to Loading.
        let app = App::new();
        let s = state_with(vec![make_proc(
            1,
            "phi3",
            WorkloadCategory::LLM,
            Duration::from_secs(5),
        )]);
        let rows = ordered_rows(&s, &app);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, WorkloadStatus::Loading);
    }

    #[test]
    fn empty_total_renders_nothing_in_ordered_rows() {
        let s = state_with(vec![]);
        let rows = ordered_rows(&s, &App::new());
        assert!(rows.is_empty());
    }

    #[test]
    fn empty_category_does_not_appear_in_ordered_rows() {
        // ROS2 has no detection until L9 — categories without rows
        // are simply absent from the output (the render layer skips
        // empty group headers per §1 region 4).
        let app = App::new();
        let s = state_with(vec![
            make_proc(10, "phi3", WorkloadCategory::LLM, warm()),
            make_proc(20, "yolo", WorkloadCategory::Vision, warm()),
        ]);
        let cats: Vec<WorkloadCategory> =
            ordered_rows(&s, &app).iter().map(|r| r.category).collect();
        assert!(!cats.contains(&WorkloadCategory::ROS2));
        assert!(!cats.contains(&WorkloadCategory::Embeddings));
        assert!(!cats.contains(&WorkloadCategory::Unknown));
    }

    #[test]
    fn ordered_pids_matches_render_order() {
        let app = App::new();
        let s = state_with(vec![
            make_proc(20, "phi3", WorkloadCategory::LLM, warm()),
            make_proc(10, "yolo", WorkloadCategory::Vision, warm()),
        ]);
        // LLM first, Vision second; the contract order beats PID.
        assert_eq!(ordered_pids(&s, &app), vec![20, 10]);
    }

    #[test]
    fn workload_category_display_order_matches_contract_headers() {
        // L11c — the contract pins both the order and the strings
        // for group headers. `all_in_order()` and `category_header`
        // together must reproduce the contract sequence verbatim.
        let headers: Vec<&str> = WorkloadCategory::all_in_order()
            .iter()
            .map(|c| category_header(*c))
            .collect();
        assert_eq!(
            headers,
            vec![
                GROUP_HEADER_LLM,
                GROUP_HEADER_VISION,
                GROUP_HEADER_ROS2,
                GROUP_HEADER_EMBEDDINGS,
                GROUP_HEADER_UNKNOWN,
            ]
        );
        for (i, c) in WorkloadCategory::all_in_order().iter().enumerate() {
            assert_eq!(c.display_order() as usize, i);
        }
    }

    #[test]
    fn workloads_panel_cold_loading_uses_contract_const() {
        // L11c lock — the Loading-state primary-metric value MUST
        // come from `ux_contract::status::COLD_LOADING`, not a
        // local literal. Pin against the const directly so a
        // future "let's just hardcode it back" regression breaks.
        let row = make_row(WorkloadCategory::LLM, WorkloadStatus::Loading);
        assert_eq!(primary_metric(&row), COLD_LOADING);
    }

    // ════════════════════════════════════════════════════════════════════════
    // L12 — degraded-row expansion.
    // ════════════════════════════════════════════════════════════════════════

    #[test]
    fn workloads_healthy_row_no_expansion_line() {
        let row = make_row(WorkloadCategory::LLM, WorkloadStatus::Healthy);
        assert_eq!(degraded_line(&row), None);
    }

    #[test]
    fn workloads_loading_row_no_expansion_line() {
        // Loading is grey/cold-start; the panel's primary line
        // already shows "cold-loading" — a second line would be
        // redundant and dishonest (no breach is happening).
        let row = make_row(WorkloadCategory::LLM, WorkloadStatus::Loading);
        assert_eq!(degraded_line(&row), None);
    }

    #[test]
    fn workloads_attention_row_shows_expansion_line() {
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Attention);
        row.vram_pct = Some(87.0);
        let expansion = degraded_line(&row).expect("Attention must produce expansion");
        assert!(expansion.contains("VRAM 87%"), "expansion: {expansion}");
    }

    #[test]
    fn workloads_critical_row_shows_expansion_line() {
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Critical);
        row.governor_armed = true;
        let expansion = degraded_line(&row).expect("Critical must produce expansion");
        assert!(expansion.contains("governor armed"), "expansion: {expansion}");
    }

    #[test]
    fn vram_pressure_expansion_includes_percentage() {
        // Lock the v1.0 placeholder shape (CAR-9 will replace
        // with the §2 schema once the contract const ships).
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Attention);
        row.vram_pct = Some(91.4);
        // Display rounds to integer; 91.4 → "VRAM 91%".
        assert_eq!(degraded_line(&row).as_deref(), Some("VRAM 91%"));
    }

    #[test]
    fn expansion_joins_multiple_triggers_with_middle_dot() {
        // Workload simultaneously over both VRAM and KV thresholds:
        // both surface in the expansion, separated by " · " per
        // §2's locked separator. Trigger order is governor →
        // VRAM → RAM → KV (most-actionable first).
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Critical);
        row.vram_pct = Some(96.0);
        row.kv_cache_pct = Some(93.0);
        let expansion = degraded_line(&row).expect("triggers present");
        assert_eq!(expansion, "VRAM 96% · KV 93%");
    }

    #[test]
    fn expansion_governor_armed_takes_first_position() {
        // Governor-armed is the user-actionable trigger — render
        // it first so it's visible even when the row is wrapped
        // or truncated.
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Critical);
        row.governor_armed = true;
        row.vram_pct = Some(96.0);
        let expansion = degraded_line(&row).expect("triggers present");
        assert!(
            expansion.starts_with("governor armed"),
            "governor must lead: {expansion}"
        );
    }

    #[test]
    fn expansion_below_attention_thresholds_falls_through_to_defensive_message() {
        // Defensive: a row in Attention/Critical with no metric
        // breaching the v0.3 thresholds would otherwise produce an
        // empty expansion. The helper surfaces an honest "no
        // specific metric trigger" so the operator sees something.
        let row = make_row(WorkloadCategory::LLM, WorkloadStatus::Attention);
        // No VRAM/RAM/KV pct set; no governor. Should never happen
        // in production (the dot wouldn't escalate without one of
        // these), but pin the fallback.
        let expansion = degraded_line(&row).expect("Attention always expands");
        assert!(expansion.contains("no specific metric trigger"));
    }

    #[test]
    fn empty_category_still_renders_no_header() {
        // Defensive: L11b's behavior unchanged after L12. Empty
        // categories produce no rows, and `ordered_rows` doesn't
        // emit headers — the render layer's emptiness check fires.
        let app = App::new();
        let s = state_with(vec![make_proc(
            10,
            "phi3",
            WorkloadCategory::LLM,
            warm(),
        )]);
        let cats: Vec<WorkloadCategory> =
            ordered_rows(&s, &app).iter().map(|r| r.category).collect();
        assert_eq!(cats, vec![WorkloadCategory::LLM]);
        // ROS2/Embeddings/Unknown categories absent.
    }

    #[test]
    fn workloads_panel_group_headers_use_contract_constants() {
        // L11c lock — every category's group header must equal the
        // matching v0.3.4 contract const. Pinned per-variant so a
        // single drift (e.g. someone re-introducing the
        // `format!("── {} ──", ...)` style) breaks here.
        assert_eq!(
            category_header(WorkloadCategory::LLM),
            GROUP_HEADER_LLM
        );
        assert_eq!(
            category_header(WorkloadCategory::Vision),
            GROUP_HEADER_VISION
        );
        assert_eq!(
            category_header(WorkloadCategory::ROS2),
            GROUP_HEADER_ROS2
        );
        assert_eq!(
            category_header(WorkloadCategory::Embeddings),
            GROUP_HEADER_EMBEDDINGS
        );
        assert_eq!(
            category_header(WorkloadCategory::Unknown),
            GROUP_HEADER_UNKNOWN
        );
    }
}
