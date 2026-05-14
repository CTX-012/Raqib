use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::theme::UiTheme;

/// Modal help overlay. Centered, dimmed background. Toggled with `?`.
///
/// L21 / §14 — overlay title bar uses `theme.accent`; warnings inside
/// the overlay (manual-kill caveat, dry-run reminder) use
/// `theme.attention` so they read as advisories but track the active
/// palette. Body text is plain foreground.
pub fn render(f: &mut Frame, area: Rect, theme: &UiTheme) {
    let popup = centered(60, 60, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));

    let body = Style::default().fg(theme.foreground);
    let lines = vec![
        Line::styled("Navigation", body),
        Line::styled("  Tab / Shift-Tab     cycle panel focus", body),
        Line::styled("  j / Down            move cursor down", body),
        Line::styled("  k / Up              move cursor up", body),
        Line::from(""),
        Line::styled("Actions", body),
        Line::styled("  /                   start filter (Esc cancel · Enter commit)", body),
        Line::styled("  d                   toggle dry-run / enforce mode", body),
        Line::styled("  k                   ARM manual kill on selected PID", body),
        Line::styled("  k (again)           CONFIRM kill on armed PID", body),
        Line::styled("  h                   show run history for selected model", body),
        Line::styled("  q / Ctrl-C          quit", body),
        Line::from(""),
        Line::from(Span::styled(
            "Manual kill is a two-step: arm then confirm.",
            Style::default().fg(theme.attention),
        )),
        Line::from(Span::styled(
            "In dry-run, kills are logged only. No signals sent.",
            Style::default().fg(theme.attention),
        )),
        Line::from(""),
        Line::styled("Press ? to close this help.", body),
    ];

    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);
}

fn centered(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v)[1]
}
