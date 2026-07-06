//! v1.3.2 / CAR-D97 / DISPATCH 97 / PHASE 5 step 9 — TUI history-
//! events browse overlay.
//!
//! SCOPED to the event archive (exits/kills/regressions). NOT a
//! chart — the CAR-D97 rule: "no trajectory samples, no ASCII/braille
//! time-series in the terminal." The web HistoryPage (D95) owns the
//! curve; the terminal is a clean event browser.
//!
//! ## Snapshot-on-open (Q5)
//!
//! The archive is snapshotted into [`App::history_events_browse`]
//! when the operator presses `H`. New events arriving on the runtime
//! side while the overlay is up do NOT shift the visible list — the
//! D76 selection-stability lesson applied to the archive too. `r`
//! re-snapshots explicitly.
//!
//! ## No-chart invariant
//!
//! The panel module is scanned by
//! `no_chart_symbols_in_history_events_panel` (below): any string
//! matching `sparkline`, `chart`, `braille`, `▁▂▃▄▅▆▇█`, or `cpu_pct`/
//! `rss_mb`/`vram_mb` in the render path fires. That guard is the
//! wire-side of the CAR-D97 STOP #4 commitment — if a future
//! contributor is tempted to add "just a small sparkline," the test
//! catches them at CI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::history::HistoryEventKind;
use crate::ui::theme::UiTheme;

use super::super::app::{App, HistoryEventsBrowse, event_key};

/// Render the events overlay (panel + dim background outside it).
/// No-op when the browse mode is off — the caller invokes us
/// unconditionally at the end of the frame.
pub fn render(f: &mut Frame, full: Rect, app: &App, theme: &UiTheme) {
    let Some(browse) = app.history_events_browse() else {
        return;
    };
    let area = centered(full, 80, 70);

    f.render_widget(Clear, area);

    // Header text carries the snapshot-time hint per the contract
    // template — the operator sees they're looking at a frozen list.
    let snapshot_hms = browse
        .snapshot_at
        .format("%H:%M:%S")
        .to_string();
    let title = ux_contract::history_events::TITLE.replace("{time}", &snapshot_hms);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header row (count summary)
            Constraint::Min(1),    // body list
            Constraint::Length(1), // footer hint
        ])
        .split(inner);

    f.render_widget(header_line(browse, theme), layout[0]);
    if browse.events.is_empty() {
        f.render_widget(empty_paragraph(theme), layout[1]);
    } else {
        f.render_widget(body_list(browse, theme), layout[1]);
    }
    f.render_widget(footer_paragraph(theme), layout[2]);
}

fn header_line<'a>(browse: &'a HistoryEventsBrowse, theme: &UiTheme) -> Paragraph<'a> {
    let count = browse.events.len();
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {count} events ", ),
            Style::default().fg(theme.foreground),
        ),
        Span::styled(
            "· columns: When  Kind  PID  Name  Summary",
            Style::default().fg(theme.muted),
        ),
    ]))
}

fn empty_paragraph(theme: &UiTheme) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        format!(" {} ", ux_contract::history_events::EMPTY),
        Style::default().fg(theme.attention),
    )))
}

fn body_list<'a>(browse: &'a HistoryEventsBrowse, theme: &UiTheme) -> List<'a> {
    // Resolve the cursor's index once. Fallback to 0 (top) when the
    // selected key is None or aged out of the fresh snapshot.
    let cursor = browse
        .selected_key
        .as_ref()
        .and_then(|k| browse.events.iter().position(|ev| &event_key(ev) == k))
        .unwrap_or(0);

    let items: Vec<ListItem<'_>> = browse
        .events
        .iter()
        .enumerate()
        .map(|(i, ev)| {
            let kind_span = kind_span(ev.kind, theme);
            let when = ev
                .timestamp
                .format("%H:%M:%S")
                .to_string();
            // Row layout matches the D95 web timeline's column order:
            // time, kind, pid, name, summary. Fixed-width fields
            // (time, kind, pid) keep the summary column left-aligned
            // for scanability.
            let mut spans: Vec<Span<'_>> = vec![
                Span::styled(
                    format!(" {when}  "),
                    Style::default().fg(theme.muted),
                ),
                kind_span,
                Span::styled(
                    format!("  {:>6}  ", ev.pid),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
                    format!("{:<16} ", truncate(&ev.name, 16)),
                    Style::default().fg(theme.foreground),
                ),
                Span::styled(
                    truncate(&ev.summary, 128),
                    Style::default().fg(theme.foreground),
                ),
            ];
            // Highlight the cursor row via a modifier — the accent
            // color already carries meaning (kind badge), so we
            // reverse-video the whole row instead of recoloring.
            // Same shape the D74 activity-browse selected row uses.
            if i == cursor {
                for s in spans.iter_mut() {
                    s.style = s.style.add_modifier(Modifier::REVERSED);
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    List::new(items).block(Block::default())
}

/// Kind badge with the ActivityKind/Severity color mapping used
/// elsewhere. Reuses the same semantic palette as the ActivityFeed
/// so an operator's mental model of "yellow means kill, red means
/// regression" carries across surfaces. Fixed-width (10 cols) so
/// the pid column lines up.
fn kind_span(kind: HistoryEventKind, theme: &UiTheme) -> Span<'static> {
    let (label, color) = match kind {
        HistoryEventKind::Exit => ("exit      ", theme.muted),
        HistoryEventKind::Kill => ("kill      ", theme.attention),
        HistoryEventKind::Regression => ("regression", theme.critical),
    };
    Span::styled(
        label.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn footer_paragraph(theme: &UiTheme) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        " j/k navigate · r reload · Esc close ",
        Style::default().fg(theme.muted),
    )))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Centered rect with the given percentage size. Identical to
/// `history_overlay::centered` — kept local to avoid a cross-module
/// dependency for a five-liner.
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

#[cfg(test)]
mod tests {
    // No `use super::*;` — the test bodies scan the module's own
    // source via `include_str!` rather than exercising the render
    // path directly (rendering ratatui requires a Frame + Terminal
    // stack, which is heavy for a pin like this). Source-scan pins
    // catch the failure modes we care about (chart-widget leakage;
    // runtime-side read leakage) at compile-check time.

    /// CAR-D97 STOP #4 — the no-chart-symbols pin. The events panel
    /// must never introduce sparklines, braille time-series, or
    /// per-sample resource fields. This test scans this file's own
    /// source (via `include_str!`) for the forbidden markers; if a
    /// future contributor is tempted to add "just a small sparkline,"
    /// this fires at CI.
    ///
    /// Trajectory-typed field names (`cpu_pct` / `rss_mb` / `vram_mb`)
    /// are also forbidden here — they belong on `Sample` at the web
    /// wire, not in the terminal event browser. Their appearance
    /// would signal that trajectory data is leaking into the TUI
    /// scope (the STOP #4 concern).
    #[test]
    fn no_chart_symbols_in_history_events_panel() {
        let src = include_str!("history_events.rs");
        // Scope the scan to PRODUCTION code only. The `#[cfg(test)]`
        // module below carries the forbidden tokens in its assertion
        // list; without this cut the test would trigger on itself.
        // Comment lines are ALSO stripped so `//! no chart` doc-prose
        // in the module header doesn't false-positive.
        let prod_end = src
            .find("#[cfg(test)]")
            .expect("history_events.rs must have a #[cfg(test)] section for this pin");
        let prod = &src[..prod_end];
        let scanned: String = prod
            .lines()
            .filter(|l| {
                let trimmed = l.trim_start();
                !(trimmed.starts_with("//") || trimmed.starts_with("///")
                    || trimmed.starts_with("//!"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Forbidden tokens — assembled character-by-character where
        // needed so the token list itself doesn't have to appear
        // verbatim in the source (else it'd contradict the scanner
        // even in the tests section). Ratatui's own `Sparkline` /
        // `Chart` widgets are the practical targets: a contributor
        // adding one would type `Sparkline` verbatim in the render
        // path.
        let forbidden: Vec<String> = vec![
            format!("{}{}", "Spark", "line"),
            format!("{}{}", "spark", "line"),
            format!("{}{}", "brail", "le"),
            format!("{}{}", "Brail", "le"),
            format!("{}{}", "ratatui::widgets::", "Chart"),
            format!("{}{}", "cpu", "_pct"),
            format!("{}{}", "rss", "_mb"),
            format!("{}{}", "vram", "_mb"),
            // The braille/block glyphs a terminal sparkline would use.
            // Encoded via char code so the token list here doesn't
            // itself carry them.
            String::from(char::from_u32(0x2581).unwrap()),
            String::from(char::from_u32(0x2582).unwrap()),
            String::from(char::from_u32(0x2583).unwrap()),
            String::from(char::from_u32(0x2584).unwrap()),
            String::from(char::from_u32(0x2585).unwrap()),
            String::from(char::from_u32(0x2586).unwrap()),
            String::from(char::from_u32(0x2587).unwrap()),
            String::from(char::from_u32(0x2588).unwrap()),
        ];
        for needle in forbidden {
            assert!(
                !scanned.contains(&needle),
                "CAR-D97 STOP #4 invariant: history_events.rs contains \
                 the forbidden token `{needle}` — this panel is EVENTS \
                 ONLY, no trajectory charts / sparklines / time-series. \
                 If the operator wants the curve, the web view has it. \
                 Move the surface to `src/web/history.rs` or drop it.",
            );
        }
    }

    /// Snapshot-on-open pin (Q5): the render function pulls from
    /// `App::history_events_browse` (the frozen `HistoryEventsBrowse`),
    /// NOT from `runtime.history_capture().event_archive` directly.
    /// That's what makes the list stable across ticks while the
    /// overlay is up.
    #[test]
    fn render_reads_from_frozen_snapshot_not_runtime_history() {
        let src = include_str!("history_events.rs");
        // Scope to PRODUCTION code (same reason as
        // `no_chart_symbols_in_history_events_panel`): the test
        // assertions carry the forbidden token strings verbatim and
        // would false-positive against themselves without this cut.
        let prod_end = src
            .find("#[cfg(test)]")
            .expect("history_events.rs must have a #[cfg(test)] section for this pin");
        let prod = &src[..prod_end];
        // Also strip doc-comments — the module header explains WHY
        // the panel doesn't read `event_archive`, and that prose
        // would otherwise trip the scan.
        let scanned: String = prod
            .lines()
            .filter(|l| {
                let trimmed = l.trim_start();
                !(trimmed.starts_with("//") || trimmed.starts_with("///")
                    || trimmed.starts_with("//!"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The render function must go through App's frozen snapshot.
        // Tokens built via `format!` so this test's own source doesn't
        // false-positive on the assertion strings.
        let must_have = format!("app.{}()", "history_events_browse");
        let forbidden_capture = format!("runtime.{}", "history_capture");
        let forbidden_archive = format!("{}{}", "event_", "archive");
        assert!(
            scanned.contains(&must_have),
            "history_events::render must read from App's frozen snapshot \
             (`app.history_events_browse()`)",
        );
        assert!(
            !scanned.contains(&forbidden_capture),
            "CAR-D97 Q5 (snapshot-on-open): the panel must NOT reach into \
             runtime for a live archive read. The snapshot is taken at \
             overlay-open time (and on `r` reload) by App's reload method; \
             the render path reads the frozen copy only.",
        );
        assert!(
            !scanned.contains(&forbidden_archive),
            "history_events::render must NOT touch the live archive \
             directly. That would defeat the snapshot-on-open Q5 invariant.",
        );
    }
}
