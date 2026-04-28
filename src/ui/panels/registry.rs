use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{List, ListItem, ListState};

use crate::runtime::RuntimeState;

use super::super::app::{App, FocusedPanel};
use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let focused = app.focus() == FocusedPanel::Registry;
    let visible_pids = if focused {
        app.visible(state)
    } else {
        // When unfocused, still list AI procs so the user sees them; just no cursor.
        state.ai_processes().map(|p| p.pid).collect::<Vec<_>>()
    };

    let items: Vec<ListItem> = visible_pids
        .iter()
        .filter_map(|pid| state.annotated.iter().find(|p| p.pid == *pid))
        .map(|p| {
            // Prefer the extracted model name when present; fall back to "-"
            // so rogue-style matches (keyword / script-sniff) still render in
            // the same column layout.
            let model = p.model_name.as_deref().unwrap_or("-");
            let vram = p
                .vram_bytes
                .map(|b| format!("{:>4}M", b / (1024 * 1024)))
                .unwrap_or_else(|| "   -".into());
            let label = format!(
                "{:>6} {:<9?} {:>5.1}% {:>5}M {} {:<18} {}",
                p.pid,
                p.category,
                p.cpu_pct,
                p.rss_mb,
                vram,
                truncate(&p.name, 18),
                model,
            );
            ListItem::new(label).style(Style::default().fg(Color::Cyan))
        })
        .collect();

    let block = panel_block("Registry (AI workloads)", focused);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    if focused && !visible_pids.is_empty() {
        list_state.select(Some(app.selected_index().min(visible_pids.len() - 1)));
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
