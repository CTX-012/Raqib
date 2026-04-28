//! Render the full TUI frame. Panels are kept private because the layout
//! is shared state — callers shouldn't be able to render a panel into the
//! wrong region.

mod audit;
mod completed;
mod culprits;
mod help;
mod history_overlay;
mod registry;
mod rogues;
mod vitals;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::runtime::RuntimeState;

use super::app::App;

pub fn render(f: &mut Frame, state: &RuntimeState, app: &App) {
    let area = f.area();

    // Top: status bar (1 line). Bottom: panels.
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Length(7), // vitals
            Constraint::Min(8),    // process panels (3 columns)
            Constraint::Length(8), // completed
            Constraint::Length(8), // audit
            Constraint::Length(1), // hint footer
        ])
        .split(area);

    render_status_bar(f, layout[0], state, app);
    vitals::render(f, layout[1], state);
    render_process_row(f, layout[2], state, app);
    completed::render(f, layout[3], state);
    audit::render(f, layout[4], state);
    render_footer(f, layout[5], app);

    if app.show_help() {
        help::render(f, area);
    }

    // History overlay last so it floats above everything (including the
    // help panel — though the input layer prevents both from being open
    // simultaneously).
    history_overlay::render(f, area, app);
}

fn render_status_bar(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let mode_label = if state.dry_run { "DRY-RUN" } else { "ENFORCE" };
    let mode_color = if state.dry_run {
        Color::Yellow
    } else {
        Color::Red
    };

    let mut spans = vec![
        Span::styled(
            " edge_monitor ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!(" {} ", mode_label),
            Style::default()
                .bg(mode_color)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  tick #{} ", state.tick_count)),
        Span::raw(format!("focus: {} ", app.focus().label())),
    ];

    if app.mode() == super::app::Mode::Filter {
        spans.push(Span::styled(
            format!(" filter: {}_ ", app.filter()),
            Style::default().fg(Color::Cyan),
        ));
    } else if !app.filter().is_empty() {
        spans.push(Span::raw(format!("(filter: {}) ", app.filter())));
    }

    if let Some(pid) = app.armed_kill_pid() {
        spans.push(Span::styled(
            format!(" ARMED kill PID={} (press k to confirm) ", pid),
            Style::default()
                .bg(Color::Red)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

fn render_process_row(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
        ])
        .split(area);

    registry::render(f, cols[0], state, app);
    rogues::render(f, cols[1], state, app);
    culprits::render(f, cols[2], state, app);
}

fn render_footer(f: &mut Frame, area: Rect, _app: &App) {
    let hints = " q quit · Tab focus · j/k select · / filter · k kill (×2) · h history · d dry-run · ? help ";
    let p = Paragraph::new(hints).style(Style::default().fg(Color::DarkGray));
    f.render_widget(p, area);
}

/// Helper used by panels: bordered block with title.
pub(super) fn panel_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(format!(" {} ", title), style))
}
