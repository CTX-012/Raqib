//! Tier 1.1 — TUI history overlay.
//!
//! Renders a centered floating panel listing the most recent runs of
//! the focused row's model. Records are snapshotted on `h` keypress
//! and drawn from `App::history()` so the render path stays pure.
//!
//! L21 / §14 — overlay borders + title use `theme.accent`; column
//! header in `theme.muted`; per-row exit indicator uses the semantic
//! palette (`theme.healthy` for clean, `theme.attention` for
//! governor, `theme.critical` for other failures). KV-saturation
//! badge is always critical regardless of exit kind.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::history::format_exit_short;
use crate::ui::theme::UiTheme;

use super::super::app::{App, HistoryOverlay};

/// Tier 3.3 — peak KV-cache occupancy at or above this is flagged as
/// a saturation event in the history view. Slightly under 100 to
/// absorb float roundoff from `gpu_cache_usage_perc * 100` in the
/// vLLM sampler.
const KV_SATURATION_PCT: f32 = 99.5;

/// Render the overlay (panel + dim background outside it). No-op when
/// the app reports no overlay open — the caller invokes us
/// unconditionally at the end of the frame.
pub fn render(f: &mut Frame, full: Rect, app: &App, theme: &UiTheme) {
    let Some(overlay) = app.history() else {
        return;
    };
    let area = centered(full, 80, 70);

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" History: {} ", overlay.model),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(1),    // rows
            Constraint::Length(1), // footer hint
        ])
        .split(inner);

    f.render_widget(header_paragraph(overlay, theme), layout[0]);
    f.render_widget(body_list(overlay, theme), layout[1]);
    f.render_widget(footer_paragraph(theme), layout[2]);
}

fn header_paragraph<'a>(overlay: &'a HistoryOverlay, theme: &UiTheme) -> Paragraph<'a> {
    let line = if overlay.records.is_empty() {
        // L1 / UX_CONTRACT.md §7 — empty-state copy is locked in
        // `ux_contract::empty::HISTORY`. The previous, more diagnostic
        // wording ("has this model exited at least once with
        // persistence enabled?") is preserved as a comment for the
        // contract-amendment record; if the diagnostic value is worth
        // re-introducing, file a CAR against v0.3 — do not inline a
        // replacement string here.
        Line::from(Span::styled(
            ux_contract::empty::HISTORY,
            Style::default().fg(theme.attention),
        ))
    } else {
        // Sprint-4 B14 — per-run metric detail (Avg CPU, Peak RSS,
        // Peak VRAM) moves into the post-mortem card body. The history
        // overlay shrinks to a chronological list with just When,
        // Duration, and Exit reason; the operator opens a per-run
        // card via Enter on the focused workload row to see metrics.
        // Dropping the per-row metric columns frees ~30 cols of width
        // for the timestamp + exit-reason which used to be cramped.
        Line::from(vec![
            Span::styled(
                format!(" {} runs · columns: ", overlay.records.len()),
                Style::default().fg(theme.foreground),
            ),
            Span::styled(
                "# When  Dur  Exit",
                Style::default().fg(theme.muted),
            ),
        ])
    };
    Paragraph::new(line)
}

fn body_list<'a>(overlay: &'a HistoryOverlay, theme: &UiTheme) -> List<'a> {
    let items: Vec<ListItem<'_>> = overlay
        .records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let idx = overlay.records.len() - i;
            let exit = format_exit_short(&r.exit_reason);
            let color: Color = match exit.as_str() {
                "clean" => theme.healthy,
                "governor" => theme.attention,
                _ => theme.critical,
            };
            // B14 — metric columns dropped; per-run detail now lives
            // in the post-mortem card. Keeps history rows scannable
            // at a glance (timestamp + duration + exit kind) without
            // the column-density that made each row hard to parse.
            let row = format!(
                " {:>3}  {}  {:>4}s  {}",
                idx,
                r.summary.exit_time.format("%m-%d %H:%M"),
                r.summary.uptime_secs,
                exit,
            );
            let mut spans: Vec<Span<'_>> = vec![Span::styled(row, Style::default().fg(color))];

            // Tier 3.3 — saturation badge. Independent of exit colour
            // so a clean-exit run that maxed KV still gets flagged.
            // Kept on the history row even after the B14 column drop
            // because it's a one-off marker (not a column), and
            // surfaces KV-pressure exits without needing the operator
            // to open every card.
            if let Some(peak) = r.metrics.kv_cache_peak_pct
                && peak >= KV_SATURATION_PCT
            {
                spans.push(Span::styled(
                    "  KV!",
                    Style::default()
                        .fg(theme.critical)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();
    List::new(items).block(Block::default())
}

fn footer_paragraph(theme: &UiTheme) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        " Esc / q close ",
        Style::default().fg(theme.muted),
    )))
}

/// Centered rect with the given percentage size.
fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1]);
    h[1]
}
