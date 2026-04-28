//! Tier 1.1 — TUI history overlay.
//!
//! Renders a centered floating panel listing the most recent runs of
//! the focused row's model. Records are snapshotted on `h` keypress
//! and drawn from `App::history()` so the render path stays pure.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::history::format_exit_short;

use super::super::app::{App, HistoryOverlay};

/// Render the overlay (panel + dim background outside it). No-op when
/// the app reports no overlay open — the caller invokes us
/// unconditionally at the end of the frame.
pub fn render(f: &mut Frame, full: Rect, app: &App) {
    let Some(overlay) = app.history() else {
        return;
    };
    let area = centered(full, 80, 70);

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" History: {} ", overlay.model),
            Style::default()
                .fg(Color::Cyan)
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

    f.render_widget(header_paragraph(overlay), layout[0]);
    f.render_widget(body_list(overlay), layout[1]);
    f.render_widget(footer_paragraph(), layout[2]);
}

fn header_paragraph(overlay: &HistoryOverlay) -> Paragraph<'_> {
    let line = if overlay.records.is_empty() {
        Line::from(Span::styled(
            "no runs found — has this model exited at least once with persistence enabled?",
            Style::default().fg(Color::Yellow),
        ))
    } else {
        Line::from(vec![
            Span::raw(format!(" {} runs · columns: ", overlay.records.len())),
            Span::styled(
                "# When  Dur  AvgCPU  PeakRSS  PeakVRAM  Exit",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };
    Paragraph::new(line)
}

fn body_list(overlay: &HistoryOverlay) -> List<'_> {
    let items: Vec<ListItem<'_>> = overlay
        .records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let idx = overlay.records.len() - i;
            let exit = format_exit_short(&r.exit_reason);
            let color = match exit.as_str() {
                "clean" => Color::Green,
                "governor" => Color::Yellow,
                _ => Color::Red,
            };
            ListItem::new(format!(
                " {:>3}  {}  {:>4}s  {:>5.0}%  {:>5}MB  {:>6}MB  {}",
                idx,
                r.summary.exit_time.format("%m-%d %H:%M"),
                r.summary.uptime_secs,
                r.summary.avg_cpu_pct,
                r.summary.peak_rss_mb,
                r.summary.peak_vram_mb,
                exit,
            ))
            .style(Style::default().fg(color))
        })
        .collect();
    List::new(items).block(Block::default())
}

fn footer_paragraph() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        " Esc / q close ",
        Style::default().fg(Color::DarkGray),
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
