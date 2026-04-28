use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{List, ListItem, ListState};

use crate::runtime::RuntimeState;

use super::super::app::{App, FocusedPanel};
use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let focused = app.focus() == FocusedPanel::Culprits;

    let pids: Vec<u32> = if focused {
        app.visible(state)
    } else {
        let mut pids: Vec<u32> = state.annotated.iter().map(|p| p.pid).collect();
        pids.sort();
        pids.into_iter().take(20).collect()
    };

    let items: Vec<ListItem> = pids
        .iter()
        .filter_map(|pid| state.annotated.iter().find(|p| p.pid == *pid))
        .map(|p| {
            let cat_marker = if p.category != crate::model::AICategory::NotAi {
                "*"
            } else {
                " "
            };
            ListItem::new(format!("{} {:>6} {}", cat_marker, p.pid, p.name))
                .style(Style::default().fg(Color::White))
        })
        .collect();

    let block = panel_block("Culprits (top by PID order)", focused);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Red)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    if focused && !pids.is_empty() {
        list_state.select(Some(app.selected_index().min(pids.len() - 1)));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}
