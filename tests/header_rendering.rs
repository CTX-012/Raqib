//! L25 / UX_CONTRACT.md §0 — golden-image header rendering.
//!
//! The mission line is the single line at the top of the default
//! screen (between the alert region and the System panel). It carries
//! the product name, the live workload count, the degraded count, and
//! the help hint — all separated by `·` (or `-` in ASCII fallback).
//!
//! These tests render the header into a `TestBackend` and read the
//! resulting buffer back to cells, so they catch regressions in:
//!   - exact wording / separator choice
//!   - count substitution at zero, single-digit, and multi-digit values
//!   - help-hint preservation (the only discoverability hook for new
//!     operators before they've spotted the footer keymap)
//!   - L20 theme plumbing (the title still picks up theme.foreground)

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;

use edge_monitor::ui::app::App;
use edge_monitor::ui::panels::header;
use edge_monitor::ui::symbols::SymbolSet;
use edge_monitor::ui::theme::{UiTheme, current_theme};

/// Read row 0 of the rendered buffer back into a String. Strips the
/// trailing padding so assertions don't have to count spaces.
fn render_row0(n_workloads: usize, n_degraded: usize, theme: &UiTheme) -> String {
    let backend = TestBackend::new(80, 3);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let app = App::with_symbol_set(SymbolSet::Unicode);

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 1);
            header::render(f, area, &app, theme, n_workloads, n_degraded);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut row = String::new();
    for x in 0..80 {
        row.push_str(buffer.cell((x, 0)).expect("cell").symbol());
    }
    row.trim_end().to_string()
}

#[test]
fn header_renders_full_mission_line_with_counts() {
    let theme = current_theme("dark");
    let row = render_row0(3, 1, &theme);
    assert_eq!(
        row,
        " edge_monitor · 3 workloads · 1 degraded · press ? for help"
    );
}

#[test]
fn header_renders_zero_counts_cleanly() {
    let theme = current_theme("dark");
    let row = render_row0(0, 0, &theme);
    assert!(
        row.contains("0 workloads"),
        "expected '0 workloads' in {row:?}"
    );
    assert!(
        row.contains("0 degraded"),
        "expected '0 degraded' in {row:?}"
    );
    assert!(
        row.ends_with("press ? for help"),
        "expected trailing help hint in {row:?}"
    );
}

#[test]
fn header_substitutes_multi_digit_counts() {
    let theme = current_theme("dark");
    let row = render_row0(42, 17, &theme);
    assert!(
        row.contains("42 workloads"),
        "expected '42 workloads' in {row:?}"
    );
    assert!(
        row.contains("17 degraded"),
        "expected '17 degraded' in {row:?}"
    );
}

#[test]
fn header_trailing_help_hint_is_always_present() {
    let theme = current_theme("dark");
    for (n, m) in [(0, 0), (1, 0), (1, 1), (5, 2), (99, 99)] {
        let row = render_row0(n, m, &theme);
        assert!(
            row.ends_with("press ? for help"),
            "missing help hint for ({n}, {m}): {row:?}"
        );
    }
}

#[test]
fn header_title_picks_up_theme_foreground() {
    // L20 plumbing invariant: switching themes must change the
    // rendered fg of the title text. cell (1, 0) is the first
    // character of 'edge_monitor' (col 0 is the leading space).
    let backend = TestBackend::new(80, 3);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let app = App::with_symbol_set(SymbolSet::Unicode);
    let theme = current_theme("high-contrast");

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 1);
            header::render(f, area, &app, &theme, 2, 0);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let fg = buffer.cell((1, 0)).expect("cell").style().fg.expect("fg");
    assert_eq!(fg, Color::Rgb(0xff, 0xff, 0xff));
}

#[test]
fn header_uses_ascii_separator_when_symbol_set_is_ascii() {
    // A `LANG=C` session falls back to `SymbolSet::Ascii` at startup.
    // The header must respect that — the mission line should render
    // with `-` between fields rather than the U+00B7 middle dot.
    let backend = TestBackend::new(80, 3);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let app = App::with_symbol_set(SymbolSet::Ascii);
    let theme = current_theme("dark");

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 1);
            header::render(f, area, &app, &theme, 2, 1);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut row = String::new();
    for x in 0..80 {
        row.push_str(buffer.cell((x, 0)).expect("cell").symbol());
    }
    let row = row.trim_end().to_string();

    assert_eq!(
        row,
        " edge_monitor - 2 workloads - 1 degraded - press ? for help"
    );
    assert!(
        !row.contains('·'),
        "ASCII-only session must not render U+00B7 middle dot: {row:?}"
    );
}
