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

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use ux_contract::WorkloadStatus;
use ux_contract::status::{AGENT_ALIVE, COLD_LOADING, RUNNING_ACTIVELY};
use ux_contract::workload_category::{
    GROUP_HEADER_AGENT, GROUP_HEADER_EMBEDDINGS, GROUP_HEADER_LLM, GROUP_HEADER_ROS2,
    GROUP_HEADER_UNKNOWN, GROUP_HEADER_VISION,
};

use crate::model::WorkloadCategory;
use crate::runtime::{
    AnnotatedProcess, RuntimeState, WorkloadStatusInputs, compute_workload_status,
};

use super::super::app::App;
use super::panel_block;
use crate::ui::theme::UiTheme;

// Local placeholder for the primary-metric column when a workload
// is alive but its category sampler hasn't reported a value this
// tick (LLM with no KV cache yet, non-LLM categories whose
// per-type sampler isn't wired in v1.0). "running actively"
// v0.3.10 vendored `status::RUNNING_ACTIVELY` (CAR-19a). The local
// const is gone; this re-export is just a navigation aid for
// readers grepping the file.
//
// v1.0.1 B-NEW-4 — Agent rows display `AGENT_ALIVE` instead of
// `RUNNING_ACTIVELY`. Inspector #2 found "running actively"
// overclaimed activity for SaaS-LLM CLIs (claude-code, cursor,
// aider, continue) — these processes proxy to a remote LLM, and
// edge_monitor measures none of the per-request rate. "alive" is
// the honest minimum signal: the process exists on this host and
// is in our annotated set; no further claim. CAR-20 lifted this
// to `ux_contract::status::AGENT_ALIVE` in v0.3.11; the local
// const is gone — see the imports at the top of this file.

/// L11c — map the local `WorkloadCategory` enum to the v0.3.4
/// contract group-header const. Contract refined CAR-8 to
/// const-only headers; the enum stays local per the orchestrator's
/// "KEEP CONST-ONLY for v1.0" decision (the
/// WorkloadCategory-to-contract migration is v1.1+ per BACKLOG.md).
fn category_header(category: WorkloadCategory) -> &'static str {
    match category {
        WorkloadCategory::LLM => GROUP_HEADER_LLM,
        // Sprint-7.5 / CAR-18 — Agent gets its own subsection header.
        // The string comes from ux_contract v0.3.9; this map honors
        // the constants-pattern guidance in the contract doc-comment.
        WorkloadCategory::Agent => GROUP_HEADER_AGENT,
        WorkloadCategory::Vision => GROUP_HEADER_VISION,
        WorkloadCategory::ROS2 => GROUP_HEADER_ROS2,
        WorkloadCategory::Embeddings => GROUP_HEADER_EMBEDDINGS,
        WorkloadCategory::Unknown => GROUP_HEADER_UNKNOWN,
    }
}

/// v1.3.2 / DISPATCH 107 FIX 2 — column-header row for the AI
/// Workloads panel. The column widths mirror the per-row `format!`
/// call sites in `render` exactly, so labels sit above their
/// columns (a leading space stands in for the status dot; the
/// header itself is styled muted at the call site). The layout
/// branches on `narrow` + `show_model` + `show_activity` because
/// the row `format!` does too — any drift would misalign the
/// header, so this fn IS the single source of truth for the
/// header labels and must be edited in lockstep with the row
/// `format!` calls in `render`.
fn column_header_line(narrow: bool, show_model: bool, show_activity: bool) -> String {
    // Wide row shape (row `format!` at the wide branch):
    //   " {:<22}{}{} {:<14} {:<14} cpu {:>5.1}% rss {:>5}M {:>4}M"
    // Narrow row shape (row `format!` at the narrow branch):
    //   " {:<16}{} {:<8} cpu {:>4.0}% rss {:>4}M {:>4}M"
    //
    // The `cpu/rss/vram` labels re-use the same `cpu ... rss ...`
    // literal so the header shows "CPU %" / "RSS MB" / "VRAM"
    // sitting above the numeric slots.
    let model_hdr = if show_model {
        format!(" {:<width$}", "MODEL", width = MODEL_COL_WIDTH)
    } else {
        String::new()
    };
    let activity_hdr = if show_activity && !narrow {
        format!(" {:<width$}", "STATE", width = ACTIVITY_COL_WIDTH)
    } else {
        String::new()
    };
    if narrow {
        // Narrow drops the primary-metric column to fit inside the
        // 80-col §12 floor; header follows.
        format!(
            " {:<16}{} {:<8} {:>5} {:>6} {:>5}",
            "NAME", model_hdr, "STARTED", "CPU %", "RSS MB", "VRAM",
        )
    } else {
        format!(
            " {:<22}{}{} {:<14} {:<14} {:>5} {:>7} {:>5}",
            "NAME",
            model_hdr,
            activity_hdr,
            "PRIMARY",
            "STARTED",
            "CPU %",
            "RSS MB",
            "VRAM",
        )
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
    /// B4 — live rolling-average tokens-per-second from
    /// [`crate::runtime::LiveTelemetry`]. `None` when no Prometheus
    /// sampler has fed a reading for this PID this run (Ollama-passive
    /// case per B4-3 docs, cold start, non-LLM workloads).
    pub tokens_per_sec_avg: Option<f32>,
    /// B4 — live rolling-average frames-per-second. Same lifecycle as
    /// `tokens_per_sec_avg`; populated for Vision workloads when the
    /// vision-probe socket or stdout-parser path has observed frames.
    pub fps_avg: Option<f32>,
    /// F2 (Sprint-3) — wall-clock spawn time pulled from the
    /// `ProcessLifecycle` for this PID (the moment edge_monitor first
    /// observed the process, NOT the OS-level spawn time — see the
    /// comment on `AnnotatedProcess::first_observed_at`). `None` when
    /// the runtime has no lifecycle snapshot yet (first tick); the
    /// row's start-time column renders empty in that case.
    pub spawn_time: Option<DateTime<Utc>>,
    /// F3 (Sprint-3) — detected LLM model name (`ollama run <X>`,
    /// `vllm --model <X>`, `llama-server -m <X>.gguf`). Populated by
    /// the classifier; `None` for non-LLM workloads, daemon-style
    /// `ollama serve`, and any cmdline that doesn't surface a model
    /// name. Panel auto-hides the column when every visible row's
    /// model is `None`.
    pub model: Option<String>,
    /// Phase 2 / DISPATCH 1 — most-recent per-category activity state
    /// for this row, surfaced by the Phase-2 samplers (agent /
    /// ros2-shellout / embeddings-cpu) via the dispatcher
    /// accumulator. `None` when no Phase-2 sampler has reported for
    /// this PID yet, or the workload category has no Phase-2 sampler
    /// (vLLM / llama.cpp continue to report throughput-only). Panel
    /// auto-hides the column when every visible row's `activity` is
    /// `None`.
    pub activity: Option<ux_contract::activity::ActivityState>,
}

/// CAR-17 — compute the §3 `WorkloadStatus` for a single annotated
/// process using the same input pipeline as the workloads panel.
/// Public so the kill_confirm card can render the workload's status
/// in its body without re-implementing the breach logic.
pub fn status_for(
    proc: &AnnotatedProcess,
    state: &RuntimeState,
    app: &App,
) -> WorkloadStatus {
    let inputs = build_status_inputs(proc, state, app.kill_confirm_pid(), Instant::now());
    compute_workload_status(&inputs, &state.thresholds)
}

/// Build the in-render-order row list. Crate-public for tests + the
/// app's selection logic so navigation stays in lock-step with the
/// rendered order.
pub(crate) fn ordered_rows(state: &RuntimeState, app: &App) -> Vec<Row> {
    let now = Instant::now();
    let armed = app.kill_confirm_pid();
    let mut rows: Vec<Row> = state
        .ai_processes()
        .map(|p| {
            let inputs = build_status_inputs(p, state, armed, now);
            let status = compute_workload_status(&inputs, &state.thresholds);
            let lt = state.live_telemetry.get(&p.pid);
            let kv_cache_pct = lt.and_then(|t| t.kv_cache_peak_pct);
            let tokens_per_sec_avg = lt.and_then(|t| t.tokens_per_sec_avg);
            let fps_avg = lt.and_then(|t| t.fps_avg);
            let activity = lt.and_then(|t| t.activity);
            // F2 — pull spawn_time from the lifecycle snapshot. None
            // on the first tick before the tracker has populated.
            let spawn_time = state
                .last_lifecycle
                .as_ref()
                .and_then(|snap| snap.processes.get(&p.pid))
                .map(|lc| lc.spawn_time);
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
                tokens_per_sec_avg,
                fps_avg,
                spawn_time,
                // F3 — classifier already populated AnnotatedProcess
                // .model_name via augment_with_model_name; copy across.
                model: p.model_name.clone(),
                // Phase 2 / DISPATCH 1 — activity surfaced via
                // RuntimeState::live_telemetry (refreshed each tick
                // from Dispatcher::activity_for in runtime.rs).
                activity,
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
pub(crate) fn degraded_line(
    row: &Row,
    thresholds: &crate::thresholds::EffectiveThresholds,
) -> Option<String> {
    if matches!(row.status, WorkloadStatus::Healthy | WorkloadStatus::Loading) {
        return None;
    }

    let mut triggers: Vec<String> = Vec::new();
    if row.governor_armed {
        triggers.push("governor armed".to_string());
    }
    // v1.3.1 — use the resolved thresholds so an operator's
    // [thresholds] override reaches the trigger-label decision.
    // Pre-v1.3.1 this read contract constants directly; under
    // override that would have shown the row as Attention/Critical
    // (compute_workload_status used the override) but the expansion
    // line would have hidden the offending metric label.
    if let Some(p) = row.vram_pct
        && p >= thresholds.vram_attention_pct
    {
        triggers.push(format!("VRAM {p:.0}%"));
    }
    if let Some(p) = row.ram_pct
        && p >= thresholds.ram_attention_pct
    {
        triggers.push(format!("RAM {p:.0}%"));
    }
    if let Some(p) = row.kv_cache_pct
        && (p as f64) >= thresholds.kv_attention_pct
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
/// otherwise category-specific (or `"running actively"` when the
/// process is alive but the type-specific sampler hasn't reported
/// a value this tick).
///
/// B4 (Sprint-2 investigation) — LLM rows prefer live tokens/sec
/// over KV-cache occupancy, falling back to `running actively` only
/// when neither signal is present. Vision rows now render fps when
/// the dispatcher has observed frames. Ollama under passive
/// monitoring stays on `running actively` because Ollama exposes
/// tokens/sec only via per-request JSON (see B4-3 in help overlay).
fn primary_metric(row: &Row) -> String {
    if matches!(row.status, WorkloadStatus::Loading) {
        return COLD_LOADING.to_string();
    }
    match row.category {
        WorkloadCategory::LLM => match (row.tokens_per_sec_avg, row.kv_cache_pct) {
            (Some(tps), _) => format!("{tps:>4.0} tok/s"),
            (None, Some(kv)) => format!("KV {kv:>4.0}%"),
            (None, None) => RUNNING_ACTIVELY.to_string(),
        },
        WorkloadCategory::Vision => match row.fps_avg {
            Some(fps) => format!("{fps:>4.0} fps"),
            None => RUNNING_ACTIVELY.to_string(),
        },
        // v1.0.1 B-NEW-4 — Agent rows show `AGENT_ALIVE` ("alive")
        // instead of "running actively". See `AGENT_ALIVE` const
        // doc-comment. ROS2 / Embeddings / Unknown keep the
        // historical fallback for v1.0.1; Phase 2 sampler work
        // will give each its own honest signal.
        WorkloadCategory::Agent => AGENT_ALIVE.to_string(),
        // v1.3.1 / DISPATCH 63 — gate the historical "running
        // actively" fallback on the row's ActivityState. Phase 2
        // (v1.1.x) added per-category samplers that emit
        // ActivityState, but `primary_metric` never read it — so a
        // ROS2/Embeddings/Unknown row whose sampler reports Idle or
        // NotDetected still claimed "running actively" in the
        // primary-metric column while the activity column read
        // "idle" / "—" right next to it. The contradiction is the
        // last surviving piece of the v1.0.0 founding lie Phase 1
        // acknowledged but Phase 2 never finished. Closes 62-A.
        //
        // The "honest neutral token" for an Idle / NotDetected row
        // is the em-dash already used by `activity_label` for
        // NotDetected. Same character, same semantic, sourced from
        // the SEPARATOR_ALLOWLIST in `tests/copy_strings_via_
        // contract.rs` so the guard stays green without inventing a
        // new contract string. (A future contract amendment could
        // add `ux_contract::status::IDLE_NEUTRAL` to lift the
        // string, but the em-dash convention is established and
        // the literal is already allowed.)
        //
        // The `None` arm — no Phase-2 sampler ran for this row at
        // all (no `activity` reported this tick) — keeps the
        // legacy `RUNNING_ACTIVELY` fallback because there's no
        // visible activity column to contradict it (the column
        // auto-hides per `show_activity` when every visible row's
        // activity is None).
        WorkloadCategory::ROS2
        | WorkloadCategory::Embeddings
        | WorkloadCategory::Unknown => {
            use ux_contract::activity::ActivityState;
            match row.activity {
                Some(ActivityState::Idle | ActivityState::NotDetected) => {
                    "—".to_string()
                }
                // Active, Loading, or None (no sampler).
                _ => RUNNING_ACTIVELY.to_string(),
            }
        }
    }
}

/// Render the whole panel: group headers (skipped when empty) + one
/// row per workload, in `ordered_rows` order.
///
/// F2 + F3 (Sprint-3) — the row layout grew two new columns:
/// `model` (LLM-only, auto-hidden when no row in the panel has a
/// model name) and `start` (every row, shown compactly on narrow
/// panels). The narrow vs wide gate uses the panel `area.width`
/// rather than the terminal-wide `SizeTier` because the panel can be
/// rendered in half-width under the §12 two-column Wide layout.
pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App, theme: &UiTheme) {
    let rows = ordered_rows(state, app);
    let block = panel_block("AI Workloads", true, theme);
    // F2/F3 — narrow threshold mirrors `SizeTier::Narrow`'s
    // `STANDARD_COLS` floor so a panel that's half of a Wide
    // terminal still gets the compact row layout.
    let narrow = area.width < ux_contract::sizing::STANDARD_COLS;
    // F3 — auto-hide the model column when every row's `model` is
    // None. Saves screen real estate on hosts running only daemon
    // workloads (bare `ollama serve` etc.) without forcing the
    // operator to interpret a perpetually-empty column.
    let show_model = rows.iter().any(|r| r.model.is_some());
    // Phase 2 / DISPATCH 1 — Inspector #8 V1: same auto-hide rule
    // for the activity column. Until any visible row has a
    // Phase-2 sampler reading, the column doesn't render at all
    // (no perpetually-empty 8-char slot). Activity column is
    // wide-rows-only — narrow rows already drop the primary metric
    // to fit Model + start-time inside the 80-col floor.
    let show_activity = !narrow && rows.iter().any(|r| r.activity.is_some());
    let now_for_relative = Utc::now();

    if rows.is_empty() {
        // Total empty — render the contract's locked empty-state
        // copy. (`registry.rs`'s rich onboarding paragraph is
        // intentionally retired in L11b; the contract is the
        // source of truth for empty-state text.)
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", ux_contract::empty::WORKLOADS),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
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

    // v1.3.2 / DISPATCH 107 FIX 2 — column-header row. Rendered ONCE
    // at the top of the panel, styled muted so it reads as
    // structure not content. The column widths mirror the per-row
    // format below so labels sit above their columns; a `_` prefix
    // takes the place of the status-dot slot each data row uses.
    // BOARD_AUDIT §2.2 "no column headers" line closed here.
    items.push(ListItem::new(Line::from(Span::styled(
        column_header_line(narrow, show_model, show_activity),
        Style::default().fg(theme.muted),
    ))));

    for category in WorkloadCategory::all_in_order() {
        let group: Vec<&Row> = rows.iter().filter(|r| r.category == category).collect();
        if group.is_empty() {
            // §1 region 4 — empty categories render no header.
            continue;
        }
        // L21 / §14 — section / group headers in muted.
        items.push(ListItem::new(Line::from(Span::styled(
            category_header(category),
            Style::default().fg(theme.muted),
        ))));

        for row in group {
            row_index_to_list_index.push(items.len());

            let dot = symbols.workload_status(row.status);
            let vram_label = match row.vram_bytes {
                Some(b) if b > 0 => format!("{:>4}M", b / (1024 * 1024)),
                _ => "    ".into(),
            };
            // F3 — model column, fixed-width and truncated. Skipped
            // entirely when no row in the panel surfaced a model name.
            // Sprint-7 Item 2 — humanize ollama-style sha256 blob
            // names so the column doesn't render a 71-char hash.
            let model_label = if show_model {
                let m = row
                    .model
                    .as_deref()
                    .map(crate::model::humanize_model_name)
                    .unwrap_or_default();
                format!(" {:<width$}", truncate(&m, MODEL_COL_WIDTH), width = MODEL_COL_WIDTH)
            } else {
                String::new()
            };
            // Phase 2 / DISPATCH 1 — activity column. Empty string
            // when `show_activity` is false (every row's `activity`
            // is None, or narrow tier) so the column disappears
            // entirely rather than rendering 8 spaces. Wide rows
            // only, per Inspector #8 V1 (narrow rows drop primary
            // metric to fit F2/F3 columns; adding an 8-char
            // activity slot on narrow would overflow the 80-col
            // floor).
            let activity_label_text = if show_activity {
                let label = row.activity.map(activity_label).unwrap_or("");
                format!(
                    " {:<width$}",
                    truncate(label, ACTIVITY_COL_WIDTH),
                    width = ACTIVITY_COL_WIDTH,
                )
            } else {
                String::new()
            };
            // F2 — start-time column. "HH:MM (Nm ago)" wide, "Nm ago"
            // narrow. Rendered as a fixed-width slot so the trailing
            // cpu/rss/vram columns align. Empty when the lifecycle
            // tracker hasn't surfaced spawn_time yet (first tick).
            let start_label = match row.spawn_time {
                Some(spawn) => format_start_time(spawn, now_for_relative, narrow),
                None => String::new(),
            };
            // L21 / §14 — "Status dot (●⚠✕○): ONLY place colors
            // appear on workload rows." Split the primary line into
            // a colored dot span + a neutral-foreground rest so the
            // body picks up `theme.foreground` while the dot picks
            // up `theme.status_color(status)`. Joining them into a
            // single string (pre-L21 shape) would force the whole
            // row into one Color and violate the contract.
            //
            // F2 + F3 — narrow rows drop the primary-metric column to
            // fit the new model + start columns inside the 80-col
            // narrow-tier floor. Wide rows keep the primary metric.
            let rest = if narrow {
                format!(
                    " {:<16}{} {:<8} cpu {:>4.0}% rss {:>4}M {}",
                    truncate(&row.name, 16),
                    model_label,
                    start_label,
                    row.cpu_pct,
                    row.rss_mb,
                    vram_label,
                )
            } else {
                format!(
                    " {:<22}{}{} {:<14} {:<14} cpu {:>5.1}% rss {:>5}M {}",
                    truncate(&row.name, 22),
                    model_label,
                    activity_label_text,
                    primary_metric(row),
                    start_label,
                    row.cpu_pct,
                    row.rss_mb,
                    vram_label,
                )
            };
            let primary_line = Line::from(vec![
                Span::styled(
                    dot.to_string(),
                    Style::default().fg(theme.status_color(row.status)),
                ),
                Span::styled(rest, Style::default().fg(theme.foreground)),
            ]);
            // L12 — combine primary + expansion into a single
            // ListItem so the highlight (selection bg) covers both
            // when this row is selected. The expansion's `Option`
            // shape keeps Healthy / Loading rows at their
            // pre-L12 single-line layout exactly.
            let mut lines = vec![primary_line];
            if let Some(expansion) = degraded_line(row, &state.thresholds) {
                // §14 — expansion line is supplementary context;
                // muted keeps it visually subordinate to the
                // primary row while still readable.
                lines.push(Line::from(Span::styled(
                    format!("    {expansion}"),
                    Style::default().fg(theme.muted),
                )));
            }
            items.push(ListItem::new(lines));
        }
    }

    // L21 / §14 — selected row tinted with Accent background.
    // Pair with `theme.background` foreground to keep the row
    // legible regardless of the accent palette in play.
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(theme.accent)
            .fg(theme.background)
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

/// F2 — compact "X{unit} ago" relative-duration formatter. Sub-second
/// elapsed renders as "0s ago"; days dominate hours, hours dominate
/// minutes, minutes dominate seconds. Returns "?" for negative
/// durations (clock skew between `spawn_time` and `now`) so the
/// column doesn't render a misleading "1234567890s ago."
pub(crate) fn format_relative_short(elapsed: chrono::Duration) -> String {
    let secs = elapsed.num_seconds();
    if secs < 0 {
        return "?".to_string();
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

/// F2 — start-time column formatter. Wide rows show "HH:MM (Nm ago)"
/// so the operator sees both the absolute wall-clock and the elapsed.
/// Narrow rows show only the relative form to fit the §12 80-col
/// floor. Uses `now` and the local timezone via the caller's choice
/// (typically `Utc::now()` + `Local` conversion); pure so tests can
/// pin the format without depending on system time.
pub(crate) fn format_start_time(
    spawn_time: DateTime<Utc>,
    now: DateTime<Utc>,
    narrow: bool,
) -> String {
    let relative = format_relative_short(now - spawn_time);
    if narrow {
        return relative;
    }
    let absolute = spawn_time.with_timezone(&Local).format("%H:%M").to_string();
    format!("{absolute} ({relative})")
}

/// F3 — max width for the model column. Model names like
/// `qwen2.5-0.5b-instruct-q8_0` are routinely 20+ chars; truncating
/// at 14 keeps the row inside the §12 narrow-tier 80-col floor.
const MODEL_COL_WIDTH: usize = 14;

/// Phase 2 / DISPATCH 1 — width of the per-row activity column.
/// 8 chars fits the longest visible label ("loading" with a
/// trailing space) without forcing a layout reflow when state
/// flips between Active / Idle / Loading. The column is shown
/// only on non-narrow panels and auto-hides when no visible row
/// has `activity = Some(_)` (Inspector #8 V1).
const ACTIVITY_COL_WIDTH: usize = 8;

/// Phase 2 / DISPATCH 1 — render an `ActivityState` to its column
/// label. Bare-variant enum keeps this a pure match; the caller
/// pads / truncates to `ACTIVITY_COL_WIDTH`.
fn activity_label(a: ux_contract::activity::ActivityState) -> &'static str {
    use ux_contract::activity::ActivityState;
    match a {
        ActivityState::Active => "active",
        ActivityState::Idle => "idle",
        ActivityState::Loading => "loading",
        ActivityState::NotDetected => "—",
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
            tokens_per_sec_avg: None,
            fps_avg: None,
            spawn_time: None,
            model: None,
            activity: None,
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
        // Two LLM rows, both warm. Opening kill_confirm on the
        // higher-PID row escalates its status to Critical via
        // `governor_armed` (the kill_confirm-targeted flag).
        let mut app = App::new();
        let s = state_with(vec![
            make_proc(10, "calm", WorkloadCategory::LLM, warm()),
            make_proc(99, "armed", WorkloadCategory::LLM, warm()),
        ]);
        app.open_kill_confirm(
            crate::ui::panels::kill_confirm::KillConfirmCard::new(
                "armed".into(),
                99,
                "LLM".into(),
                "Running".into(),
                10,
                5.0,
                256,
                None,
                false,
            ),
        );
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

    // ── B4 (Sprint-2 investigation) — primary_metric live wiring ────
    //
    // Pre-fix: LLM rows showed "KV NN%" if KV-cache occupancy was
    // present and "running actively" otherwise, regardless of whether
    // tokens/sec was available. Vision rows had no fps path at all.
    // After fix: tokens/sec wins for LLM; fps wins for Vision; the
    // old fallbacks remain for the "no signal yet" case.

    #[test]
    fn workloads_panel_renders_tokens_per_sec_when_present() {
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Healthy);
        row.tokens_per_sec_avg = Some(42.7);
        // KV present too — tokens/sec must win.
        row.kv_cache_pct = Some(67.0);
        let s = primary_metric(&row);
        assert!(s.contains("tok/s"), "expected tok/s; got {s:?}");
        assert!(s.contains("43"), "expected rounded 43 in {s:?}");
        assert!(!s.contains("KV"), "tokens/sec must override KV; got {s:?}");
    }

    #[test]
    fn workloads_panel_falls_back_to_kv_cache_when_no_tokens() {
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Healthy);
        row.tokens_per_sec_avg = None;
        row.kv_cache_pct = Some(45.0);
        let s = primary_metric(&row);
        assert!(s.contains("KV"), "expected KV fallback; got {s:?}");
        assert!(s.contains("45"));
    }

    #[test]
    fn workloads_panel_renders_fps_for_vision() {
        let mut row = make_row(WorkloadCategory::Vision, WorkloadStatus::Healthy);
        row.fps_avg = Some(28.0);
        let s = primary_metric(&row);
        assert!(s.contains("fps"), "expected fps; got {s:?}");
        assert!(s.contains("28"));
    }

    #[test]
    fn workloads_panel_falls_back_to_running_actively_when_both_none() {
        // Healthy LLM with no live telemetry: still must say
        // "running actively" rather than print "tok/s" or "KV" with
        // empty placeholders.
        let row = make_row(WorkloadCategory::LLM, WorkloadStatus::Healthy);
        assert_eq!(primary_metric(&row), RUNNING_ACTIVELY);
        // And Vision parallel — fps absent → fall back.
        let v = make_row(WorkloadCategory::Vision, WorkloadStatus::Healthy);
        assert_eq!(primary_metric(&v), RUNNING_ACTIVELY);
    }

    // ── DISPATCH 63 / 62-A — the residual "running actively" lie ────
    //
    // Phase 1 acknowledged that ROS2 / Embeddings / Unknown rows had
    // no honest signal of their own; Phase 2 added the ActivityState
    // sampler, but `primary_metric` never consulted it. With the
    // gate in place, an Idle or NotDetected row no longer claims
    // "running actively" in the primary-metric column while the
    // activity column right next to it reads "idle" / "—".

    /// Helper that builds a Row in one of the three "fallback"
    /// categories so the per-category tests below stay tight.
    fn fallback_row(
        category: WorkloadCategory,
        activity: Option<ux_contract::activity::ActivityState>,
    ) -> Row {
        let mut row = make_row(category, WorkloadStatus::Healthy);
        row.activity = activity;
        row
    }

    /// `ROS2` row with `Idle` activity MUST NOT claim "running
    /// actively". This is the wide-tier visible contradiction Phase
    /// 2 left in place.
    #[test]
    fn ros2_idle_does_not_claim_running_actively() {
        use ux_contract::activity::ActivityState;
        let row = fallback_row(WorkloadCategory::ROS2, Some(ActivityState::Idle));
        let s = primary_metric(&row);
        assert_ne!(
            s, RUNNING_ACTIVELY,
            "ROS2 + Idle MUST NOT show {RUNNING_ACTIVELY:?}; got {s:?}",
        );
    }

    /// `Embeddings` row with `Idle` activity MUST NOT claim
    /// "running actively". Same shape as the ROS2 case.
    #[test]
    fn embeddings_idle_does_not_claim_running_actively() {
        use ux_contract::activity::ActivityState;
        let row = fallback_row(WorkloadCategory::Embeddings, Some(ActivityState::Idle));
        let s = primary_metric(&row);
        assert_ne!(
            s, RUNNING_ACTIVELY,
            "Embeddings + Idle MUST NOT show {RUNNING_ACTIVELY:?}; got {s:?}",
        );
    }

    /// `Unknown` row with `Idle` activity MUST NOT claim "running
    /// actively". Closes the third surviving variant of the lie.
    #[test]
    fn unknown_idle_does_not_claim_running_actively() {
        use ux_contract::activity::ActivityState;
        let row = fallback_row(WorkloadCategory::Unknown, Some(ActivityState::Idle));
        let s = primary_metric(&row);
        assert_ne!(
            s, RUNNING_ACTIVELY,
            "Unknown + Idle MUST NOT show {RUNNING_ACTIVELY:?}; got {s:?}",
        );
    }

    /// `NotDetected` is the other failure mode: the sampler ran but
    /// couldn't determine state. Same display rule — no honest
    /// claim, neutral marker only.
    #[test]
    fn ros2_not_detected_does_not_claim_running_actively() {
        use ux_contract::activity::ActivityState;
        let row = fallback_row(
            WorkloadCategory::ROS2,
            Some(ActivityState::NotDetected),
        );
        let s = primary_metric(&row);
        assert_ne!(
            s, RUNNING_ACTIVELY,
            "ROS2 + NotDetected MUST NOT show {RUNNING_ACTIVELY:?}; got {s:?}",
        );
    }

    /// No regression of the honest case: `Active` MUST still
    /// surface as `running actively` for every fallback category.
    /// The gate exists to mute lies on Idle / NotDetected, not to
    /// erase the legitimate fallback when the sampler reports work.
    #[test]
    fn active_state_still_renders_running_actively_for_fallback_categories() {
        use ux_contract::activity::ActivityState;
        for cat in [
            WorkloadCategory::ROS2,
            WorkloadCategory::Embeddings,
            WorkloadCategory::Unknown,
        ] {
            let row = fallback_row(cat, Some(ActivityState::Active));
            assert_eq!(
                primary_metric(&row),
                RUNNING_ACTIVELY,
                "{cat:?} + Active should still show {RUNNING_ACTIVELY:?}",
            );
        }
    }

    /// `Loading` activity is honest about ongoing warm-up — keep
    /// the legacy `RUNNING_ACTIVELY` fallback (the workload IS
    /// doing observable startup work).
    #[test]
    fn loading_activity_keeps_running_actively_fallback() {
        use ux_contract::activity::ActivityState;
        let row = fallback_row(WorkloadCategory::ROS2, Some(ActivityState::Loading));
        assert_eq!(primary_metric(&row), RUNNING_ACTIVELY);
    }

    /// `None` activity — no Phase-2 sampler reported for this row
    /// at all — keeps the legacy fallback. The activity column
    /// auto-hides when every visible row's activity is None (per
    /// `show_activity`), so there's no contradiction to close.
    #[test]
    fn none_activity_keeps_running_actively_fallback() {
        let row = fallback_row(WorkloadCategory::Unknown, None);
        assert_eq!(primary_metric(&row), RUNNING_ACTIVELY);
    }

    /// Narrow-tier corollary. The narrow `format!` at lines ~515
    /// drops the primary-metric column entirely (the format string
    /// has no `{:<14}` slot for it), so the lie was never visible
    /// to a narrow-tier operator in the current code. The gate
    /// still hardens against a future layout change that reintroduces
    /// primary_metric in narrow rendering: even if it did, the
    /// returned string would no longer be the lie for an Idle row.
    #[test]
    fn narrow_tier_invariant_returned_metric_is_not_the_lie_for_idle() {
        use ux_contract::activity::ActivityState;
        // Mirror the exact row a narrow operator's screen would
        // describe — a ROS2 node going Idle between publishes.
        let row = fallback_row(WorkloadCategory::ROS2, Some(ActivityState::Idle));
        let s = primary_metric(&row);
        assert_ne!(
            s, RUNNING_ACTIVELY,
            "narrow-tier display invariant: even if a future layout \
             change re-introduces primary_metric on narrow rows, the \
             returned string MUST NOT be the lie for an Idle row. \
             got {s:?}",
        );
    }

    // ── F2 — start-time formatter (Sprint-3 per-row spawn column) ──

    #[test]
    fn format_relative_short_under_60s_returns_seconds() {
        assert_eq!(format_relative_short(chrono::Duration::seconds(0)), "0s ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(12)), "12s ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(59)), "59s ago");
    }

    #[test]
    fn format_relative_short_minutes() {
        // Boundary: 60s rolls to 1m, not "60s".
        assert_eq!(format_relative_short(chrono::Duration::seconds(60)), "1m ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(12 * 60)), "12m ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(59 * 60)), "59m ago");
    }

    #[test]
    fn format_relative_short_hours() {
        // Boundary: 3600s rolls to 1h.
        assert_eq!(format_relative_short(chrono::Duration::seconds(3600)), "1h ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(2 * 3600)), "2h ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(23 * 3600)), "23h ago");
    }

    #[test]
    fn format_relative_short_days() {
        // Boundary: 24h rolls to 1d.
        assert_eq!(format_relative_short(chrono::Duration::seconds(24 * 3600)), "1d ago");
        assert_eq!(format_relative_short(chrono::Duration::seconds(3 * 24 * 3600)), "3d ago");
    }

    #[test]
    fn format_start_time_wide_uses_absolute_plus_relative() {
        let spawn = chrono::DateTime::parse_from_rfc3339("2026-05-18T06:48:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = spawn + chrono::Duration::seconds(12 * 60);
        let s = format_start_time(spawn, now, /*narrow=*/ false);
        assert!(s.contains("12m ago"), "expected 12m ago; got {s:?}");
        // The exact HH:MM depends on the test host's local timezone,
        // so assert on shape (4 digits + colon + parens) rather than
        // a fixed string.
        assert!(
            s.matches(':').count() >= 1 && s.contains('('),
            "wide form must include HH:MM (… ago); got {s:?}"
        );
    }

    #[test]
    fn format_start_time_narrow_uses_relative_only() {
        let spawn = chrono::DateTime::parse_from_rfc3339("2026-05-18T06:48:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = spawn + chrono::Duration::seconds(12 * 60);
        let s = format_start_time(spawn, now, /*narrow=*/ true);
        assert_eq!(s, "12m ago");
        assert!(
            !s.contains('('),
            "narrow form must NOT include the absolute HH:MM; got {s:?}"
        );
    }

    #[test]
    fn workload_row_displays_spawn_time() {
        // ordered_rows must pull spawn_time off the lifecycle snapshot
        // so the render path can format it into the start-time
        // column.
        use crate::lifecycle::{LifecycleSnapshot, ProcessLifecycle};
        let proc = make_proc(
            42,
            "llama-server",
            WorkloadCategory::LLM,
            Duration::from_secs(60),
        );
        let sample = crate::model::ProcessSample {
            pid: 42,
            ppid: Some(1),
            name: "llama-server".into(),
            ..Default::default()
        };
        let mut snap = LifecycleSnapshot::new();
        snap.processes.insert(42, ProcessLifecycle::new(&sample, None));
        let mut s = state_with(vec![proc]);
        s.last_lifecycle = Some(snap);
        let rows = ordered_rows(&s, &App::new());
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].spawn_time.is_some(),
            "row must surface lifecycle spawn_time when the tracker has the PID"
        );
    }

    // ── F3 — model column on the workloads panel (Sprint-3) ─────────

    #[test]
    fn workload_row_displays_model_when_present() {
        let mut proc = make_proc(
            42,
            "ollama",
            WorkloadCategory::LLM,
            Duration::from_secs(60),
        );
        proc.model_name = Some("tinyllama".to_string());
        let s = state_with(vec![proc]);
        let rows = ordered_rows(&s, &App::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("tinyllama"));
    }

    #[test]
    fn workload_row_renders_empty_when_model_none() {
        // Bare `ollama serve` daemon: classifier produces no
        // model_name; row.model stays None and the renderer
        // either hides the column entirely (when ALL rows are None)
        // or pads it blank.
        let proc = make_proc(
            42,
            "ollama",
            WorkloadCategory::LLM,
            Duration::from_secs(60),
        );
        let s = state_with(vec![proc]);
        let rows = ordered_rows(&s, &App::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, None);
    }

    #[test]
    fn workload_row_truncates_long_model_names() {
        // Render width caps the model column at MODEL_COL_WIDTH; the
        // panel renderer's `truncate` helper inserts a trailing
        // ellipsis when the model name exceeds the cap. This test
        // exercises the truncation primitive directly so a future
        // change to MODEL_COL_WIDTH retains the cap behaviour.
        let long = "qwen2.5-0.5b-instruct-q8_0";
        let truncated = truncate(long, MODEL_COL_WIDTH);
        assert!(
            truncated.chars().count() <= MODEL_COL_WIDTH,
            "truncated length must fit MODEL_COL_WIDTH; got {truncated:?}"
        );
        assert!(
            truncated.ends_with('…'),
            "over-long model names must show ellipsis; got {truncated:?}"
        );
    }

    #[test]
    fn live_telemetry_round_trips_tokens_per_sec_into_row() {
        // End-to-end: a `LiveTelemetry` entry with tokens_per_sec_avg
        // populated must land on the row produced by `ordered_rows`,
        // so the renderer can read it. Catches a future regression
        // where the runtime stops copying the field across.
        use crate::runtime::LiveTelemetry;
        let proc = make_proc(
            42,
            "llama-server",
            WorkloadCategory::LLM,
            Duration::from_secs(60),
        );
        let mut s = state_with(vec![proc]);
        s.live_telemetry.insert(
            42,
            LiveTelemetry {
                tokens_per_sec_avg: Some(38.4),
                fps_avg: None,
                kv_cache_peak_pct: None,
                kv_cache_evictions_total: None,
                activity: None,
            },
        );
        let rows = ordered_rows(&s, &App::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens_per_sec_avg, Some(38.4));
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
                // Sprint-7.5 / CAR-18 — Agent sits between LLM and
                // Vision per `WorkloadCategory::display_order`. The
                // contract const came from ux_contract v0.3.9.
                GROUP_HEADER_AGENT,
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
        assert_eq!(degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()), None);
    }

    #[test]
    fn workloads_loading_row_no_expansion_line() {
        // Loading is grey/cold-start; the panel's primary line
        // already shows "cold-loading" — a second line would be
        // redundant and dishonest (no breach is happening).
        let row = make_row(WorkloadCategory::LLM, WorkloadStatus::Loading);
        assert_eq!(degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()), None);
    }

    #[test]
    fn workloads_attention_row_shows_expansion_line() {
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Attention);
        row.vram_pct = Some(87.0);
        let expansion = degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()).expect("Attention must produce expansion");
        assert!(expansion.contains("VRAM 87%"), "expansion: {expansion}");
    }

    #[test]
    fn workloads_critical_row_shows_expansion_line() {
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Critical);
        row.governor_armed = true;
        let expansion = degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()).expect("Critical must produce expansion");
        assert!(expansion.contains("governor armed"), "expansion: {expansion}");
    }

    #[test]
    fn vram_pressure_expansion_includes_percentage() {
        // Lock the v1.0 placeholder shape (CAR-9 will replace
        // with the §2 schema once the contract const ships).
        let mut row = make_row(WorkloadCategory::LLM, WorkloadStatus::Attention);
        row.vram_pct = Some(91.4);
        // Display rounds to integer; 91.4 → "VRAM 91%".
        assert_eq!(degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()).as_deref(), Some("VRAM 91%"));
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
        let expansion = degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()).expect("triggers present");
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
        let expansion = degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()).expect("triggers present");
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
        let expansion = degraded_line(&row, &crate::thresholds::EffectiveThresholds::default()).expect("Attention always expands");
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

    /// DISPATCH 107 FIX 2 — `column_header_line` is the single
    /// source of truth for the AI Workloads panel's column labels.
    /// The doc-comment on the fn states it must be edited "in
    /// lockstep with the row `format!` calls in `render`" — if the
    /// header format drifts from the row format, the "NAME MODEL
    /// STATE PRIMARY..." labels stop sitting above their columns
    /// and the panel looks scrambled.
    ///
    /// These tests pin the header text for each of the three
    /// render-tier shapes (narrow, wide-no-optional, wide-full).
    /// A drift in either the header fn OR the row `format!` calls
    /// will fail one of these OR the visual test — forcing the
    /// operator to reconcile both before the panel renders wrong.
    #[test]
    fn column_header_line_wide_shows_all_column_labels() {
        let header = column_header_line(false, true, true);
        // Wide + model + activity: NAME, MODEL, STATE, PRIMARY, STARTED, CPU %, RSS MB, VRAM.
        for label in ["NAME", "MODEL", "STATE", "PRIMARY", "STARTED", "CPU %", "RSS MB", "VRAM"] {
            assert!(
                header.contains(label),
                "wide-full header must show {label:?}; got {header:?}",
            );
        }
    }

    #[test]
    fn column_header_line_wide_no_optionals_drops_model_and_state() {
        // When the panel has no model column or no activity column
        // (nothing to show), the header must drop those slots too —
        // otherwise the trailing PRIMARY/STARTED slots land at the
        // wrong offset.
        let header = column_header_line(false, false, false);
        assert!(header.contains("NAME"), "NAME always present");
        assert!(header.contains("PRIMARY"), "PRIMARY always present on wide");
        assert!(!header.contains("MODEL"), "MODEL dropped when show_model=false; got {header:?}");
        assert!(!header.contains("STATE"), "STATE dropped when show_activity=false; got {header:?}");
    }

    #[test]
    fn column_header_line_narrow_drops_primary() {
        // Narrow tier drops the PRIMARY column to fit inside the
        // 80-col §12 floor. Same reason the row `format!` at the
        // narrow branch omits it — if header drifts and keeps
        // PRIMARY here, the header overflows.
        let header = column_header_line(true, true, true);
        assert!(header.contains("NAME"));
        assert!(header.contains("MODEL"));
        assert!(header.contains("STARTED"));
        assert!(header.contains("CPU %"));
        assert!(header.contains("RSS MB"));
        assert!(header.contains("VRAM"));
        assert!(!header.contains("PRIMARY"), "narrow tier must drop PRIMARY; got {header:?}");
        assert!(!header.contains("STATE"), "narrow tier drops STATE (activity col wide-only per Inspector #8 V1); got {header:?}");
    }
}
