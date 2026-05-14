//! L13 / UX_CONTRACT.md §1 region 5 — Top processes panel.
//!
//! Read-only system-wide process list, sorted by RAM (RSS)
//! descending by default. Sits between the Workloads panel and the
//! Activity panel in the default layout. **Not selectable** — Top
//! processes is informational; the workloads panel remains the
//! sole selectable surface in v1.0.
//!
//! ## Filtering
//!
//! Excludes `edge_monitor` itself (the operator already knows the
//! monitor is running; surfacing its own RSS would clutter the
//! panel and confuse the "what's eating my memory" question).
//!
//! AI workloads ARE included — they appear in both this panel and
//! the AI Workloads panel above. UX_CONTRACT.md §1 region 5's
//! example shows `ollama` (an AI workload) in the Top processes
//! list, and per the orchestrator's L13 brief: "AI processes
//! appear in BOTH panels — they're not de-duplicated. The Top
//! processes panel is for system-wide visibility; AI Workloads is
//! for model-aware monitoring. Different views of the same data."
//!
//! Note: §1 region 5's prose says "Filters … processes already in
//! Workloads", which contradicts the example. Implementation
//! follows the example + the orchestrator's brief; flagged for
//! contract-clarification routing in the L13 report.
//!
//! ## L14
//!
//! L14 wires the `t` key (`Action::CycleTopSort`) to cycle this
//! panel's sort: RAM → CPU → VRAM. State (`TopProcessesSort`)
//! lives on `App`; the render call selects the matching named
//! sort fn (`top_n_by_rss` / `top_n_by_cpu` / `top_n_by_vram`)
//! and the matching panel title. Three named fns over one
//! parameterized helper: trivial perf cost, much easier to debug
//! a specific sort if it goes wrong, and the v1.1 GPU-watts sort
//! lands as a fourth fn without bloating one branch.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::ui::theme::UiTheme;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};

use crate::runtime::{AnnotatedProcess, RuntimeState};

use super::panel_block;

/// Maximum rows rendered in standard mode. §12 narrow-mode caps
/// further (panel hidden entirely below 80×24); sizing-aware
/// truncation lives in L22's row.
pub const MAX_VISIBLE_ROWS: usize = 5;

/// L14 — which dimension the Top processes panel is currently
/// sorted on. Cycled by the `t` key via `Action::CycleTopSort`.
/// Lives in this module (not `ui::app`) because the sort fns and
/// the per-sort panel titles all live here; the field on `App`
/// just holds the current choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TopProcessesSort {
    /// Sort by resident set size (RSS) descending. §13 default.
    #[default]
    Ram,
    /// Sort by per-tick CPU percent of one core, descending.
    Cpu,
    /// Sort by per-process VRAM bytes, descending, with
    /// `vram_bytes: None` placed last so present-GPU rows lead
    /// the list on hybrid / no-GPU systems.
    Vram,
}

impl TopProcessesSort {
    /// Cycle: Ram → Cpu → Vram → Ram. Pure; called by
    /// `App::cycle_top_sort` on each `t` press.
    pub fn next(self) -> Self {
        match self {
            Self::Ram => Self::Cpu,
            Self::Cpu => Self::Vram,
            Self::Vram => Self::Ram,
        }
    }

    /// Human label used in the contract status template
    /// (`status::TOP_SORT_CHANGED` — "Top processes sorted by
    /// {dimension}"). Matches the panel-title suffix so the
    /// footer feedback and the header agree.
    pub fn dimension_label(self) -> &'static str {
        match self {
            Self::Ram => "RAM",
            Self::Cpu => "CPU",
            Self::Vram => "VRAM",
        }
    }
}

/// Per-sort panel title. Local literals; no
/// `ux_contract::top_processes::PANEL_TITLE_BY_*` constants in
/// v0.3.4 (CAR-11 pending). Title shape matches the §1 region 5
/// example ("Top processes (by RAM)").
pub(crate) fn panel_title(sort: TopProcessesSort) -> &'static str {
    match sort {
        TopProcessesSort::Ram => "Top processes (by RAM)",
        TopProcessesSort::Cpu => "Top processes (by CPU)",
        TopProcessesSort::Vram => "Top processes (by VRAM)",
    }
}

/// Returns the top-N processes for the panel. Pure: takes the
/// runtime state slice it needs and a self-PID for the
/// edge_monitor exclusion. Sort: rss_mb descending; ties broken
/// by PID ascending so renderings are stable across ticks.
pub(crate) fn top_n_by_rss(
    state: &RuntimeState,
    self_pid: u32,
    n: usize,
) -> Vec<&AnnotatedProcess> {
    let mut procs: Vec<&AnnotatedProcess> = state
        .annotated
        .iter()
        .filter(|p| p.pid != self_pid)
        .collect();
    // Highest RSS first, PID-asc tiebreak.
    procs.sort_by(|a, b| b.rss_mb.cmp(&a.rss_mb).then(a.pid.cmp(&b.pid)));
    procs.truncate(n);
    procs
}

/// L14 — top-N processes by per-tick CPU percent, descending.
/// PID-ascending tiebreak so renderings stay stable across ticks
/// when several processes hit the same CPU bucket.
pub(crate) fn top_n_by_cpu(
    state: &RuntimeState,
    self_pid: u32,
    n: usize,
) -> Vec<&AnnotatedProcess> {
    let mut procs: Vec<&AnnotatedProcess> = state
        .annotated
        .iter()
        .filter(|p| p.pid != self_pid)
        .collect();
    // f32 has no total Ord; partial_cmp can return None for NaN.
    // /proc-sourced cpu_pct never produces NaN in practice, but
    // fall back to Equal defensively so a poisoned sample can't
    // panic the panel.
    procs.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.pid.cmp(&b.pid))
    });
    procs.truncate(n);
    procs
}

/// L14 — top-N processes by per-process VRAM bytes, descending,
/// with `vram_bytes: None` placed LAST. The default `Option<T>`
/// Ord puts `None < Some(_)`, which would surface no-VRAM rows
/// first on hybrid / no-GPU systems — the opposite of what the
/// operator wants from a "top processes by VRAM" view. PID-asc
/// tiebreak (including across the None block) for stability.
pub(crate) fn top_n_by_vram(
    state: &RuntimeState,
    self_pid: u32,
    n: usize,
) -> Vec<&AnnotatedProcess> {
    let mut procs: Vec<&AnnotatedProcess> = state
        .annotated
        .iter()
        .filter(|p| p.pid != self_pid)
        .collect();
    procs.sort_by(|a, b| {
        match (a.vram_bytes, b.vram_bytes) {
            // Both report VRAM: descending by value, PID-asc tiebreak.
            (Some(av), Some(bv)) => bv.cmp(&av).then(a.pid.cmp(&b.pid)),
            // Only `a` reports VRAM → `a` ranks higher (comes first).
            (Some(_), None) => std::cmp::Ordering::Less,
            // Only `b` reports VRAM → `b` ranks higher.
            (None, Some(_)) => std::cmp::Ordering::Greater,
            // Neither reports — PID-asc for deterministic order.
            (None, None) => a.pid.cmp(&b.pid),
        }
    });
    procs.truncate(n);
    procs
}

fn format_rss(rss_mb: u64) -> String {
    if rss_mb >= 1024 {
        let gb = rss_mb as f64 / 1024.0;
        format!("{gb:>5.1} GB")
    } else {
        format!("{rss_mb:>5} MB")
    }
}

pub fn render(
    f: &mut Frame,
    area: Rect,
    state: &RuntimeState,
    sort: TopProcessesSort,
    theme: &UiTheme,
) {
    let block = panel_block(panel_title(sort), false, theme);
    let self_pid = std::process::id();
    // Branch on sort dimension. Three named fns over one
    // parameterized helper: easier to debug per-sort regressions
    // and v1.1 additions stay isolated.
    let procs = match sort {
        TopProcessesSort::Ram => top_n_by_rss(state, self_pid, MAX_VISIBLE_ROWS),
        TopProcessesSort::Cpu => top_n_by_cpu(state, self_pid, MAX_VISIBLE_ROWS),
        TopProcessesSort::Vram => top_n_by_vram(state, self_pid, MAX_VISIBLE_ROWS),
    };

    if procs.is_empty() {
        // Defensive: in production `state.annotated` is empty only
        // for the very first tick (before the platform layer's
        // first sample). Render an italic "—" so the panel
        // doesn't render as a blank box. No contract const for
        // this empty state; left as a local "—" given the
        // transient nature.
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  —",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            )),
        ];
        let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    // L21 / §14 — Top processes rows are "All other text" per §14
    // (no status dots, no bars), so foreground is the single rule
    // here. Styling the list once is cheaper than per-item style.
    let items: Vec<ListItem<'_>> = procs
        .iter()
        .map(|p| {
            // Match the §1 example's column shape:
            //   <name 28-wide> <rss right-aligned> <cpu pct>
            // Truncate name with the same `…` rule as the
            // workloads panel.
            let name = truncate(&p.name, 28);
            let rss = format_rss(p.rss_mb);
            ListItem::new(format!(
                "{:<28} {:>10}  {:>5.1}% CPU",
                name, rss, p.cpu_pct
            ))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(theme.foreground));
    f.render_widget(list, area);
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
    use crate::model::{AICategory, WorkloadCategory};
    use std::time::Instant;

    fn proc(pid: u32, name: &str, rss_mb: u64, cpu_pct: f32) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: AICategory::NotAi,
            workload_category: WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct,
            rss_mb,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        }
    }

    fn ai_proc(pid: u32, name: &str, rss_mb: u64) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: AICategory::Inference,
            workload_category: WorkloadCategory::LLM,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        }
    }

    fn state_with(procs: Vec<AnnotatedProcess>) -> RuntimeState {
        RuntimeState {
            annotated: procs,
            ..Default::default()
        }
    }

    #[test]
    fn top_processes_panel_renders_processes_sorted_by_rss_descending() {
        let state = state_with(vec![
            proc(1, "small", 100, 0.0),
            proc(2, "huge", 8_000, 0.0),
            proc(3, "medium", 1_000, 0.0),
        ]);
        let top = top_n_by_rss(&state, /* self_pid */ 9999, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![2, 3, 1], "RSS descending");
    }

    #[test]
    fn top_processes_panel_caps_visible_rows_to_n() {
        // 8 processes → top_n_by_rss(state, _, 5) returns 5.
        let mut all = Vec::new();
        for i in 0..8 {
            all.push(proc(i + 1, "x", (8 - i) as u64 * 100, 0.0));
        }
        let state = state_with(all);
        let top = top_n_by_rss(&state, 9999, MAX_VISIBLE_ROWS);
        assert_eq!(top.len(), MAX_VISIBLE_ROWS);
    }

    #[test]
    fn top_processes_panel_excludes_edge_monitor_self() {
        // The monitor's own RSS would dominate at "what's eating
        // memory" otherwise. Lock the self-PID exclusion.
        let state = state_with(vec![
            proc(42, "edge_monitor", 50_000, 0.0),
            proc(100, "browser", 2_000, 0.0),
        ]);
        let top = top_n_by_rss(&state, /* self_pid */ 42, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![100], "self-PID 42 must be filtered");
    }

    #[test]
    fn top_processes_panel_includes_ai_workloads_in_unfiltered_list() {
        // Per UX_CONTRACT.md §1 region 5's example (and the
        // orchestrator's L13 brief) — AI workloads (ollama in
        // the contract example) appear in BOTH panels. This
        // test pins the un-filtered behavior against future
        // "let's de-duplicate" refactors that read only the §1
        // region 5 prose ("Filters … processes already in
        // Workloads"), which contradicts the example.
        let state = state_with(vec![
            ai_proc(206, "ollama", 4_200),
            proc(1000, "browser", 900, 0.0),
        ]);
        let top = top_n_by_rss(&state, /* self_pid */ 9999, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert!(pids.contains(&206), "AI ollama PID 206 must appear");
    }

    #[test]
    fn top_processes_panel_tiebreaks_equal_rss_by_pid_ascending() {
        // Two processes at exactly the same RSS — render order
        // must be deterministic across ticks so the panel
        // doesn't visually shuffle.
        let state = state_with(vec![
            proc(50, "second", 1_000, 0.0),
            proc(10, "first", 1_000, 0.0),
        ]);
        let top = top_n_by_rss(&state, 9999, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![10, 50], "PID-ascending tiebreak");
    }

    #[test]
    fn top_processes_panel_empty_state_returns_empty_vec() {
        // No processes → empty top list. The render path's
        // defensive "—" branch is what the operator sees in
        // this state.
        let state = state_with(vec![]);
        let top = top_n_by_rss(&state, 9999, 10);
        assert!(top.is_empty());
    }

    #[test]
    fn top_processes_panel_has_no_selectable_rows() {
        // L13 design lock (intent preserved through L14): Top
        // processes is read-only. The panel takes no `App`
        // reference and exposes no selection state. L14 added a
        // `TopProcessesSort` parameter — that's panel-local
        // display state, not selection plumbing — so the lock
        // still holds: no `&App` here, no selected_index, no
        // selectable rows.
        let _: fn(&mut Frame, Rect, &RuntimeState, TopProcessesSort, &UiTheme) = render;
    }

    // ── format_rss formatting ─────────────────────────────────────

    #[test]
    fn format_rss_under_1gb_renders_as_mb() {
        assert_eq!(format_rss(0).trim(), "0 MB");
        assert_eq!(format_rss(512).trim(), "512 MB");
        assert_eq!(format_rss(1023).trim(), "1023 MB");
    }

    #[test]
    fn format_rss_at_or_above_1gb_renders_as_gb_with_one_decimal() {
        assert_eq!(format_rss(1024).trim(), "1.0 GB");
        assert_eq!(format_rss(4500).trim(), "4.4 GB");
        assert_eq!(format_rss(8000).trim(), "7.8 GB");
    }

    // ── L14 — CPU / VRAM sort + panel title ───────────────────────

    fn proc_full(
        pid: u32,
        name: &str,
        rss_mb: u64,
        cpu_pct: f32,
        vram_bytes: Option<u64>,
    ) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: AICategory::NotAi,
            workload_category: WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct,
            rss_mb,
            vram_bytes,
            first_observed_at: Instant::now(),
        }
    }

    #[test]
    fn top_n_by_cpu_sorts_descending() {
        // Pin descending order on cpu_pct. PID-asc tiebreak isn't
        // exercised here — there's a separate test for stability.
        let state = state_with(vec![
            proc_full(1, "idle", 8_000, 2.0, None),
            proc_full(2, "busy", 100, 92.5, None),
            proc_full(3, "warm", 1_000, 45.0, None),
        ]);
        let top = top_n_by_cpu(&state, /* self_pid */ 9999, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![2, 3, 1], "CPU descending");
    }

    #[test]
    fn top_n_by_vram_sorts_descending_with_none_last() {
        // NVML returns None for vram_bytes on no-GPU systems. The
        // panel sort must place None LAST so present-GPU rows
        // lead — default `Option<T>` Ord would surface them first.
        let state = state_with(vec![
            proc_full(1, "cpu_only", 8_000, 0.0, None),
            proc_full(2, "gpu_big", 1_000, 0.0, Some(4_000_000_000)),
            proc_full(3, "gpu_small", 500, 0.0, Some(800_000_000)),
            proc_full(4, "also_cpu_only", 4_000, 0.0, None),
        ]);
        let top = top_n_by_vram(&state, /* self_pid */ 9999, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        // Some(4e9) → Some(8e8) → then the two Nones in PID-asc.
        assert_eq!(pids, vec![2, 3, 1, 4]);
    }

    #[test]
    fn top_processes_panel_vram_sort_puts_none_last() {
        // §7 lock: a single Some(_) row and a single None row.
        // Some MUST come first regardless of rss/pid. This guards
        // against an accidental "just sort by Option<u64>" change
        // (default Ord puts None < Some).
        let state = state_with(vec![
            // Make None-row "look better" by other axes (lower PID,
            // higher RSS) so a naive sort would surface it first.
            proc_full(10, "no_gpu_first_pid", 16_000, 99.0, None),
            proc_full(99, "gpu_high_pid", 100, 0.0, Some(1)),
        ]);
        let top = top_n_by_vram(&state, 9999, 10);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![99, 10], "Some(1) outranks None even at PID 99");
    }

    #[test]
    fn panel_title_reflects_current_sort_order() {
        assert_eq!(panel_title(TopProcessesSort::Ram), "Top processes (by RAM)");
        assert_eq!(panel_title(TopProcessesSort::Cpu), "Top processes (by CPU)");
        assert_eq!(panel_title(TopProcessesSort::Vram), "Top processes (by VRAM)");
    }
}
