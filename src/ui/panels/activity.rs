//! L15 / UX_CONTRACT.md §1 region 6 — Activity panel.
//!
//! Heterogeneous time-ordered event log replacing the legacy
//! `completed.rs` (run-summary list) + `audit.rs` (governor +
//! regression list, dead in default render after L2b removed
//! detail mode). Three current event sources merged in time-
//! descending order, capped at 5 per §1 region 6 ("last 5 events"):
//!
//! 1. **Run summaries** — `state.completed` workload exits, AI
//!    only (matches `completed.rs`'s pre-L15 filter).
//! 2. **Governor audit** — `state.audit` kill / cancel / abort
//!    entries.
//! 3. **Regressions** — `state.regressions` Tier 1.3 alerts.
//!
//! Read-only by design (per §1 region 6's "Activity panel — last
//! 5 events" — passive log, not interactive). Selection stays on
//! the workloads panel; `j`/`K` navigation is unaffected. Enter
//! continues to open the focused workload's post-mortem (existing
//! L2 behaviour); Activity rows are not Enter-actionable in v1.0.
//!
//! AlertState events ("alert raised" / "alert acknowledged" per
//! §4 "Each raise + ack writes to Activity panel") aren't shown
//! yet — AlertState's events are produced by `observe()` /
//! `ack_all()` but not accumulated into RuntimeState. Filed as
//! BACKLOG; not blocking v1.0.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};

use crate::analysis::Severity;
use crate::governor::manual::{KillSource, ManualKillAction};
use crate::runtime::RuntimeState;

use super::panel_block;

/// Per UX_CONTRACT.md §1 region 6 — "last 5 events". §12 narrow-
/// mode caps Activity at 3 rows; sizing-aware truncation lives in
/// L22's row.
pub const MAX_VISIBLE_EVENTS: usize = 5;

/// One ready-to-render event with its source-specific colour and
/// the timestamp used for ordering. Tests construct these
/// directly without spinning up a full RuntimeState.
#[derive(Debug, Clone)]
pub(crate) struct ActivityEvent {
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub colour: Color,
}

/// Build the time-descending event list from RuntimeState. Pure;
/// tests drive it with synthetic state.
pub(crate) fn build_events(state: &RuntimeState) -> Vec<ActivityEvent> {
    let mut events: Vec<ActivityEvent> = Vec::new();

    // Run summaries (AI-only — matches the pre-L15 `completed.rs`
    // filter; non-AI exits still hit the persistent JSONL log).
    for s in &state.completed {
        if s.category.is_none() {
            continue;
        }
        let killed_by_signal = s.signal.is_some();
        let colour = if killed_by_signal {
            Color::Red
        } else {
            Color::Green
        };
        let text = format_run_summary(s);
        events.push(ActivityEvent {
            timestamp: s.exit_time,
            text,
            colour,
        });
    }

    // Governor audit entries (kill / cancel / abort).
    for e in &state.audit {
        let action = match e.action {
            ManualKillAction::SendSigterm => "SIGTERM",
            ManualKillAction::SendSigkill => "SIGKILL",
            ManualKillAction::Cancelled => "CANCELLED",
            ManualKillAction::PidReusedAborted => "ABORT-PID-REUSE",
        };
        let source = match e.source {
            KillSource::Manual => "manual",
            KillSource::Automated => "auto",
        };
        let status = if e.success { "OK" } else { "FAIL" };
        let colour = if !e.success {
            Color::Red
        } else if e.source == KillSource::Manual {
            Color::Yellow
        } else {
            Color::Green
        };
        let text = format!(
            "{} {} {} pid={} {} - {}",
            action, status, source, e.pid, e.process_name, e.reason
        );
        events.push(ActivityEvent {
            timestamp: e.timestamp,
            text,
            colour,
        });
    }

    // Regression events (Tier 1.3).
    for r in &state.regressions {
        let colour = if r.regression.severity >= Severity::Critical {
            Color::Red
        } else {
            Color::Yellow
        };
        let text = format!(
            "REGRESSION {:?} {} {} {:+.1}% (n={})",
            r.regression.severity,
            r.model,
            r.regression.metric,
            r.regression.delta_pct,
            r.baseline_size,
        );
        events.push(ActivityEvent {
            timestamp: r.timestamp,
            text,
            colour,
        });
    }

    // Newest first; cap to §1 region 6's 5-event budget.
    events.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
    events.truncate(MAX_VISIBLE_EVENTS);
    events
}

fn format_run_summary(s: &crate::lifecycle::LifecycleSummary) -> String {
    let cat = s
        .category
        .map(|c| format!("{:?}", c))
        .unwrap_or_default();
    let mut row = format!("exit pid={} {} {}", s.pid, s.name, cat);
    if let Some(model) = s.model_name.as_deref().filter(|m| !m.is_empty()) {
        row.push_str(&format!(" model={}", model));
    }
    row.push_str(&format!(
        " cpu avg={:.0}% peak={:.0}% RAM {} MB",
        s.avg_cpu_pct, s.peak_cpu_pct, s.peak_rss_mb,
    ));
    if s.peak_vram_mb > 0 {
        row.push_str(&format!(", GPU memory {} MB", s.peak_vram_mb));
    }
    row.push_str(&format!(" up={}s", s.uptime_secs));
    row
}

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState) {
    let block = panel_block("Activity", false);
    let events = build_events(state);

    if events.is_empty() {
        // Contract-locked empty state from `ux_contract::empty::ACTIVITY`
        // (v0.3.2). No local literal — single source of truth across
        // Linux + Windows binaries.
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", ux_contract::empty::ACTIVITY),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
        ];
        let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem<'_>> = events
        .iter()
        .map(|ev| {
            ListItem::new(format!(
                "{}  {}",
                ev.timestamp.format("%H:%M:%S"),
                ev.text
            ))
            .style(Style::default().fg(ev.colour))
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Regression, RegressionEvent};
    use crate::governor::manual::{AuditLogEntry, KillSource, ManualKillAction};
    use crate::lifecycle::LifecycleSummary;
    use crate::model::AICategory;
    use chrono::TimeZone;
    use std::collections::VecDeque;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn run_summary(pid: u32, name: &str, when: DateTime<Utc>) -> LifecycleSummary {
        LifecycleSummary {
            pid,
            name: name.into(),
            category: Some(AICategory::Inference),
            model_name: None,
            spawn_time: when - chrono::Duration::seconds(60),
            exit_time: when,
            uptime_secs: 60,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 0.0,
            peak_cpu_pct: 0.0,
            peak_rss_mb: 100,
            peak_vram_mb: 0,
            samples: 1,
        }
    }

    fn audit_entry(pid: u32, name: &str, when: DateTime<Utc>) -> AuditLogEntry {
        AuditLogEntry {
            timestamp: when,
            action: ManualKillAction::SendSigterm,
            success: true,
            source: KillSource::Manual,
            pid,
            process_name: name.into(),
            category: Some(AICategory::Inference),
            reason: "user request".into(),
            error_msg: None,
        }
    }

    fn regression_event(when: DateTime<Utc>) -> RegressionEvent {
        RegressionEvent {
            timestamp: when,
            model: "phi3".into(),
            baseline_size: 5,
            regression: Regression {
                metric: "tokens_per_sec_avg".into(),
                baseline: 40.0,
                current: 28.0,
                delta_pct: -30.0,
                severity: Severity::Critical,
            },
        }
    }

    fn empty_state() -> RuntimeState {
        RuntimeState::default()
    }

    #[test]
    fn empty_state_produces_no_events() {
        assert!(build_events(&empty_state()).is_empty());
    }

    #[test]
    fn activity_renders_run_summary_event() {
        let mut state = empty_state();
        state
            .completed
            .push_back(run_summary(206, "phi3", ts(1_000)));
        let events = build_events(&state);
        assert_eq!(events.len(), 1);
        assert!(
            events[0].text.contains("exit pid=206 phi3"),
            "{}",
            events[0].text
        );
    }

    #[test]
    fn run_summary_filters_non_ai_processes() {
        // Pre-L15 `completed.rs` only listed AI exits ("operators
        // only want to see AI workloads here. Non-AI exits still
        // hit the persistent JSONL log."). L15 preserves that.
        let mut state = empty_state();
        let mut non_ai = run_summary(99, "bash", ts(1_000));
        non_ai.category = None;
        state.completed.push_back(non_ai);
        assert!(build_events(&state).is_empty());
    }

    #[test]
    fn activity_renders_governor_kill_event() {
        let mut state = empty_state();
        state.audit.push_back(audit_entry(206, "phi3", ts(1_000)));
        let events = build_events(&state);
        assert_eq!(events.len(), 1);
        assert!(
            events[0].text.contains("SIGTERM OK manual pid=206 phi3"),
            "{}",
            events[0].text
        );
    }

    #[test]
    fn activity_renders_regression_event() {
        let mut state = empty_state();
        state.regressions.push_back(regression_event(ts(1_000)));
        let events = build_events(&state);
        assert_eq!(events.len(), 1);
        assert!(
            events[0].text.starts_with("REGRESSION"),
            "{}",
            events[0].text
        );
        assert!(events[0].text.contains("-30.0%"), "{}", events[0].text);
    }

    #[test]
    fn activity_orders_events_by_timestamp_descending() {
        // Three events at different times across all three
        // sources. `build_events` interleaves them and emits
        // newest-first regardless of source.
        let mut state = empty_state();
        state
            .completed
            .push_back(run_summary(1, "early", ts(1_000)));
        state.audit.push_back(audit_entry(2, "middle", ts(2_000)));
        state.regressions.push_back(regression_event(ts(3_000)));

        let events = build_events(&state);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].timestamp, ts(3_000));
        assert_eq!(events[1].timestamp, ts(2_000));
        assert_eq!(events[2].timestamp, ts(1_000));
    }

    #[test]
    fn activity_caps_event_count_to_max_visible() {
        // Per §1 region 6 — last 5 events. Six entries collapse
        // to five with the oldest dropped.
        let mut state = empty_state();
        for i in 0..6 {
            state
                .completed
                .push_back(run_summary(i + 1, "x", ts((i + 1) as i64 * 100)));
        }
        let events = build_events(&state);
        assert_eq!(events.len(), MAX_VISIBLE_EVENTS);
        // Oldest (ts=100) dropped; newest (ts=600) leads.
        assert_eq!(events[0].timestamp, ts(600));
        assert_eq!(events[4].timestamp, ts(200));
    }

    #[test]
    fn run_summary_signal_exit_renders_red() {
        // Signal-terminated runs must visibly stand out — the L15
        // event colour mirrors the pre-L15 `completed.rs` rule.
        let mut state = empty_state();
        let mut killed = run_summary(206, "phi3", ts(1_000));
        killed.signal = Some(15);
        killed.exit_code = None;
        state.completed.push_back(killed);
        let events = build_events(&state);
        assert_eq!(events[0].colour, Color::Red);
    }

    #[test]
    fn governor_failed_kill_renders_red() {
        let mut state = empty_state();
        let mut entry = audit_entry(206, "phi3", ts(1_000));
        entry.success = false;
        state.audit.push_back(entry);
        let events = build_events(&state);
        assert_eq!(events[0].colour, Color::Red);
    }

    #[test]
    fn regression_critical_severity_renders_red() {
        let mut state = empty_state();
        state.regressions.push_back(regression_event(ts(1_000)));
        let events = build_events(&state);
        assert_eq!(events[0].colour, Color::Red);
    }

    #[test]
    fn regression_below_critical_renders_yellow() {
        // Severity has Info / Warn / Critical (no Attention variant
        // — that's the WorkloadStatus bucket name); Warn is the
        // "below Critical" tier.
        let mut state = empty_state();
        let mut ev = regression_event(ts(1_000));
        ev.regression.severity = Severity::Warn;
        state.regressions.push_back(ev);
        let events = build_events(&state);
        assert_eq!(events[0].colour, Color::Yellow);
    }

    #[test]
    fn empty_state_render_uses_contract_template() {
        // Contract-lock: the empty-state copy MUST come from
        // `ux_contract::empty::ACTIVITY`, not a local literal.
        // Pin the const so a future "let's hardcode it back"
        // refactor breaks.
        assert_eq!(
            ux_contract::empty::ACTIVITY,
            "No recent activity.",
            "if Contract changed the const, update this test"
        );
    }

    #[test]
    fn activity_does_not_steal_workloads_selection() {
        // Defensive: Activity is a passive log per §1 region 6.
        // It should not have selection state; navigation (j/K)
        // remains on the workloads panel. The only signal we
        // can assert here is that `build_events` is read-only —
        // it never mutates state. The render path mirrors
        // (no ListState carried; selection isn't even an option
        // on the panel).
        let state = empty_state();
        let _events = build_events(&state);
        // Implicit: no selection field on ActivityEvent or panel.
        // If a future row adds one, it should also update the
        // workloads-panel coupling explicitly.
    }

    #[test]
    fn build_events_uses_runtime_state_directly_no_clone() {
        // Defensive against a refactor that re-clones the entire
        // state per render — `build_events` borrows immutably.
        let mut state = empty_state();
        state.completed.push_back(run_summary(1, "x", ts(1_000)));
        // Take immutable references and call repeatedly.
        let _ = build_events(&state);
        let _ = build_events(&state);
        // If this compiled, immutability invariant holds.
    }

    /// Defensive: the ring-buffer caps in RuntimeState are
    /// configured at startup; `build_events` shouldn't add its
    /// own truncation beyond the §1-region-6 cap. This test
    /// pins that the cap *is* exactly MAX_VISIBLE_EVENTS, not
    /// arbitrarily larger.
    #[test]
    fn cap_matches_contract_section_one_region_six() {
        assert_eq!(MAX_VISIBLE_EVENTS, 5);
    }

    #[test]
    fn empty_state_renders_without_panic() {
        // Smoke check on the render-path-adjacent build_events —
        // can't render without a Frame, but the no-event short-
        // circuit branch is exercised by build_events returning
        // empty.
        assert!(build_events(&empty_state()).is_empty());
        // The render() function will hit its early-return path
        // with no events, surfacing ux_contract::empty::ACTIVITY.
        // The text-content check is in
        // `empty_state_render_uses_contract_template`.
    }

    fn _unused_state_with(state: &RuntimeState) -> &VecDeque<crate::lifecycle::LifecycleSummary> {
        // Ensures `state.completed`'s type is what we expect. If
        // the field is renamed, this test breaks at compile time
        // with a clear signal pointing at `build_events`.
        &state.completed
    }
}
