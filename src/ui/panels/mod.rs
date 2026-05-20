//! Render the full TUI frame. Panels are kept private because the layout
//! is shared state — callers shouldn't be able to render a panel into the
//! wrong region.

mod activity;
pub mod alerts;
pub mod header;
mod help;
mod history_overlay;
pub mod kill_confirm;
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

    // CAR-17 — the kill_confirm card replaces the top-of-screen ARMED
    // banner pattern. The card renders as a centered overlay below
    // (alongside live_detail / post_mortem), not as a row at the top
    // of the frame, so the layout no longer reserves a banner row.
    let alerts_height = alerts::region_height(app);
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(alerts_height), Constraint::Min(0)])
        .split(full);
    let (alerts_area, body_area) = (split[0], split[1]);

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

    // CAR-17 / §5 — detail-card layer floats above every other panel.
    // Three cards, one z-slot. Priority (highest first):
    //   1. kill_confirm — the destructive prompt sits above everything
    //      else; an Enter on the focused row must reach the confirm
    //      handler, not a stale live-detail dismiss.
    //   2. live_detail — the running-workload card.
    //   3. post_mortem — the retrospective card.
    // The dispatcher in `ui::mod.rs` enforces the same priority so
    // the input layer and the render layer agree on which card is
    // "in focus."
    if let Some(card) = app.kill_confirm() {
        kill_confirm::render(f, full, card, theme);
    } else if let Some(card) = live_detail {
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
    // Sprint-4 FIX 1 — extend Bundle-3's B7 spacer pattern to every
    // panel adjacency, not just Vitals↔Workloads. The user-reported
    // "Vitals merges with adjacent panel when a card opens" symptom
    // didn't reproduce against Vitals (the original B7 spacer
    // survives every card overlay — see the Sprint-4 examples repro).
    // What DID stack visually was Workloads↔Top and Top↔Activity in
    // Standard/Wide, and Workloads↔Activity in Narrow — those pairs
    // had no spacer at all, and a card narrowing the visible side
    // slivers made the lack of gap far more obvious.
    //
    // The cost is 2 spacer rows in Standard/Wide (Min for Workloads
    // drops 8 → 6) and 1 spacer row in Narrow (Min drops 7 → 6).
    // At §12's 80×24 floor the layout fits exactly; at 120×40 the
    // workloads area still flexes to ~14 rows of content.
    let constraints: &[Constraint] = if tier == SizeTier::Narrow {
        // 1+7+1+Min(6)+1+7+1 = 24 fits §12's 80×24 floor.
        &[
            Constraint::Length(1), // [0] §0 mission-line header (L25)
            Constraint::Length(7), // [1] vitals (System)
            Constraint::Length(1), // [2] B7 spacer (vitals → workloads)
            Constraint::Min(6),    // [3] AI Workloads (flexes)
            Constraint::Length(1), // [4] FIX-1 spacer (workloads → activity)
            Constraint::Length(7), // [5] Activity (Top processes hidden)
            Constraint::Length(1), // [6] hint footer
        ]
    } else {
        // 1+7+1+Min(6)+1+7+1+7+1 = 26+Min. At 40 rows Min=14.
        &[
            Constraint::Length(1), // [0] §0 mission-line header (L25)
            Constraint::Length(7), // [1] vitals (System)
            Constraint::Length(1), // [2] B7 spacer (vitals → workloads)
            Constraint::Min(6),    // [3] AI Workloads (flexes)
            Constraint::Length(1), // [4] FIX-1 spacer (workloads → top)
            Constraint::Length(7), // [5] Top processes (L13)
            Constraint::Length(1), // [6] FIX-1 spacer (top → activity)
            Constraint::Length(7), // [7] Activity (L15)
            Constraint::Length(1), // [8] hint footer
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
    // layout[2] is the B7 spacer — intentionally unrendered.

    // L22 / §12 Wide tier — split workloads into two columns when
    // there are 4+ workloads; single-column otherwise (Narrow /
    // Standard tiers always use single-column).
    let workload_count = state.ai_processes().count();
    if tier == SizeTier::Wide && workload_count >= 4 {
        render_workloads_two_col(f, layout[3], state, app, theme);
    } else {
        workloads::render(f, layout[3], state, app, theme);
    }
    // layout[4] is the FIX-1 spacer (workloads → next) — unrendered.

    if tier == SizeTier::Narrow {
        // §12: Top processes is "first to drop on narrow screens".
        activity::render(f, layout[5], state, theme);
        render_footer(f, layout[6], app, theme);
    } else {
        top_processes::render(f, layout[5], state, app.top_processes_sort(), theme);
        // layout[6] is the FIX-1 spacer (top → activity) — unrendered.
        activity::render(f, layout[7], state, theme);
        render_footer(f, layout[8], app, theme);
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
        ("k", "kill (confirm)"),
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
