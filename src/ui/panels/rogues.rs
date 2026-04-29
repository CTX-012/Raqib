use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{List, ListItem, ListState};

use crate::model::AICategory;
use crate::runtime::RuntimeState;

use super::super::app::{App, FocusedPanel};
use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let focused = app.focus() == FocusedPanel::Rogues;

    let pids: Vec<u32> = if focused {
        app.visible(state)
    } else {
        state
            .annotated
            .iter()
            .filter(|p| p.category == AICategory::Framework)
            .map(|p| p.pid)
            .collect()
    };

    let items: Vec<ListItem> = pids
        .iter()
        .filter_map(|pid| state.annotated.iter().find(|p| p.pid == *pid))
        .map(|p| {
            ListItem::new(format!("{:>6} {}", p.pid, p.name))
                .style(Style::default().fg(Color::Yellow))
        })
        .collect();

    let block = panel_block("Framework procs", focused);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    if focused && !pids.is_empty() {
        list_state.select(Some(app.selected_index().min(pids.len() - 1)));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}
