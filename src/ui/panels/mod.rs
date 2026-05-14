//! Render the full TUI frame. Panels are kept private because the layout
//! is shared state — callers shouldn't be able to render a panel into the
//! wrong region.

mod activity;
pub mod alerts;
pub mod armed_banner;
pub mod header;
mod help;
mod history_overlay;
pub mod live_detail;
pub mod postmortem;
mod top_processes;
mod vitals;
pub mod workloads;

// L14 — re-exported so `ui::app::App` can hold the current sort
// without panels.rs leaking its private module structure. The
// sort fns + per-sort title live in `top_processes`; the enum is
// the only public surface.
pub use top_processes::TopProcessesSort;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::runtime::RuntimeState;
use crate::ui::panels::live_detail::LiveDetailCard;
use crate::ui::theme::UiTheme;

use super::app::App;

pub fn render(
    f: &mut Frame,
    state: &RuntimeState,
    app: &App,
    theme: &UiTheme,
    live_detail: Option<&LiveDetailCard>,
) {
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
    render_default(f, body_area, state, app, theme);

    if app.show_help() {
        help::render(f, body_area);
    }

    // History overlay sits above panels but below the post-mortem
    // card (see below) — though the input layer prevents both from
    // being open simultaneously.
    history_overlay::render(f, body_area, app);

    // L16 / §5 — detail card renders LAST so it floats above every
    // other panel. The two card kinds are mutually exclusive at the
    // dispatch level (`handle_open_detail` in `ui::mod.rs` picks one
    // based on whether the focused workload is running or exited);
    // when both happen to be set the live card wins because it was
    // necessarily opened after any pre-existing post-mortem — same
    // "latest wins" rule that governs same-kind card replacement.
    if let Some(card) = live_detail {
        live_detail::render(f, full, card, theme);
    } else if let Some(card) = app.postmortem() {
        postmortem::render(f, full, card);
    }
}

/// The default (and only) v1.0 layout. Per UX_CONTRACT.md §1
/// region map: header (§0 mission line) + System (vitals) + AI
/// Workloads + Top processes + Activity + footer. §12 (sizing
/// breakpoints) will hide Top processes in narrow mode — that's
/// L22's row, not this one.
fn render_default(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App, theme: &UiTheme) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // §0 mission-line header (L25)
            Constraint::Length(7), // vitals (System)
            Constraint::Min(8),    // AI Workloads (flexes)
            Constraint::Length(7), // Top processes (L13)
            Constraint::Length(7), // Activity (L15, was 8 — yielded 1 row to Top processes)
            Constraint::Length(1), // hint footer
        ])
        .split(area);

    // L25 / §0 — derive workload + degraded counts from the same row
    // builder the Workloads panel renders from. Keeping one source of
    // truth for `WorkloadStatus` per workload means the header count
    // can never drift from what the operator sees one panel down.
    let rows = workloads::ordered_rows(state, app);
    let n_workloads = rows.len();
    let n_degraded = rows
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                ux_contract::WorkloadStatus::Attention | ux_contract::WorkloadStatus::Critical
            )
        })
        .count();

    header::render(f, layout[0], app, theme, n_workloads, n_degraded);
    vitals::render(f, layout[1], state);
    workloads::render(f, layout[2], state, app);
    top_processes::render(f, layout[3], state, app.top_processes_sort());
    activity::render(f, layout[4], state);
    render_footer(f, layout[5], app);
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
