//! L25 / UX_CONTRACT.md §0 — mission-line header.
//!
//! Single-line render at the top of the default screen, between the
//! alert region (§1 region 1, above) and the System panel (§1 region
//! 3, below). Content is locked by §0:
//!
//! ```text
//!   edge_monitor · {n} workloads · {m} degraded · press ? for help
//! ```
//!
//! `{n}` is the total live AI-classified workload count;  `{m}` is
//! the subset whose `WorkloadStatus` is `Attention` or `Critical`
//! (per §3 — `Loading` and `Healthy` are not "degraded"). Counts
//! flow in from the caller because the workload row computation
//! (`panels::workloads::ordered_rows`) is the single source of truth
//! for status — recomputing it here would risk drift.
//!
//! The `·` separator routes through `SymbolSet::header_separator` so
//! a `LANG=C` SSH session that fell back to ASCII at startup renders
//! `-` instead, matching the rest of the TUI's glyph regime.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use ux_contract::mission;

use crate::ui::app::App;
use crate::ui::symbols::SymbolSet;
use crate::ui::theme::UiTheme;

/// Render the mission line into `area`. `area` is expected to be one
/// row tall — the caller's `Layout` reserves exactly `Length(1)` for
/// this slot.
pub fn render(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: &UiTheme,
    n_workloads: usize,
    n_degraded: usize,
) {
    let label = mission_line_text(app.symbol_set(), n_workloads, n_degraded);
    // Leading space mirrors the contract's render — the §0 example
    // shows two columns of padding before the product name. One space
    // is enough here because the panel block borders for the System
    // panel below start at col 0; the visual gutter is provided by
    // the indentation alone.
    let line = Paragraph::new(Line::from(Span::styled(
        format!(" {label}"),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(line, area);
}

/// Assemble the mission-line text for the given symbol set + counts.
/// Pure — exposed for unit tests in this module and the integration
/// test at `tests/header_rendering.rs` so assertions can target the
/// text shape without spinning a `TestBackend`.
///
/// D5 — sources the template from `ux_contract::mission::TEMPLATE`
/// rather than rebuilding the format string locally, so any future
/// edit to the §0 mission line happens once in the contract crate and
/// both consumers (L25 here, W46 on Windows) pick it up. The
/// `SymbolSet` override is preserved for `LANG=C` SSH sessions that
/// fall back to ASCII: when the active symbol set isn't Unicode we
/// swap the contract's `·` for the set's `header_separator()` so the
/// header matches the rest of the TUI's glyph regime.
pub fn mission_line_text(set: SymbolSet, n_workloads: usize, n_degraded: usize) -> String {
    let text = mission::TEMPLATE
        .replace("{n}", &n_workloads.to_string())
        .replace("{m}", &n_degraded.to_string());
    let sep = set.header_separator();
    if sep == "·" {
        text
    } else {
        text.replace('·', sep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_text_uses_middle_dot_separator() {
        let text = mission_line_text(SymbolSet::Unicode, 3, 1);
        assert_eq!(
            text,
            "edge_monitor · 3 workloads · 1 degraded · press ? for help"
        );
    }

    #[test]
    fn ascii_text_uses_hyphen_separator() {
        let text = mission_line_text(SymbolSet::Ascii, 3, 1);
        assert_eq!(
            text,
            "edge_monitor - 3 workloads - 1 degraded - press ? for help"
        );
    }

    #[test]
    fn zero_workloads_renders_zero_counts() {
        let text = mission_line_text(SymbolSet::Unicode, 0, 0);
        assert!(text.contains("0 workloads"));
        assert!(text.contains("0 degraded"));
    }

    #[test]
    fn trailing_help_hint_is_present() {
        // The "press ? for help" tail is non-optional — it's the only
        // discoverability hook for the help overlay shown on a fresh
        // boot before the operator has scanned the footer keymap.
        for n in 0..5 {
            for m in 0..=n {
                let text = mission_line_text(SymbolSet::Unicode, n, m);
                assert!(
                    text.ends_with("press ? for help"),
                    "text {text:?} missing trailing help hint"
                );
            }
        }
    }
}
