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
// L22 — `Alignment` is for the centred TERMINAL_TOO_SMALL paragraph.
// `Color` from wp5's pre-L21 status_bar/footer is intentionally
// dropped: L21 (l14) routes all colour through `theme: &UiTheme`,
// so the merged render_footer never references `Color::*` literals.
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::runtime::RuntimeState;
use crate::ui::SizeTier;
use crate::ui::panels::live_detail::{LiveDetailBuffers, LiveDetailCard};
use crate::ui::theme::UiTheme;

use super::app::App;

pub fn render(
    f: &mut Frame,
    state: &RuntimeState,
    app: &App,
    theme: &UiTheme,
    live_detail: Option<&LiveDetailCard>,
    live_buffers: Option<&LiveDetailBuffers>,
) {
    let full = f.area();
    let tier = SizeTier::classify(full.width, full.height);

    // L22 / §12 — below `MIN_COLS × MIN_ROWS` the contract forbids a
    // degraded render. Paint the `errors::TERMINAL_TOO_SMALL` message
    // and return; the banner / alerts / overlays would fight for the
    // few cells we have left.
    if tier == SizeTier::TooSmall {
        render_too_small(f, full);
        return;
    }

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
        armed_banner::render(f, banner_area, armed, state.dry_run, theme);
    }

    if alerts_height > 0 {
        alerts::render(f, alerts_area, app, state, theme);
    }

    // L2b removed the legacy "detail mode" toggle (`v` key + the
    // 6-panel layout that surfaced rogues/culprits/audit). v0.3 §1
    // defines a single layout, so the default render is the only
    // path now.
    render_default(f, body_area, state, app, theme, tier);

    if app.show_help() {
        help::render(f, body_area, theme);
    }

    // History overlay sits above panels but below the post-mortem
    // card (see below) — though the input layer prevents both from
    // being open simultaneously.
    history_overlay::render(f, body_area, app, theme);

    // L16 / §5 — detail card renders LAST so it floats above every
    // other panel. The two card kinds are mutually exclusive at the
    // dispatch level (`handle_open_detail` in `ui::mod.rs` picks one
    // based on whether the focused workload is running or exited);
    // when both happen to be set the live card wins because it was
    // necessarily opened after any pre-existing post-mortem — same
    // "latest wins" rule that governs same-kind card replacement.
    if let Some(card) = live_detail {
        live_detail::render(f, full, card, theme, live_buffers);
    } else if let Some(card) = app.postmortem() {
        postmortem::render(f, full, card, theme);
    }
}

/// L22 / §12 — minimum-viable terminal: paint the contract's
/// `errors::TERMINAL_TOO_SMALL` message centered, with the current
/// dimensions substituted in so the operator knows how far they need
/// to drag the resize handle.
fn render_too_small(f: &mut Frame, area: Rect) {
    let msg = ux_contract::errors::TERMINAL_TOO_SMALL
        .replace("{w}", &area.width.to_string())
        .replace("{h}", &area.height.to_string());
    let paragraph = Paragraph::new(msg)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

/// The §1 layout, parametrised by `tier`. `Narrow` drops the Top
/// processes panel (§12: "first to drop on narrow screens"); `Wide`
/// renders the workloads panel side-by-side when there are 4+
/// workloads. The rest of the §1 region map is identical across
/// non-TooSmall tiers. `theme` threads through to every panel
/// (L21 § 14 color usage).
///
/// **L22 deferral:** panel-internal sizing (bar graph cell counts —
/// 17/25/40, Activity 3-row cap, sparkline 30-cell extension in the
/// live detail card) is left to a follow-up row (likely absorbed by
/// L21's panel audit). This commit only owns the layout gate.
///
/// **L25 / L22 merge:** the merged region renders the §0 mission-line
/// header (L25) at layout[0] in every non-TooSmall tier. The prior
/// wp5 `render_status_bar` is dropped in favor of the contract-
/// aligned header (which carries the same operator-facing role).
fn render_default(
    f: &mut Frame,
    area: Rect,
    state: &RuntimeState,
    app: &App,
    theme: &UiTheme,
    tier: SizeTier,
) {
    let constraints: &[Constraint] = if tier == SizeTier::Narrow {
        // 1 + 7 + Min(8) + 7 + 1 = 24 fixed/min rows → fits §12's
        // 80×24 floor exactly with Min(8) absorbing the workload
        // panel. Header (L25 mission line) replaces the prior
        // status bar at layout[0].
        &[
            Constraint::Length(1), // §0 mission-line header (L25)
            Constraint::Length(7), // vitals (System)
            Constraint::Min(8),    // AI Workloads (flexes)
            Constraint::Length(7), // Activity (Top processes hidden)
            Constraint::Length(1), // hint footer
        ]
    } else {
        &[
            Constraint::Length(1), // §0 mission-line header (L25)
            Constraint::Length(7), // vitals (System)
            Constraint::Min(8),    // AI Workloads (flexes)
            Constraint::Length(7), // Top processes (L13)
            Constraint::Length(7), // Activity (L15)
            Constraint::Length(1), // hint footer
        ]
    };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
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
    vitals::render(f, layout[1], state, theme);

    // L22 / §12 Wide tier — split workloads into two columns when
    // there are 4+ workloads; single-column otherwise (Narrow /
    // Standard tiers always use single-column).
    let workload_count = state.ai_processes().count();
    if tier == SizeTier::Wide && workload_count >= 4 {
        render_workloads_two_col(f, layout[2], state, app, theme);
    } else {
        workloads::render(f, layout[2], state, app, theme);
    }

    if tier == SizeTier::Narrow {
        // §12: Top processes is "first to drop on narrow screens".
        activity::render(f, layout[3], state, theme);
        render_footer(f, layout[4], app, theme);
    } else {
        top_processes::render(f, layout[3], state, app.top_processes_sort(), theme);
        activity::render(f, layout[4], state, theme);
        render_footer(f, layout[5], app, theme);
    }
}

/// L22 / §12 Wide tier — split the workloads area into two columns
/// when there are 4+ workloads. Done by cloning `state` into two
/// narrow views, each retaining a contiguous half of the
/// AI-classified workloads (RuntimeState's doc-comment says it's
/// "cheap to clone for the UI to render between samples"; we pay
/// that cost only at Wide tier with 4+ workloads).
///
/// **Known limitation:** the selection highlight may render in both
/// halves at the same local index because `App::selected_index` is
/// a global pointer with no two-column awareness. Mapping the global
/// selection cleanly into one half is deferred to a follow-up — the
/// L22 commit owns the layout gate, not the input-mapping rewrite.
fn render_workloads_two_col(
    f: &mut Frame,
    area: Rect,
    state: &RuntimeState,
    app: &App,
    theme: &UiTheme,
) {
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // First-half / second-half partition of the AI subset only.
    // Non-AI entries in `annotated` are dropped from each clone for
    // free because `workloads::render` filters via
    // `state.ai_processes()` anyway, but explicitly retaining only
    // the AI PIDs in each half keeps the partition exact.
    let ai_pids: Vec<u32> = state.ai_processes().map(|p| p.pid).collect();
    let mid = ai_pids.len().div_ceil(2);
    let left_pids: std::collections::HashSet<u32> =
        ai_pids[..mid].iter().copied().collect();

    let mut left = state.clone();
    left.annotated.retain(|p| left_pids.contains(&p.pid));
    let mut right = state.clone();
    right.annotated.retain(|p| !left_pids.contains(&p.pid));

    workloads::render(f, halves[0], &left, app, theme);
    workloads::render(f, halves[1], &right, app, theme);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App, theme: &UiTheme) {
    // An ephemeral status message wins over the keybind hints — the
    // operator-feedback path is more valuable than the always-visible
    // cheat sheet for the few seconds the message is live.
    // L21 / §14 — transient status messages render in attention
    // color (they're surface-level operator feedback that should
    // catch the eye, but they're not workload-row content so the
    // "only status dots are colored on workload rows" rule doesn't
    // apply here).
    if let Some(msg) = app.status() {
        let p = Paragraph::new(format!(" {msg} ")).style(
            Style::default()
                .fg(theme.attention)
                .add_modifier(Modifier::BOLD),
        );
        f.render_widget(p, area);
        return;
    }

    // L21 / §14 — "Footer key hints: Accent for the key letter,
    // Muted for description." The hints string is `· `-separated
    // groups of `{key} {description}`; render each group as two
    // spans so the key letter picks up `theme.accent` and the rest
    // picks up `theme.muted`. The separator itself stays muted —
    // it's structural, not a key letter.
    //
    // L2b removed `v` and Tab from the keymap; L2c removed `/`.
    // The locked v0.3 footer keymap lands when CAR-5
    // (`status::FOOTER_KEYMAP`) ships — until then this stub matches
    // the active key set.
    let groups: [(&str, &str); 5] = [
        ("q", "quit"),
        ("j/k", "select"),
        ("k", "kill (×2)"),
        ("h", "history"),
        ("?", "help"),
    ];
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(groups.len() * 3 + 1);
    let muted = Style::default().fg(theme.muted);
    let accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    spans.push(Span::raw(" "));
    for (i, (key, desc)) in groups.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ".to_string(), muted));
        }
        spans.push(Span::styled((*key).to_string(), accent));
        spans.push(Span::styled(format!(" {desc}"), muted));
    }
    spans.push(Span::raw(" "));
    let p = Paragraph::new(Line::from(spans));
    f.render_widget(p, area);
}

/// Helper used by panels: bordered block with title.
///
/// L21 / §14 — section headers (System / Workloads / Top / Activity)
/// render in `theme.muted`; the focused panel switches to
/// `theme.accent` to keep the v1.0 "selected row tinted with accent"
/// rule consistent across panels. Borders match the title style so
/// the panel reads as a single unit.
pub(super) fn panel_block<'a>(title: &'a str, focused: bool, theme: &UiTheme) -> Block<'a> {
    let style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(format!(" {} ", title), style))
}
