//! Render the full TUI frame. Panels are kept private because the layout
//! is shared state — callers shouldn't be able to render a panel into the
//! wrong region.

mod activity;
pub mod alerts;
pub mod armed_banner;
mod help;
mod history_overlay;
pub mod postmortem;
mod vitals;
pub mod workloads;

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
    // L6 / §1 region 1 — alert region sits between the armed-kill
    // banner and the System panel. Height shrinks to 0 when no
    // alerts are active so the workload area takes the full body.
    let alerts_height = alerts::region_height(app);
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_height),
            Constraint::Length(alerts_height),
            Constraint::Min(0),
        ])
        .split(full);
    let (banner_area, alerts_area, body_area) = (split[0], split[1], split[2]);

    if let Some(armed) = app.armed_kill() {
        armed_banner::render(f, banner_area, armed, state.dry_run);
    }

    if alerts_height > 0 {
        alerts::render(f, alerts_area, app, state);
    }

    // L2b removed the legacy "detail mode" toggle (`v` key + the
    // 6-panel layout that surfaced rogues/culprits/audit). v0.3 §1
    // defines a single layout, so the default render is the only
    // path now.
    render_default(f, body_area, state, app);

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

/// The default (and only) v1.0 layout. Status bar + vitals + AI
/// Workloads + recent runs + footer. L11/L13/L15/L25 reshape the
/// individual panels, but the top-level structure is locked here.
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
    workloads::render(f, layout[2], state, app);
    activity::render(f, layout[3], state);
    render_footer(f, layout[4], app);
}

fn render_status_bar(f: &mut Frame, area: Rect, state: &RuntimeState, _app: &App) {
    let mode_label = if state.dry_run { "DRY-RUN" } else { "ENFORCE" };
    let mode_color = if state.dry_run {
        Color::Yellow
    } else {
        Color::Red
    };

    let spans = vec![
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
    ];

    // L2c removed the filter-mode prompt and the post-commit
    // `(filter: ...)` indicator from the status bar — the `/` key is
    // unbound and the App no longer carries a filter buffer.

    // The full-row armed-kill banner ([UX-1]) supersedes the old
    // inline status-bar marker. Leaving them both would double-render
    // the same state and confuse the operator about which one to
    // watch.

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    // An ephemeral status message wins over the keybind hints — the
    // operator-feedback path is more valuable than the always-visible
    // cheat sheet for the few seconds the message is live. Yellow
    // matches the DRY-RUN label colour in the status bar so the two
    // dry-run cues read as the same channel.
    if let Some(msg) = app.status() {
        let p = Paragraph::new(format!(" {msg} ")).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        f.render_widget(p, area);
        return;
    }

    // L2b removed `v` and Tab from the keymap; L2c will remove `/`.
    // The string still mentions stale bindings — the locked v0.3
    // footer ("Enter detail · k kill · g graph · h history · ? help
    // · q quit") lands in L25 alongside the contract const
    // `status::FOOTER_KEYMAP` (CAR-5). Until then this stub keeps the
    // pre-L2b text minus `v hide/show details` and Tab focus, since
    // those bindings genuinely no longer work.
    let hints = " q quit · j/k select · k kill (×2) · h history · ? help ";
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
