//! L20 / UX_CONTRACT.md §13 — theme switching observed at panel-fill
//! time.
//!
//! The render layer pulls hex values from `ux_contract::{DARK, LIGHT,
//! HIGH_CONTRAST}` and converts them to `ratatui::style::Color::Rgb`
//! via `edge_monitor::ui::theme::current_theme`. This test renders the
//! TUI's status bar into a `TestBackend` with each of the three
//! themes and asserts that the foreground color landed on the title
//! span matches the contract palette — which incidentally also pins
//! that swapping themes actually changes rendered output (i.e. the
//! plumbing is wired end-to-end rather than parsing the hex and then
//! dropping it on the floor).
//!
//! L20 wires a single panel-fill site (the status-bar title) so the
//! test has a stable cell to inspect. L21 will sweep the rest of the
//! panels; the expansion is tested by extending this file with
//! additional `theme.X` assertions per panel as they land.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use edge_monitor::config::Config;
use edge_monitor::runtime::Runtime;
use edge_monitor::ui::app::App;
use edge_monitor::ui::panels;
use edge_monitor::ui::theme::{UiTheme, current_theme};

/// Render one frame with the supplied theme and return the foreground
/// color of the cell at the start of the status-bar title (" edge_monitor "
/// at row 0, col 1 — col 0 is the leading space of the title span).
fn rendered_title_fg(theme: &UiTheme) -> Color {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let runtime = Runtime::new(Config::default());
    let app = App::new();

    // Prime: Runtime::new doesn't tick on construction, so render
    // against a `RuntimeState` that's still at its initial default —
    // that's fine, the status bar only reads `tick_count` from state,
    // which is present on default.
    terminal
        .draw(|f| panels::render(f, runtime.state(), &app, theme, None, None))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let cell = buffer.cell((1, 0)).expect("status-bar cell at (1, 0)");
    cell.style().fg.expect("title span sets fg from theme")
}

#[test]
fn dark_theme_renders_dark_foreground() {
    let theme = current_theme("dark");
    let fg = rendered_title_fg(&theme);
    assert_eq!(fg, Color::Rgb(0xc0, 0xca, 0xf5));
}

#[test]
fn light_theme_renders_light_foreground() {
    let theme = current_theme("light");
    let fg = rendered_title_fg(&theme);
    assert_eq!(fg, Color::Rgb(0x2c, 0x2c, 0x2a));
}

#[test]
fn high_contrast_theme_renders_white_foreground() {
    let theme = current_theme("high-contrast");
    let fg = rendered_title_fg(&theme);
    assert_eq!(fg, Color::Rgb(0xff, 0xff, 0xff));
}

#[test]
fn three_themes_produce_distinct_rendered_colors() {
    let dark = rendered_title_fg(&current_theme("dark"));
    let light = rendered_title_fg(&current_theme("light"));
    let hc = rendered_title_fg(&current_theme("high-contrast"));
    assert_ne!(dark, light, "dark and light must paint different colors");
    assert_ne!(light, hc, "light and high-contrast must paint different colors");
    assert_ne!(dark, hc, "dark and high-contrast must paint different colors");
}

#[test]
fn unknown_theme_name_falls_back_to_dark() {
    // Operators may hand-edit `[ui].theme` to an unsupported value;
    // the renderer must still produce a usable frame rather than
    // refusing to launch. §13 default is Dark.
    let fg = rendered_title_fg(&current_theme("solarized"));
    let dark_fg = rendered_title_fg(&current_theme("dark"));
    assert_eq!(fg, dark_fg);
}

#[test]
fn underscore_and_dash_spellings_resolve_identically() {
    // The contract names the variant `HighContrast`; users copying
    // that spelling into a config will type `high_contrast`, while
    // CLI users tend to type `high-contrast`. Both must select the
    // same palette.
    let dash = rendered_title_fg(&current_theme("high-contrast"));
    let under = rendered_title_fg(&current_theme("high_contrast"));
    assert_eq!(dash, under);
}
