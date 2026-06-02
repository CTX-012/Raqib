//! L6 / UX_CONTRACT.md §1 region 1, §4 — alert region rendering.
//!
//! Up to `ALERT_MAX_VISIBLE` (3) banners stack at the top of the
//! frame, in priority order resolved by [`crate::ui::alerts::AlertState::visible`].
//! When more alerts are active than fit, a `+N more` line follows.
//!
//! Each banner instantiates a contract template (`ux_contract::alerts::*`)
//! with substitutions:
//! - `{workload}` and `{pid}` come from [`AlertEntry`] (captured at
//!   fire time).
//! - `{pct}` comes from a *live* metric value resolved at render
//!   time, not from the entry — the contract template reads as a
//!   current-state value ("VRAM at {pct}% — approaching limit") and
//!   storing pct on the entry would render stale numbers.
//! - `{reason}` (`WorkloadExited` only) comes from a live lookup
//!   too. L8 owns the exit-side wiring; until then the placeholder
//!   stays "—".
//!
//! L21 / §14 — alert banners render with `theme.attention` /
//! `theme.critical` backgrounds and `theme.background` foreground so
//! the contrast pair tracks the active palette ("Background tinted:
//! amber bg for VRAM/RAM/KV, red bg for OOM/Critical"). Pre-L21 the
//! banners hardcoded `Color::Black` on `Color::Yellow`/`Red`, which
//! made `--theme light` and `--theme high-contrast` indistinguishable
//! from `dark`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use ux_contract::AlertId;

use crate::runtime::RuntimeState;
use crate::ui::alerts::{AlertEntry, AlertScope};
use crate::ui::app::App;
use crate::ui::theme::UiTheme;

/// Tier label for an `AlertId`. Drives both visibility ordering (in
/// the data layer) and banner color (here, until L21 wires themes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertTier {
    Attention,
    Critical,
}

pub fn alert_tier(alert: AlertId) -> AlertTier {
    match alert {
        AlertId::GovernorArmed | AlertId::OomDetected | AlertId::WorkloadExited => {
            AlertTier::Critical
        }
        AlertId::VramPressure | AlertId::RamPressure | AlertId::KvPressure => AlertTier::Attention,
    }
}

/// L21 / §14 — theme-driven banner background color per tier.
/// Attention banners (VRAM/RAM/KV pressure) use `theme.attention`;
/// Critical banners (governor armed / OOM / workload exited) use
/// `theme.critical`. Both contrast against `theme.background` for
/// the banner foreground.
fn tier_color(tier: AlertTier, theme: &UiTheme) -> Color {
    match tier {
        AlertTier::Attention => theme.attention,
        AlertTier::Critical => theme.critical,
    }
}

/// Live values that depend on the current tick rather than the
/// fire-time snapshot. Resolved by [`live_values_for`] from the
/// runtime state at render time.
#[derive(Debug, Clone, Default)]
pub struct LiveValues {
    /// Percent for `{pct}` substitution. Caller decides the source
    /// per alert id (system RAM%, total VRAM%, KV cache%).
    pub pct: Option<f64>,
    /// Reason string for `{reason}` (WorkloadExited only). L8 wires
    /// the real source; "—" fallback otherwise.
    pub reason: Option<String>,
}

/// Resolve live values for an entry's `{pct}` / `{reason}`
/// placeholders. Pure: reads `state` snapshots and returns a
/// freshly-computed view. The fire-time entry is used for scope/PID
/// disambiguation only.
pub fn live_values_for(entry: &AlertEntry, state: &RuntimeState) -> LiveValues {
    match entry.alert_id {
        AlertId::RamPressure => LiveValues {
            pct: state
                .last_snapshot
                .as_ref()
                .map(|s| s.system.memory_usage_percent()),
            reason: None,
        },
        AlertId::VramPressure => {
            let pid = match entry.scope {
                AlertScope::Workload(pid) => Some(pid),
                AlertScope::System => None,
            };
            let total_vram = state
                .last_snapshot
                .as_ref()
                .map(|s| s.gpu.total_vram_all_devices())
                .filter(|&v| v > 0);
            let used_vram = pid
                .and_then(|p| state.annotated.iter().find(|a| a.pid == p))
                .and_then(|a| a.vram_bytes);
            let pct = match (total_vram, used_vram) {
                (Some(total), Some(used)) => Some((used as f64 / total as f64) * 100.0),
                _ => None,
            };
            LiveValues { pct, reason: None }
        }
        AlertId::KvPressure => {
            let pct = match entry.scope {
                AlertScope::Workload(pid) => state
                    .live_telemetry
                    .get(&pid)
                    .and_then(|lt| lt.kv_cache_peak_pct.map(|v| v as f64)),
                AlertScope::System => None,
            };
            LiveValues { pct, reason: None }
        }
        AlertId::GovernorArmed | AlertId::OomDetected => {
            // Critical-tier alerts without a `{reason}` token. The
            // `Kill armed on …` and `OOM kill detected — …` templates
            // need only `{workload}`/`{pid}`, and those come from the
            // entry, not live values.
            LiveValues::default()
        }
        AlertId::WorkloadExited => {
            // L8 — `{reason}` is captured at fire time on the entry
            // (the workload is gone after exit; no live source
            // exists). Use the stored reason verbatim. `{pct}` is
            // not in the WorkloadExited template.
            LiveValues {
                pct: None,
                reason: entry.reason.clone(),
            }
        }
    }
}

/// Apply `{workload}`, `{pid}`, `{pct}`, `{reason}` substitutions to
/// a contract template. Unknown tokens are left in place (defensive
/// — surfacing a stale template on screen is better than silently
/// dropping characters).
pub fn substitute(template: &str, entry: &AlertEntry, live: &LiveValues) -> String {
    let mut out = template.to_string();
    out = out.replace("{workload}", &entry.workload_name);
    out = out.replace(
        "{pid}",
        &entry
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string()),
    );
    out = out.replace(
        "{pct}",
        &live
            .pct
            .map(|p| format!("{:.0}", p))
            .unwrap_or_else(|| "—".to_string()),
    );
    out = out.replace(
        "{reason}",
        live.reason.as_deref().unwrap_or("—"),
    );
    out
}

fn template_for(alert: AlertId) -> &'static str {
    match alert {
        AlertId::VramPressure => ux_contract::alerts::VRAM_PRESSURE,
        AlertId::RamPressure => ux_contract::alerts::RAM_PRESSURE,
        AlertId::KvPressure => ux_contract::alerts::KV_PRESSURE,
        AlertId::GovernorArmed => ux_contract::alerts::GOVERNOR_ARMED,
        AlertId::OomDetected => ux_contract::alerts::OOM_DETECTED,
        AlertId::WorkloadExited => ux_contract::alerts::WORKLOAD_EXITED,
    }
}

/// Build the lines the alert region renders, in priority order.
/// Includes a "+N more" line when `active_count > visible.len()`.
/// Used both by the render path and by tests that assert on the
/// rendered text without spinning a `TestBackend`.
pub fn build_lines(_app: &App, state: &RuntimeState, theme: &UiTheme) -> Vec<Line<'static>> {
    // v1.1.11 / DISPATCH 36 — AlertState lives on `RuntimeState`
    // (lifted from `App` per Phase 3 step 1). `_app` is retained on
    // the signature because the broader render context may want it
    // for future panel layout decisions; today the alert region
    // only needs `state`.
    let visible = state.alerts.visible();
    let total = state.alerts.active_count();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible.len() + 1);

    for entry in &visible {
        let live = live_values_for(entry, state);
        let text = substitute(template_for(entry.alert_id), entry, &live);
        let tier = alert_tier(entry.alert_id);
        // L21 / §14 — banner fg on tinted bg for contrast. Pre-L21
        // used `Color::Black` which broke against dark-palette
        // backgrounds; `theme.background` flips with the palette so
        // light-bg banners read as dark text, dark-bg banners as
        // light text.
        let style = Style::default()
            .fg(theme.background)
            .bg(tier_color(tier, theme))
            .add_modifier(Modifier::BOLD);
        lines.push(Line::from(Span::styled(format!(" {text} "), style)));
    }

    let hidden = total.saturating_sub(visible.len());
    if hidden > 0 {
        // "+N more" line is muted (no banner background) — it's a
        // count, not a banner.
        lines.push(Line::from(Span::styled(
            format!(" +{hidden} more "),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    lines
}

pub fn render(f: &mut Frame, area: Rect, app: &App, state: &RuntimeState, theme: &UiTheme) {
    let lines = build_lines(app, state, theme);
    if lines.is_empty() {
        return;
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Total rows the alert region needs given the current `AlertState`
/// — used by the top-level layout in `panels/mod.rs` to reserve
/// exactly the right number of rows. Returns 0 when no alerts fit
/// or are active.
pub fn region_height(state: &RuntimeState) -> u16 {
    // v1.1.11 / DISPATCH 36 — argument changed from `&App` to
    // `&RuntimeState` because the AlertState moved to `RuntimeState`.
    // Caller in `panels/mod.rs` updates accordingly.
    let visible = state.alerts.visible().len();
    let plus_more = if state.alerts.active_count() > visible {
        1
    } else {
        0
    };
    (visible + plus_more) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::alerts::WorkloadRef;
    use crate::ui::theme::current_theme;
    use std::time::{Duration, Instant};

    fn after(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    /// Construct an App with a fresh AlertState. Tests directly call
    /// `App::alerts_mut().observe(...)` to inject alert entries
    /// rather than driving the metric pipeline end-to-end.
    fn empty_app() -> App {
        App::new()
    }

    fn test_theme() -> UiTheme {
        current_theme("dark")
    }

    fn lines_to_string(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn lines_to_styles(lines: &[Line<'_>]) -> Vec<Style> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.style))
            .collect()
    }

    fn empty_state() -> RuntimeState {
        RuntimeState::default()
    }

    #[test]
    fn substitute_replaces_workload_pid_pct_reason() {
        let entry = AlertEntry {
            alert_id: AlertId::VramPressure,
            scope: AlertScope::Workload(4523),
            pid: Some(4523),
            workload_name: "Llama-70B".into(),
            fired_at: Instant::now(),
            reason: None,
        };
        let live = LiveValues {
            pct: Some(91.4),
            reason: Some("OOM".into()),
        };
        let out = substitute(ux_contract::alerts::VRAM_PRESSURE, &entry, &live);
        assert_eq!(
            out,
            "VRAM at 91% — Llama-70B (PID 4523) approaching limit"
        );
    }

    #[test]
    fn substitute_uses_em_dash_when_pct_or_reason_missing() {
        // Defensive: a template with `{pct}` but no live value
        // resolves to "—" rather than leaving the placeholder
        // visible. Rendering a literal "{pct}" on screen would be a
        // worse UX than admitting the metric is missing.
        let entry = AlertEntry {
            alert_id: AlertId::VramPressure,
            scope: AlertScope::Workload(4523),
            pid: Some(4523),
            workload_name: "x".into(),
            fired_at: Instant::now(),
            reason: None,
        };
        let live = LiveValues::default();
        let out = substitute(ux_contract::alerts::VRAM_PRESSURE, &entry, &live);
        assert!(out.contains("VRAM at —%"), "missing pct should become —: {out}");
    }

    #[test]
    fn substitutes_pct_from_live_values_not_entry() {
        // L6 design lock: the entry stores fire-time scope/name only.
        // Live pct overrides any snapshot value at render time.
        // This test exists so a future "let's cache pct on the entry"
        // refactor breaks loudly.
        let entry = AlertEntry {
            alert_id: AlertId::VramPressure,
            scope: AlertScope::Workload(1),
            pid: Some(1),
            workload_name: "phi3".into(),
            fired_at: Instant::now(),
            reason: None,
        };
        let live = LiveValues {
            pct: Some(92.0),
            reason: None,
        };
        let out = substitute(ux_contract::alerts::VRAM_PRESSURE, &entry, &live);
        assert!(
            out.contains("VRAM at 92%"),
            "live pct must reach the rendered string: {out}"
        );
    }

    #[test]
    fn alert_tier_maps_governor_armed_to_critical() {
        assert_eq!(alert_tier(AlertId::GovernorArmed), AlertTier::Critical);
        assert_eq!(alert_tier(AlertId::OomDetected), AlertTier::Critical);
        assert_eq!(alert_tier(AlertId::WorkloadExited), AlertTier::Critical);
    }

    #[test]
    fn alert_tier_maps_pressure_to_attention() {
        assert_eq!(alert_tier(AlertId::VramPressure), AlertTier::Attention);
        assert_eq!(alert_tier(AlertId::RamPressure), AlertTier::Attention);
        assert_eq!(alert_tier(AlertId::KvPressure), AlertTier::Attention);
    }

    #[test]
    fn alert_region_renders_active_alert() {
        let app = empty_app();
        let mut state = empty_state();
        let now = Instant::now();
        state.alerts.observe(
            now,
            WorkloadRef::workload(206, "phi3"),
            AlertId::GovernorArmed,
            true,
        );
        let lines = build_lines(&app, &state, &test_theme());
        let text = lines_to_string(&lines);
        assert!(text.contains("Kill armed on phi3"), "{text}");
        assert!(text.contains("(PID 206)"), "{text}");
    }

    #[test]
    fn alert_region_uses_critical_color_for_governor_armed() {
        let app = empty_app();
        let mut state = empty_state();
        state.alerts.observe(
            Instant::now(),
            WorkloadRef::workload(206, "phi3"),
            AlertId::GovernorArmed,
            true,
        );
        let styles = lines_to_styles(&build_lines(&app, &state, &test_theme()));
        assert_eq!(styles.len(), 1);
        assert_eq!(
            styles[0].bg,
            Some(tier_color(AlertTier::Critical, &test_theme())),
            "governor-armed banner must render with the critical-tier bg"
        );
    }

    #[test]
    fn alert_region_uses_attention_color_for_vram_pressure() {
        let app = empty_app();
        let mut state = empty_state();
        let start = Instant::now();
        // Drive VRAM through its sustain gate.
        state.alerts.observe(
            start,
            WorkloadRef::workload(206, "phi3"),
            AlertId::VramPressure,
            true,
        );
        state.alerts.observe(
            after(start, 5),
            WorkloadRef::workload(206, "phi3"),
            AlertId::VramPressure,
            true,
        );
        let styles = lines_to_styles(&build_lines(&app, &state, &test_theme()));
        assert_eq!(
            styles[0].bg,
            Some(tier_color(AlertTier::Attention, &test_theme()))
        );
    }

    #[test]
    fn alert_region_renders_plus_n_when_active_count_above_three() {
        let app = empty_app();
        let mut state = empty_state();
        let now = Instant::now();
        // Five instant alerts on different PIDs.
        for pid in 100u32..105 {
            state.alerts.observe(
                now,
                WorkloadRef::workload(pid, "phi3"),
                AlertId::OomDetected,
                true,
            );
        }
        let lines = build_lines(&app, &state, &test_theme());
        assert_eq!(lines.len(), 4, "3 banners + 1 +N more line");
        let last = lines_to_string(std::slice::from_ref(&lines[3]));
        assert!(last.contains("+2 more"), "last line: {last}");
    }

    #[test]
    fn alert_region_does_not_render_suppressed_alerts() {
        let app = empty_app();
        let mut state = empty_state();
        let now = Instant::now();
        state.alerts.observe(
            now,
            WorkloadRef::workload(206, "phi3"),
            AlertId::GovernorArmed,
            true,
        );
        assert_eq!(build_lines(&app, &state, &test_theme()).len(), 1);
        // Ack moves the slot from Active to Suppressed; visible() —
        // and therefore build_lines — must drop it.
        state.alerts.ack_all();
        assert_eq!(build_lines(&app, &state, &test_theme()).len(), 0);
    }

    #[test]
    fn workload_exited_substitutes_workload_and_reason_from_entry() {
        // L8 — `{reason}` for WorkloadExited comes from the entry
        // (captured at fire time), not from a live source. Pin
        // against the contract template directly so a future
        // local-literal regression breaks here. Note: v0.3.2's
        // WORKLOAD_EXITED template is "{workload} exited with
        // {reason} — press Enter for post-mortem" — it has NO
        // `{pid}` token (unlike OOM_DETECTED which does), so this
        // test asserts only on workload + reason ordering.
        let app = empty_app();
        let mut state = empty_state();
        state.alerts.observe_exit(
            Instant::now(),
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::WorkloadExited,
            Some("exit code 139".into()),
        );
        let text = lines_to_string(&build_lines(&app, &state, &test_theme()));
        assert!(
            text.contains("Llama-70B exited with exit code 139"),
            "template assembly wrong: {text}"
        );
        assert!(
            text.contains("press Enter for post-mortem"),
            "trailing literal missing: {text}"
        );
    }

    #[test]
    fn oom_detected_substitutes_workload_pid_no_reason_token() {
        // L8 — OomDetected's template has no `{reason}` placeholder
        // (it's just "OOM kill detected — {workload} (PID {pid})
        // terminated by kernel"). Confirm the entry's reason field
        // (if any) is ignored and the substitution still produces
        // the expected text.
        let app = empty_app();
        let mut state = empty_state();
        state.alerts.observe_exit(
            Instant::now(),
            WorkloadRef::workload(206, "phi3"),
            AlertId::OomDetected,
            None,
        );
        let text = lines_to_string(&build_lines(&app, &state, &test_theme()));
        assert!(
            text.contains("OOM kill detected — phi3 (PID 206) terminated by kernel"),
            "{text}"
        );
    }

    #[test]
    fn region_height_matches_visible_plus_overflow_indicator() {
        let mut state = empty_state();
        let now = Instant::now();
        // Two instant alerts → height = 2.
        for pid in 100u32..102 {
            state.alerts.observe(
                now,
                WorkloadRef::workload(pid, "phi3"),
                AlertId::OomDetected,
                true,
            );
        }
        assert_eq!(region_height(&state), 2);
        // Five instant alerts → height = 3 (cap) + 1 ("+N more") = 4.
        for pid in 102u32..105 {
            state.alerts.observe(
                now,
                WorkloadRef::workload(pid, "phi3"),
                AlertId::OomDetected,
                true,
            );
        }
        assert_eq!(region_height(&state), 4);
    }
}
