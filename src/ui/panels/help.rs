use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::theme::UiTheme;

/// Modal help overlay. Centered, dimmed background. Toggled with `?`.
///
/// L21 / §14 — overlay title bar uses `theme.accent`; the manual-kill
/// confirmation advisory uses `theme.attention` so it reads as a
/// non-numeric caution but tracks the active palette. Body text is
/// plain foreground.
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
        Line::styled("  j / Down            move cursor down", body),
        Line::styled("  K / Up              move cursor up", body),
        Line::from(""),
        Line::styled("Actions", body),
        Line::styled("  k                   open Kill Confirmation card on selected PID", body),
        Line::styled("  Enter               confirm kill (kill_confirm) · open detail · dismiss", body),
        Line::styled("  Esc                 cancel kill_confirm · dismiss card · ack alerts · quit", body),
        Line::styled("  h                   show run history for selected model", body),
        Line::styled("  a                   acknowledge all visible alerts", body),
        Line::styled("  t                   cycle Top processes sort (RAM → CPU → VRAM)", body),
        Line::styled("  q / Ctrl-C          quit", body),
        Line::from(""),
        Line::from(Span::styled(
            "Manual kill is real on confirm. Esc cancels without firing.",
            Style::default().fg(theme.attention),
        )),
        Line::from(""),
        Line::styled("Limitations", body),
        Line::styled(
            "  Ollama tokens/sec: requires `edge_monitor exec -- ollama …`",
            body,
        ),
        Line::styled(
            "  Passive ollama monitoring cannot read tokens/sec",
            body,
        ),
        Line::styled(
            "  (Ollama embeds metrics in per-request JSON, no Prom endpoint).",
            body,
        ),
        Line::from(""),
        // v1.0.1 B-NEW-1 / Inspector #1 — operators were surprised
        // that no governor action ever fired even after telemetry
        // flagged a workload. The default is now Allow; opting back
        // in to kills is an explicit config change, not a silent
        // built-in. Surface that here so the next operator who reads
        // `?` knows where the safety surface lives.
        Line::from(Span::styled(
            "Automated governor: DEFAULT DISABLED (v1.0.1).",
            Style::default().fg(theme.attention),
        )),
        Line::styled(
            "  Enable via edge_monitor.toml:",
            body,
        ),
        Line::styled(
            "    [governor.policy]  default_ai_action = \"Kill\"",
            body,
        ),
        Line::styled(
            "  Manual kill (the `k` keybinding) still works regardless.",
            body,
        ),
        Line::from(""),
        // Sprint-7 Item 4 — surface the no-auth posture on the help
        // overlay so the operator who just SSH'd in to a shared box
        // can see why the dashboard is reachable from their laptop.
        Line::from(Span::styled(
            "Web UI: 0.0.0.0:7070 by default · NO AUTH · trusted LAN only",
            Style::default().fg(theme.attention),
        )),
        Line::styled(
            "  Restrict with --bind 127.0.0.1 on untrusted networks.",
            body,
        ),
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
