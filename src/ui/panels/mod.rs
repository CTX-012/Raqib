//! Render the full TUI frame. Panels are kept private because the layout
//! is shared state — callers shouldn't be able to render a panel into the
//! wrong region.

pub mod armed_banner;
mod audit;
mod completed;
mod culprits;
mod help;
mod history_overlay;
pub mod postmortem;
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
    let full = f.area();

    // [UX-1] — reserve the top row for the armed-kill banner ONLY when
    // a kill is armed. Allocating an empty row otherwise would leave
    // a stale red strip on screen between arms.
    let banner_height = if app.armed_kill().is_some() { 1 } else { 0 };
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(banner_height), Constraint::Min(0)])
        .split(full);
    let (banner_area, body_area) = (split[0], split[1]);

    if let Some(armed) = app.armed_kill() {
        armed_banner::render(f, banner_area, armed);
    }

    if app.detail_mode() {
        render_detail(f, body_area, state, app);
    } else {
        render_default(f, body_area, state, app);
    }

    if app.show_help() {
        help::render(f, body_area);
    }

    // History overlay sits above panels but below the post-mortem
    // card (see below) — though the input layer prevents both from
    // being open simultaneously.
    history_overlay::render(f, body_area, app);

    // [UX-2] — post-mortem card renders LAST so it floats above
    // everything else, including the help / history overlays.
    if let Some(card) = app.postmortem() {
        postmortem::render(f, full, card);
    }
}

/// Default view — what the operator sees on launch. Drops the
/// secondary panels (Framework procs, All processes, Recent actions)
/// so the screen is just the bits that answer "what's running and how
/// did the last few runs go". Hit `v` to bring the others back.
fn render_default(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Length(7), // vitals
            Constraint::Min(8),    // AI Workloads (full width)
            Constraint::Length(8), // recent runs
            Constraint::Length(1), // hint footer
        ])
        .split(area);

    render_status_bar(f, layout[0], state, app);
    vitals::render(f, layout[1], state);
    registry::render(f, layout[2], state, app);
    completed::render(f, layout[3], state);
    render_footer(f, layout[4], app);
}

/// Detail view — the legacy six-panel layout, behind a `v` toggle.
/// Kept for operators who actually want to see framework procs by PID
/// or scroll through the audit trail without leaving the TUI.
fn render_detail(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Length(7), // vitals
            Constraint::Min(8),    // process panels (3 columns)
            Constraint::Length(8), // recent runs
            Constraint::Length(8), // recent actions
            Constraint::Length(1), // hint footer
        ])
        .split(area);

    render_status_bar(f, layout[0], state, app);
    vitals::render(f, layout[1], state);
    render_process_row(f, layout[2], state, app);
    completed::render(f, layout[3], state);
    audit::render(f, layout[4], state);
    render_footer(f, layout[5], app);
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

    // The full-row armed-kill banner ([UX-1]) supersedes the old
    // inline status-bar marker. Leaving them both would double-render
    // the same state and confuse the operator about which one to
    // watch.

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

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    // Detail mode unlocks Tab; default mode locks focus to AI Workloads.
    // The footer reflects what the operator can actually do right now —
    // listing keys that no-op in default mode would be misleading.
    let hints = if app.detail_mode() {
        " q quit · v hide details · Tab focus · j/k select · / filter · k kill (×2) · h history · d dry-run · ? help "
    } else {
        " q quit · v show details · j/k select · / filter · k kill (×2) · h history · d dry-run · ? help "
    };
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
