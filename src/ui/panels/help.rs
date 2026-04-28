use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Modal help overlay. Centered, dimmed background. Toggled with `?`.
pub fn render(f: &mut Frame, area: Rect) {
    let popup = centered(60, 60, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let lines = vec![
        Line::from("Navigation"),
        Line::from("  Tab / Shift-Tab     cycle panel focus"),
        Line::from("  j / Down            move cursor down"),
        Line::from("  k / Up              move cursor up"),
        Line::from(""),
        Line::from("Actions"),
        Line::from("  /                   start filter (Esc cancel · Enter commit)"),
        Line::from("  d                   toggle dry-run / enforce mode"),
        Line::from("  k                   ARM manual kill on selected PID"),
        Line::from("  k (again)           CONFIRM kill on armed PID"),
        Line::from("  q / Ctrl-C          quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Manual kill is a two-step: arm then confirm.",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            "In dry-run, kills are logged only. No signals sent.",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from("Press ? to close this help."),
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
