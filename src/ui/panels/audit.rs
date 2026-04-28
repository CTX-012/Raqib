use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{List, ListItem};

use crate::analysis::Severity;
use crate::governor::manual::{KillSource, ManualKillAction};
use crate::runtime::RuntimeState;

use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState) {
    let block = panel_block("Audit (kills + regressions)", false);

    // Interleave kill entries and regression events by timestamp.
    // Tier 1.3 — `state.regressions` is the rolling buffer fed by the
    // runtime when an exit's metrics deviate from the rolling baseline.
    let mut items: Vec<(chrono::DateTime<chrono::Utc>, ListItem)> = Vec::new();

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
        let color = if !e.success {
            Color::Red
        } else if e.source == KillSource::Manual {
            Color::Yellow
        } else {
            Color::Green
        };
        let item = ListItem::new(format!(
            "{} {} {} {} pid={} {} - {}",
            e.timestamp.format("%H:%M:%S"),
            action,
            status,
            source,
            e.pid,
            e.process_name,
            e.reason
        ))
        .style(Style::default().fg(color));
        items.push((e.timestamp, item));
    }

    for r in &state.regressions {
        let color = if r.regression.severity >= Severity::Critical {
            Color::Red
        } else {
            Color::Yellow
        };
        let item = ListItem::new(format!(
            "{} REGRESSION {:?} {} {} {:+.1}% (n={})",
            r.timestamp.format("%H:%M:%S"),
            r.regression.severity,
            r.model,
            r.regression.metric,
            r.regression.delta_pct,
            r.baseline_size,
        ))
        .style(Style::default().fg(color));
        items.push((r.timestamp, item));
    }

    items.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let rendered: Vec<ListItem> = items.into_iter().take(20).map(|(_, i)| i).collect();

    let list = List::new(rendered).block(block);
    f.render_widget(list, area);
}
