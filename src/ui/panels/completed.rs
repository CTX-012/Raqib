use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{List, ListItem};

use crate::runtime::RuntimeState;

use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState) {
    let block = panel_block("Recent runs", false);

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
                .unwrap_or_default();
            // Signal-terminated runs stand out in red; clean exits in green.
            // Decided up front so we apply a single style to the row instead
            // of stacking calls (which silently overwrites earlier ones).
            let killed_by_signal = s.signal.is_some();
            // Build the row from optional segments — `model=…` and the GPU
            // memory clause are dropped when the underlying value is unset
            // or zero, so the operator never sees a confusing "model=-"
            // or "GPU memory 0 MB" placeholder for runs we couldn't
            // observe.
            let mut row = format!(
                "{} pid={} {} {}",
                s.exit_time.format("%H:%M:%S"),
                s.pid,
                s.name,
                cat,
            );
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
