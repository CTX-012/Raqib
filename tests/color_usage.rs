//! L21 / UX_CONTRACT.md §14 — color-usage golden-image tests.
//!
//! Pins the four §14 rules end-to-end via `TestBackend`:
//!
//!   1. Status dots on workload rows are the only colored thing —
//!      the rest of the row reads in `theme.foreground`.
//!   2. Bar graphs flip color at 85% (attention) and 95% (critical).
//!   3. Section headers (panel block titles) render in
//!      `theme.muted` when unfocused, `theme.accent` when focused.
//!   4. Footer key letters render in `theme.accent`; descriptions
//!      render in `theme.muted`.
//!
//! Uses TestBackend at the §12 Standard breakpoint (120×40). Reuses
//! the harness pattern established by `tests/theme_switching.rs` and
//! `tests/header_rendering.rs`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use edge_monitor::ui::theme::{UiTheme, current_theme};

// ─── Rule 4: footer ────────────────────────────────────────────────

#[test]
fn footer_key_letters_render_in_accent() {
    // The footer is rendered by `panels::mod.rs::render_footer` from
    // inside the top-level `panels::render`. We exercise the full
    // panels::render path because render_footer is private.
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::ui::app::App;
    use edge_monitor::ui::panels;

    let theme = current_theme("dark");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default());
    let app = App::new();

    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, &theme, None, None))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // Footer is row 39 (last) in a 120×40 frame. Scan the row for
    // each key letter and assert its fg is `theme.accent`. Letters
    // appear in the order q · j/k · k · h · ? — so we look for the
    // first 'q' from the left edge.
    let footer_y = 39u16;
    let mut q_fg = None;
    for x in 0..120 {
        let cell = buffer.cell((x, footer_y)).expect("cell");
        if cell.symbol() == "q" {
            q_fg = Some(cell.style().fg);
            break;
        }
    }
    assert_eq!(
        q_fg.flatten(),
        Some(theme.accent),
        "footer 'q' key letter must use theme.accent"
    );
}

#[test]
fn footer_descriptions_render_in_muted() {
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::ui::app::App;
    use edge_monitor::ui::panels;

    let theme = current_theme("dark");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default());
    let app = App::new();

    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, &theme, None, None))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // The description "quit" sits right after the 'q' key letter on
    // the footer row. Find 'q' then walk past the leading space to
    // 'u' of "uit" and assert that cell uses theme.muted.
    let footer_y = 39u16;
    let mut q_col = None;
    for x in 0..120 {
        if buffer.cell((x, footer_y)).expect("cell").symbol() == "q" {
            q_col = Some(x);
            break;
        }
    }
    let q_col = q_col.expect("'q' must be present in footer");
    // Layout produced by render_footer:
    //   [" "(raw)] [q(accent)] [" quit"(muted)] [" · "(muted)] ...
    // The first 'q' the loop finds is the key letter at q_col=1.
    // The description begins at q_col+1 with a leading space, then
    // 'q' (q_col+2) 'u' (q_col+3) 'i' (q_col+4) 't' (q_col+5).
    let u_cell = buffer.cell((q_col + 3, footer_y)).expect("cell");
    assert_eq!(u_cell.symbol(), "u", "expected 'u' of 'quit' description");
    assert_eq!(
        u_cell.style().fg,
        Some(theme.muted),
        "footer description must use theme.muted, not accent"
    );
}

// ─── Rule 2: bar graphs ────────────────────────────────────────────

#[test]
fn bar_color_at_50_percent_uses_foreground() {
    let theme = current_theme("dark");
    assert_eq!(theme.bar_color(50.0), theme.foreground);
}

#[test]
fn bar_color_at_87_percent_uses_attention() {
    let theme = current_theme("dark");
    // §14 threshold band: 85% ≤ pct < 95%.
    assert_eq!(theme.bar_color(87.0), theme.attention);
}

#[test]
fn bar_color_at_96_percent_uses_critical() {
    let theme = current_theme("dark");
    // §14 threshold band: pct ≥ 95%.
    assert_eq!(theme.bar_color(96.0), theme.critical);
}

#[test]
fn bar_color_boundary_85_is_attention_not_foreground() {
    let theme = current_theme("dark");
    // Pin the half-open boundary: exactly 85% is already attention.
    assert_eq!(theme.bar_color(85.0), theme.attention);
}

#[test]
fn bar_color_boundary_95_is_critical_not_attention() {
    let theme = current_theme("dark");
    // Pin the half-open boundary: exactly 95% is already critical.
    assert_eq!(theme.bar_color(95.0), theme.critical);
}

// ─── Rule 1: status dot is the only colored thing on workload rows ─

#[test]
fn status_color_maps_per_workload_status() {
    use ux_contract::WorkloadStatus;

    let theme = current_theme("dark");
    // Each WorkloadStatus variant resolves to the right palette
    // slot. The render path in `workloads.rs` splits the row into a
    // colored dot span + a foreground-only rest span; testing the
    // helper here pins the mapping the render path consumes.
    assert_eq!(theme.status_color(WorkloadStatus::Healthy), theme.healthy);
    assert_eq!(theme.status_color(WorkloadStatus::Attention), theme.attention);
    assert_eq!(theme.status_color(WorkloadStatus::Critical), theme.critical);
    assert_eq!(theme.status_color(WorkloadStatus::Loading), theme.muted);
}

// ─── Rule 3: section headers ───────────────────────────────────────

#[test]
fn vitals_panel_renders_with_muted_block_border() {
    // The Vitals panel is unfocused by default and §14 says
    // unfocused section headers (== panel block borders) render
    // muted. Render a full frame and check the top-left corner of
    // the vitals panel (row 1, col 0 — the panel sits just below
    // the §0 mission-line header).
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::ui::app::App;
    use edge_monitor::ui::panels;

    let theme = current_theme("dark");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default());
    let app = App::new();

    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, &theme, None, None))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // The §0 header occupies row 0; the vitals panel border starts
    // at row 1. The top-left corner glyph is the box-drawing
    // character '╭' (or similar) styled with the border color.
    let border_cell = buffer.cell((0, 1)).expect("cell");
    assert_eq!(
        border_cell.style().fg,
        Some(theme.muted),
        "Vitals panel (unfocused) border must use theme.muted, got cell {border_cell:?}"
    );
}

#[test]
fn workloads_panel_renders_with_accent_block_border_when_focused() {
    // The Workloads panel is always rendered with `focused=true` per
    // panels::mod.rs::render_default. §14: focused borders use
    // accent.
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::ui::app::App;
    use edge_monitor::ui::panels;

    let theme = current_theme("dark");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default());
    let app = App::new();

    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, &theme, None, None))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // Layout from render_default:
    //   row 0     : §0 header
    //   rows 1-7  : vitals (7)
    //   row 8     : B7 spacer (1)
    //   rows 9-N  : workloads (flexes)
    // First row of the workloads block border is row 9.
    let workloads_border_cell = buffer.cell((0, 9)).expect("cell");
    assert_eq!(
        workloads_border_cell.style().fg,
        Some(theme.accent),
        "focused Workloads panel border must use theme.accent, got {workloads_border_cell:?}"
    );
}

// ─── Theme propagation: switching theme changes rendered colors ────

#[test]
fn switching_theme_changes_footer_key_letter_color() {
    // The footer must follow `--theme`. Pre-L21 it was hardcoded
    // `Color::DarkGray` and didn't pick up `--theme light` or
    // `--theme high-contrast`. Render the same scene under two
    // themes and assert the rendered key-letter color differs.
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::ui::app::App;
    use edge_monitor::ui::panels;

    fn footer_q_fg(theme: &UiTheme) -> Option<ratatui::style::Color> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let runtime = Runtime::new(Config::default());
        let app = App::new();
        terminal
            .draw(|f| panels::render(f, runtime.state(), &app, theme, None, None))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let footer_y = 39u16;
        for x in 0..120 {
            let cell = buffer.cell((x, footer_y)).expect("cell");
            if cell.symbol() == "q" {
                return cell.style().fg;
            }
        }
        None
    }

    let dark = footer_q_fg(&current_theme("dark"));
    let hc = footer_q_fg(&current_theme("high-contrast"));
    let light = footer_q_fg(&current_theme("light"));
    assert!(dark.is_some(), "dark theme key letter must have fg");
    assert_ne!(dark, hc, "key letter color must change with theme");
    assert_ne!(hc, light, "key letter color must change with theme");
    assert_ne!(dark, light, "key letter color must change with theme");
}

// Provide the rect type used by some assertions even though tests
// don't render manually — keeps the import discoverable when tests
// are extended.
#[allow(dead_code)]
fn _dummy_rect_use() -> Rect {
    Rect::new(0, 0, 0, 0)
}
