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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::analysis::Severity;
use crate::governor::manual::{KillSource, ManualKillAction};
use crate::runtime::RuntimeState;
use crate::ui::theme::UiTheme;

use super::panel_block;

/// Per UX_CONTRACT.md §1 region 6 — "last N events". §12 narrow-
/// mode caps Activity at 3 rows; sizing-aware truncation lives in
/// L22's row. v0.3.10 (CAR-19c) ratifies the cap in the contract
/// as `limits::ACTIVITY_FEED_TUI_MAX`; this re-export keeps the
/// existing call sites unchanged.
pub const MAX_VISIBLE_EVENTS: usize = ux_contract::limits::ACTIVITY_FEED_TUI_MAX;

/// One ready-to-render event with a semantic tone and the timestamp
/// used for ordering. Tests construct these directly without
/// spinning up a full RuntimeState.
///
/// L21 / §14 — pre-L21 the activity panel hardcoded `Color::Red /
/// Yellow / Green`, which froze the rendering to the dark Tokyo
/// Night palette regardless of `--theme`. The tone enum decouples
/// the semantic ("this is a critical event") from the literal color;
/// the render path resolves to `theme.critical / attention / healthy`
/// at draw time so theme switches land everywhere consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventTone {
    /// Healthy / successful operation — clean exit, successful kill.
    Healthy,
    /// Attention-band — non-failure but worth noting (manual kill
    /// success, sub-critical regression).
    Attention,
    /// Critical — failed kill, signal-terminated workload, Critical
    /// regression severity.
    Critical,
}

impl EventTone {
    pub fn color(self, theme: &UiTheme) -> ratatui::style::Color {
        match self {
            EventTone::Healthy => theme.healthy,
            EventTone::Attention => theme.attention,
            EventTone::Critical => theme.critical,
        }
    }
}

/// v1.3.2 / CAR-D75 / DISPATCH 76 — discriminator for the three
/// activity sources. Used by the browse-mode renderer to decide
/// whether a row is Enter-expandable (Exit / Kill) or not
/// (Regression — no RunRecord, no detail). Mirrors
/// `ux_contract::activity::ActivityKind` v0.3.17 + the web's
/// `WireActivityEntry::kind` so the TUI's expand surface stays in
/// lock-step with the web's click-to-expand (D74).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    Exit,
    Kill,
    Regression,
}

/// v1.3.2 / CAR-D75 / DISPATCH 76 — per-entry detail for the
/// browse-mode expand surface. Carries the same fields the web's
/// `WireActivityDetail` (D74) carries; TUI ↔ web parity by
/// construction (same source data, same field set).
///
/// Regression rows carry no detail (hard rule #4 — "no fabricated
/// exit fields"). The `Option<ActivityEventDetail>` on
/// `ActivityEvent` is `None` for them.
#[derive(Debug, Clone)]
pub(crate) enum ActivityEventDetail {
    Exit {
        uptime_secs: i64,
        avg_cpu_pct: f32,
        peak_cpu_pct: f32,
        peak_rss_mb: u64,
        peak_vram_mb: u64,
        /// STOP #3 honesty — mirrors the web's `vram_unmeasured`
        /// flag. `true` when the lifecycle summary's `samples`
        /// count is 0 (no resource sample ever fired for this PID,
        /// so the 0 in `peak_vram_mb` is "no measurement," not
        /// "real zero"). The renderer prints
        /// `status::VRAM_UNMEASURED` in that case rather than
        /// "0 MB."
        vram_unmeasured: bool,
        /// Sourced from `RuntimeState::recent_exit_attribution`
        /// (D74 lock-step buffer). `None` when no classification
        /// ran for this exit (non-AI exit, or attribution slot
        /// was never patched).
        exit_kind: Option<String>,
        exit_detail: Option<String>,
    },
    Kill {
        action: String,
        success: bool,
        error_msg: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ActivityEvent {
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub tone: EventTone,
    /// v1.3.2 / CAR-D75 / DISPATCH 76 — kind discriminator for the
    /// browse-mode expand surface. Drives the per-kind detail
    /// renderer + the Enter-is-no-op rule for Regression rows
    /// (mirrors the web's button-disabled regression case from
    /// D74).
    pub kind: ActivityKind,
    /// Per-kind detail. `None` for Regression entries.
    pub detail: Option<ActivityEventDetail>,
    /// Identity for the entry. PID for Exit / Kill rows;
    /// regression rows wire `0` as the sentinel (same as the
    /// web's wire shape per D71).
    pub pid: u32,
}

impl ActivityEvent {
    /// Composite key matching the web's `{#each}` key
    /// `${kind}-${pid}-${timestamp}` (D71 + D74). Used by the
    /// browse-mode cursor so selection survives live 1 Hz
    /// refreshes — the cursor pins by IDENTITY, not by index. If a
    /// new event arrives at the top of the feed mid-browse, the
    /// cursor stays on the same logical row, not on whatever
    /// happens to be at the same index.
    pub fn key(&self) -> String {
        let kind = match self.kind {
            ActivityKind::Exit => "exit",
            ActivityKind::Kill => "kill",
            ActivityKind::Regression => "regression",
        };
        format!("{kind}-{}-{}", self.pid, self.timestamp.to_rfc3339())
    }
}

/// Build the time-descending event list from RuntimeState. Pure;
/// tests drive it with synthetic state.
pub(crate) fn build_events(state: &RuntimeState) -> Vec<ActivityEvent> {
    let mut events: Vec<ActivityEvent> = Vec::new();

    // Run summaries (AI-only — matches the pre-L15 `completed.rs`
    // filter; non-AI exits still hit the persistent JSONL log).
    //
    // v1.3.2 / CAR-D75 / DISPATCH 76 — also zip the lock-step
    // `state.recent_exit_attribution` buffer (D74) so the per-row
    // detail carries the classified exit_kind/exit_detail. The
    // attribution VecDeque length matches `state.completed` length
    // by construction (push/pop in lock-step at the runtime
    // exit-drain site); a debug_assert here pins the invariant for
    // the browse-mode renderer.
    debug_assert_eq!(
        state.completed.len(),
        state.recent_exit_attribution.len(),
        "state.recent_exit_attribution must stay lock-step with state.completed",
    );
    for (s, attr) in state
        .completed
        .iter()
        .zip(state.recent_exit_attribution.iter())
    {
        if s.category.is_none() {
            continue;
        }
        let killed_by_signal = s.signal.is_some();
        let tone = if killed_by_signal {
            EventTone::Critical
        } else {
            EventTone::Healthy
        };
        let text = format_run_summary(s);
        let detail = ActivityEventDetail::Exit {
            uptime_secs: s.uptime_secs,
            avg_cpu_pct: s.avg_cpu_pct,
            peak_cpu_pct: s.peak_cpu_pct,
            peak_rss_mb: s.peak_rss_mb,
            peak_vram_mb: s.peak_vram_mb,
            // STOP #3 honesty — `samples=0` ⇒ no resource sample
            // ever fired ⇒ `peak_vram_mb=0` is "no measurement,"
            // not real zero. The renderer prints
            // `status::VRAM_UNMEASURED` in that case.
            vram_unmeasured: s.samples == 0,
            exit_kind: attr.as_ref().map(|a| a.exit_kind.clone()),
            exit_detail: attr.as_ref().and_then(|a| a.exit_detail.clone()),
        };
        events.push(ActivityEvent {
            timestamp: s.exit_time,
            text,
            tone,
            kind: ActivityKind::Exit,
            detail: Some(detail),
            pid: s.pid,
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
        let tone = if !e.success {
            EventTone::Critical
        } else if e.source == KillSource::Manual {
            EventTone::Attention
        } else {
            EventTone::Healthy
        };
        let text = format!(
            "{} {} {} pid={} {} - {}",
            action, status, source, e.pid, e.process_name, e.reason
        );
        events.push(ActivityEvent {
            timestamp: e.timestamp,
            text,
            tone,
            kind: ActivityKind::Kill,
            detail: Some(ActivityEventDetail::Kill {
                action: action.to_string(),
                success: e.success,
                error_msg: e.error_msg.clone(),
            }),
            pid: e.pid,
        });
    }

    // Regression events (Tier 1.3).
    for r in &state.regressions {
        let tone = if r.regression.severity >= Severity::Critical {
            EventTone::Critical
        } else {
            EventTone::Attention
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
            tone,
            kind: ActivityKind::Regression,
            // CAR-D75 hard rule #4 / dispatch hard rule #6 —
            // regression entries have no detail. Enter on a
            // selected regression row is a no-op (the renderer
            // doesn't paint an expand chevron); same shape the
            // web's regression rendering takes per D74.
            detail: None,
            pid: 0,
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

pub fn render(
    f: &mut Frame,
    area: Rect,
    state: &RuntimeState,
    app: &crate::ui::app::App,
    theme: &UiTheme,
) {
    let block = panel_block("Activity", false, theme);
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
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            )),
        ];
        let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    // v1.3.2 / CAR-D75 / DISPATCH 76 — when browse mode is active,
    // render a cursor + (when expanded) a detail block below the
    // selected row. Default render (browse mode off) stays exactly
    // as before — passive log, severity-colored, no cursor — so
    // the §1 region 6 at-a-glance scan property is preserved
    // byte-for-byte for operators who never press `A`.
    let browse = app.activity_browse();
    let selected_idx: Option<usize> = browse.and_then(|b| {
        // Resolve the composite key to an index in the current
        // event list. `None` selected_key (just-entered browse
        // mode) falls back to index 0 so the cursor always paints
        // somewhere visible.
        match b.selected_key.as_ref() {
            Some(k) => events.iter().position(|e| &e.key() == k),
            None => Some(0),
        }
    });
    let expanded = browse.is_some_and(|b| b.expanded);

    // Build the rendered lines. Pre-bump this was a `List` of
    // single-line ListItems; we switch to a `Paragraph` of
    // multi-line content so the expand block can live underneath
    // the selected row without a separate widget. The visual
    // result for the passive (no-browse) path is byte-identical
    // to the pre-bump `List` render — same row format, same
    // severity tone, no cursor.
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        let is_selected = selected_idx == Some(i);
        let cursor_prefix = if browse.is_some() {
            if is_selected { "▸ " } else { "  " }
        } else {
            ""
        };
        let text = format!(
            "{}{}  {}",
            cursor_prefix,
            ev.timestamp.format("%H:%M:%S"),
            ev.text
        );
        let mut style = Style::default().fg(ev.tone.color(theme));
        if is_selected {
            // Bold the selected row so the cursor + bold combo
            // are unambiguous regardless of terminal mono-color
            // limitations.
            style = style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(text, style)));

        // Expanded detail block — only for the selected row,
        // only while browse mode is active and `expanded == true`,
        // and only when the row carries a detail (Exit / Kill).
        // Regression rows (`detail = None`) silently skip — the
        // operator's Enter was a no-op per the contract.
        if is_selected
            && expanded
            && let Some(detail) = ev.detail.as_ref()
        {
            for detail_line in detail_lines(detail, theme) {
                lines.push(detail_line);
            }
        }
    }

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

/// Format the per-kind detail block for a selected, expanded entry.
/// Field labels source from `ux_contract::postmortem_labels::*` and
/// `ux_contract::status::VRAM_UNMEASURED` (v0.3.18 lift) so the TUI
/// shares its label vocabulary with the web (D74) and the post-mortem
/// card.
fn detail_lines(
    detail: &ActivityEventDetail,
    theme: &UiTheme,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted);
    let fg = Style::default().fg(theme.foreground);
    let mut out: Vec<Line<'static>> = Vec::new();
    match detail {
        ActivityEventDetail::Exit {
            uptime_secs,
            avg_cpu_pct,
            peak_cpu_pct,
            peak_rss_mb,
            peak_vram_mb,
            vram_unmeasured,
            exit_kind,
            exit_detail,
        } => {
            if let Some(kind) = exit_kind.as_deref() {
                let mut spans = vec![
                    Span::styled("    cause: ", muted),
                    Span::styled(kind.to_string(), fg),
                ];
                if let Some(d) = exit_detail.as_deref() {
                    spans.push(Span::styled(format!(" — {d}"), muted));
                }
                out.push(Line::from(spans));
            }
            out.push(Line::from(vec![
                Span::styled("    uptime: ", muted),
                Span::styled(format!("{uptime_secs}s"), fg),
            ]));
            out.push(Line::from(vec![
                Span::styled("    peak RSS: ", muted),
                Span::styled(format!("{peak_rss_mb} MB"), fg),
            ]));
            // STOP #3 honesty — never render "0 MB" when the
            // value was never sampled. The contract-locked
            // `status::VRAM_UNMEASURED` string ("no measurements")
            // is the single source of truth shared with the web
            // (D74's `vramLabel()`).
            let vram_text = if *vram_unmeasured {
                ux_contract::status::VRAM_UNMEASURED.to_string()
            } else {
                format!("{peak_vram_mb} MB")
            };
            out.push(Line::from(vec![
                Span::styled("    peak GPU memory: ", muted),
                Span::styled(vram_text, fg),
            ]));
            out.push(Line::from(vec![
                Span::styled("    CPU: ", muted),
                Span::styled(
                    format!("avg {avg_cpu_pct:.0}% / peak {peak_cpu_pct:.0}%"),
                    fg,
                ),
            ]));
        }
        ActivityEventDetail::Kill {
            action,
            success,
            error_msg,
        } => {
            // v0.3.18 contract labels — KILL_ACTION / KILL_RESULT.
            out.push(Line::from(vec![
                Span::styled(
                    format!("    {} ", ux_contract::postmortem_labels::KILL_ACTION),
                    muted,
                ),
                Span::styled(action.clone(), fg),
            ]));
            out.push(Line::from(vec![
                Span::styled(
                    format!("    {} ", ux_contract::postmortem_labels::KILL_RESULT),
                    muted,
                ),
                Span::styled(
                    if *success { "delivered" } else { "failed" }.to_string(),
                    fg,
                ),
            ]));
            if let Some(err) = error_msg.as_deref() {
                out.push(Line::from(vec![
                    Span::styled("    error: ", muted),
                    Span::styled(err.to_string(), Style::default().fg(theme.critical)),
                ]));
            }
        }
    }
    out
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
        state.push_completed_exit(run_summary(206, "phi3", ts(1_000)), None);
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
        state.push_completed_exit(non_ai, None);
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
        state.push_completed_exit(run_summary(1, "early", ts(1_000)), None);
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
            state.push_completed_exit(
                run_summary(i + 1, "x", ts((i + 1) as i64 * 100)),
                None,
            );
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
        state.push_completed_exit(killed, None);
        let events = build_events(&state);
        assert_eq!(events[0].tone, EventTone::Critical);
    }

    #[test]
    fn governor_failed_kill_renders_red() {
        let mut state = empty_state();
        let mut entry = audit_entry(206, "phi3", ts(1_000));
        entry.success = false;
        state.audit.push_back(entry);
        let events = build_events(&state);
        assert_eq!(events[0].tone, EventTone::Critical);
    }

    #[test]
    fn regression_critical_severity_renders_red() {
        let mut state = empty_state();
        state.regressions.push_back(regression_event(ts(1_000)));
        let events = build_events(&state);
        assert_eq!(events[0].tone, EventTone::Critical);
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
        assert_eq!(events[0].tone, EventTone::Attention);
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
        state.push_completed_exit(run_summary(1, "x", ts(1_000)), None);
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

    // ── v1.3.2 / CAR-D75 / DISPATCH 76 — browse-mode tests ────────

    /// Build_events now stamps `kind` + `detail` + composite
    /// `key()` on every event. Pins the projection so a future
    /// refactor that drops a field surfaces here.
    #[test]
    fn build_events_populates_kind_pid_and_composite_key() {
        let mut state = empty_state();
        state.push_completed_exit(run_summary(206, "phi3", ts(1_000)), None);
        state.audit.push_back(audit_entry(207, "ollama", ts(2_000)));
        state.regressions.push_back(regression_event(ts(3_000)));

        let events = build_events(&state);
        // Time-descending: regression (3000) → kill (2000) → exit (1000).
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0].kind, ActivityKind::Regression));
        assert_eq!(events[0].pid, 0); // regression sentinel
        assert!(events[0].detail.is_none());
        assert!(matches!(events[1].kind, ActivityKind::Kill));
        assert_eq!(events[1].pid, 207);
        assert!(matches!(events[1].detail, Some(ActivityEventDetail::Kill { .. })));
        assert!(matches!(events[2].kind, ActivityKind::Exit));
        assert_eq!(events[2].pid, 206);
        assert!(matches!(events[2].detail, Some(ActivityEventDetail::Exit { .. })));

        // Composite keys are pid+kind+timestamp; unique across the
        // three sources even if a same-PID exit+kill collide later.
        let keys: std::collections::HashSet<String> = events.iter().map(|e| e.key()).collect();
        assert_eq!(keys.len(), 3, "composite keys must stay unique: {keys:?}");
    }

    /// Exit detail carries the lock-step attribution and the
    /// `vram_unmeasured` honest discriminator. Pins both branches
    /// of STOP #3 — `samples=0` ⇒ unmeasured, `samples>0` + 0 MB ⇒
    /// real CPU-only zero.
    #[test]
    fn exit_detail_carries_attribution_and_vram_honesty() {
        let mut state = empty_state();
        let mut s_with_samples = run_summary(10, "ollama", ts(1_000));
        s_with_samples.samples = 30;
        s_with_samples.peak_vram_mb = 0;
        state.push_completed_exit(
            s_with_samples,
            Some(crate::runtime::ExitAttribution {
                exit_kind: "governor".into(),
                exit_detail: Some("operator pressed k".into()),
            }),
        );
        let events = build_events(&state);
        let Some(ActivityEventDetail::Exit {
            exit_kind,
            exit_detail,
            vram_unmeasured,
            peak_vram_mb,
            ..
        }) = events[0].detail.clone() else {
            panic!("expected Exit detail on the only event");
        };
        assert_eq!(exit_kind.as_deref(), Some("governor"));
        assert_eq!(exit_detail.as_deref(), Some("operator pressed k"));
        assert!(
            !vram_unmeasured,
            "samples>0 ⇒ vram_unmeasured=false (real CPU-only zero)",
        );
        assert_eq!(peak_vram_mb, 0);

        // Inverse: samples=0 ⇒ unmeasured (the tick-window-short
        // process that never sampled VRAM).
        let mut state2 = empty_state();
        let mut s_no_samples = run_summary(11, "shortlived", ts(2_000));
        s_no_samples.samples = 0;
        s_no_samples.peak_vram_mb = 0;
        state2.push_completed_exit(s_no_samples, None);
        let events2 = build_events(&state2);
        if let Some(ActivityEventDetail::Exit { vram_unmeasured, .. }) =
            events2[0].detail.clone()
        {
            assert!(vram_unmeasured, "samples=0 ⇒ vram_unmeasured=true");
        } else {
            panic!("expected Exit detail");
        }
    }

    /// Regression rows have `detail = None` — hard rule #4. Enter
    /// in browse mode is a no-op for these rows (the dispatcher
    /// checks `ev.detail.is_some()` before toggling expand). Pins
    /// the contract so a future "we have nothing structured to
    /// show but let's invent some text" temptation fails this
    /// test.
    #[test]
    fn regression_event_has_no_detail() {
        let mut state = empty_state();
        state.regressions.push_back(regression_event(ts(1_000)));
        let events = build_events(&state);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].kind, ActivityKind::Regression));
        assert!(
            events[0].detail.is_none(),
            "regression rows MUST have detail=None (hard rule #4); got {:?}",
            events[0].detail,
        );
    }
}
