use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{List, ListItem};

use crate::governor::manual::{KillSource, ManualKillAction};
use crate::runtime::RuntimeState;

use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState) {
    let block = panel_block("Audit (governor decisions)", false);

    let items: Vec<ListItem> = state
        .audit
        .iter()
        .rev()
        .take(20)
        .map(|e| {
            let action = match e.action {
                ManualKillAction::SendSigterm => "SIGTERM",
                ManualKillAction::SendSigkill => "SIGKILL",
                ManualKillAction::Cancelled => "CANCELLED",
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
            ListItem::new(format!(
                "{} {} {} {} pid={} {} - {}",
                e.timestamp.format("%H:%M:%S"),
                action,
                status,
                source,
                e.pid,
                e.process_name,
                e.reason
            ))
            .style(Style::default().fg(color))
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}
