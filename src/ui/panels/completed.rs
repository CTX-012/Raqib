use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{List, ListItem};

use crate::runtime::RuntimeState;

use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState) {
    let block = panel_block("AI run summaries", false);

    // Mirrors the headless exit-log filter: operators only want to see AI
    // workloads here. Non-AI exits still hit the persistent JSONL log.
    let items: Vec<ListItem> = state
        .completed
        .iter()
        .rev()
        .filter(|s| s.category.is_some())
        .take(20)
        .map(|s| {
            let cat = s
                .category
                .map(|c| format!("{:?}", c))
                .unwrap_or_else(|| "-".into());
            let model = s.model_name.as_deref().unwrap_or("-");
            // Signal-terminated runs stand out in red; clean exits in green.
            // Decided up front so we apply a single style to the row instead
            // of stacking calls (which silently overwrites earlier ones).
            let killed_by_signal = s.signal.is_some();
            let row = format!(
                "{} pid={} {} {} model={} cpu avg={:.0}% peak={:.0}% rss={}M vram={}M up={}s",
                s.exit_time.format("%H:%M:%S"),
                s.pid,
                s.name,
                cat,
                model,
                s.avg_cpu_pct,
                s.peak_cpu_pct,
                s.peak_rss_mb,
                s.peak_vram_mb,
                s.uptime_secs,
            );
            let color = if killed_by_signal {
                Color::Red
            } else {
                Color::Green
            };
            ListItem::new(row).style(Style::default().fg(color))
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}
