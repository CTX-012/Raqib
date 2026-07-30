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

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ux_contract::mission;

use crate::ui::app::App;
use crate::ui::symbols::SymbolSet;
use crate::ui::theme::UiTheme;

/// TUI header web-link — build the operator-facing URL from the
/// runtime bind address + port. Pure; unit-testable without a Frame.
///
/// ## Bind-address mapping
///
/// * `127.0.0.1` / `::1` → `localhost` (nicer text than the IP form;
///   equivalent from any browser on the same host).
/// * `0.0.0.0` / `::` → `localhost` too. The zero-address is not
///   browsable — 0.0.0.0 is a "listen on every interface" bind, not
///   a routable target. `localhost` always works when the server is
///   bound to it, and telling the operator "http://0.0.0.0:7070"
///   would send them to a dead URL. LAN operators who want the
///   remote-reachable form already know their host IP; we don't
///   over-engineer per-interface discovery here (would need
///   `getifaddrs` + a heuristic for "primary" LAN — brittle).
/// * Any other IP (a specific bind like `192.168.1.21`) → verbatim,
///   since the operator explicitly chose that address and it IS
///   browsable.
///
/// Always emits `http://` (not `https://`) — the web companion
/// doesn't terminate TLS itself (the README's trusted-LAN
/// assumption). Operators fronting it with a reverse proxy get
/// their real URL via other channels; the TUI reflects the actual
/// bind.
pub fn web_display_url(bind: IpAddr, port: u16) -> String {
    let host = match bind {
        IpAddr::V4(v4) if v4 == Ipv4Addr::UNSPECIFIED || v4.is_loopback() => {
            "localhost".to_string()
        }
        IpAddr::V6(v6) if v6 == Ipv6Addr::UNSPECIFIED || v6.is_loopback() => {
            "localhost".to_string()
        }
        IpAddr::V6(v6) => {
            // Bracketed IPv6-literal form per RFC 3986 §3.2.2 —
            // "http://[fe80::1]:7070" not "http://fe80::1:7070"
            // (the latter parses the trailing ':7070' as part of
            // the address).
            format!("[{v6}]")
        }
        IpAddr::V4(v4) => v4.to_string(),
    };
    format!("http://{host}:{port}")
}

/// F1 — minimum gap between the contract mission text and the wall
/// clock. Below this we hide the clock entirely; rendering with one
/// space of padding (or zero) would either look like a typo or cause
/// the clock to abut the mission text. The threshold is 2 because
/// "edge_monitor · 0 workloads · 0 degraded · press ? for help" plus
/// " HH:MM:SS" needs a visible gutter.
const MIN_TIME_GAP_COLS: u16 = 2;
/// F1 — wall clock format. HH:MM:SS in the local timezone so an SSH
/// session running across timezones sees the operator's local wall
/// clock, not UTC.
const TIME_FORMAT: &str = "%H:%M:%S";
const TIME_WIDTH_COLS: u16 = 8;

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
    let label = mission_line_text_with_web(
        app.symbol_set(),
        n_workloads,
        n_degraded,
        app.web_url(),
    );
    let now_str = Local::now().format(TIME_FORMAT).to_string();
    let assembled = assemble_mission_line(&label, &now_str, area.width);
    // Leading space mirrors the contract's render — the §0 example
    // shows two columns of padding before the product name. One space
    // is enough here because the panel block borders for the System
    // panel below start at col 0; the visual gutter is provided by
    // the indentation alone.
    let line = Paragraph::new(Line::from(Span::styled(
        assembled,
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(line, area);
}

/// F1 — compose the rendered mission line: contract text on the left,
/// padding, wall clock right-aligned. Pure so tests can drive
/// assertions without spinning a `Frame` or wiring real time.
///
/// Drops the clock entirely when the terminal is too narrow to fit
/// both with at least `MIN_TIME_GAP_COLS` of visual breathing room
/// between them — at that point the operator is already squeezed and
/// the mission text is the higher-value content.
///
/// Padding is computed against the column width of `label` (treated
/// as ASCII for col-width purposes: the contract template is pure
/// ASCII plus `·`, which renders at one column; the SymbolSet swap
/// keeps that property in the ASCII fallback). One leading space
/// matches the pre-F1 render and the §0 example's gutter.
pub fn assemble_mission_line(label: &str, time_str: &str, area_width: u16) -> String {
    // 1 col leading space + label cols.
    let label_cols = (label.chars().count() as u16).saturating_add(1);
    let time_cols = time_str.chars().count() as u16;
    // Need: label_cols + gap + time_cols ≤ area_width, gap ≥ MIN_TIME_GAP_COLS.
    let needed = label_cols
        .saturating_add(MIN_TIME_GAP_COLS)
        .saturating_add(time_cols);
    if area_width < needed || time_str.is_empty() {
        // Too narrow — render the label alone (pre-F1 behaviour).
        return format!(" {label}");
    }
    let pad = (area_width as usize)
        .saturating_sub(label_cols as usize)
        .saturating_sub(time_cols as usize);
    format!(" {label}{}{}", " ".repeat(pad), time_str)
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
    mission_line_text_with_web(set, n_workloads, n_degraded, None)
}

/// Same as [`mission_line_text`], but appends a "web: <url>" tail
/// when `web_url` is `Some(_)`. When the web companion is disabled
/// (`--no-web`), the caller passes `None` and the header renders
/// the pre-dispatch shape — honesty: we don't advertise a server
/// that isn't running.
///
/// The URL is appended using the same `SymbolSet::header_separator`
/// so the ASCII fallback stays consistent (` - web: …` vs
/// ` · web: …`).
pub fn mission_line_text_with_web(
    set: SymbolSet,
    n_workloads: usize,
    n_degraded: usize,
    web_url: Option<&str>,
) -> String {
    let text = mission::TEMPLATE
        .replace("{n}", &n_workloads.to_string())
        .replace("{m}", &n_degraded.to_string());
    let sep = set.header_separator();
    let base = if sep == "·" { text } else { text.replace('·', sep) };
    match web_url {
        Some(url) => format!("{base} {sep} web: {url}"),
        None => base,
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
            "raqib · 3 workloads · 1 degraded · press ? for help"
        );
    }

    #[test]
    fn ascii_text_uses_hyphen_separator() {
        let text = mission_line_text(SymbolSet::Ascii, 3, 1);
        assert_eq!(
            text,
            "raqib - 3 workloads - 1 degraded - press ? for help"
        );
    }

    #[test]
    fn zero_workloads_renders_zero_counts() {
        let text = mission_line_text(SymbolSet::Unicode, 0, 0);
        assert!(text.contains("0 workloads"));
        assert!(text.contains("0 degraded"));
    }

    // ── F1 — right-aligned wall clock on mission line ───────────────
    //
    // `assemble_mission_line` is the pure composition site; it owns
    // both the right-align math and the "too-narrow → drop the clock"
    // fallback. Tests pin both ends of the gate.

    #[test]
    fn mission_line_renders_time_right_aligned_on_wide_terminal() {
        let label = mission_line_text(SymbolSet::Unicode, 3, 1);
        // Wide terminal — clock must fit with a visible gap.
        let line = assemble_mission_line(&label, "14:23:01", 120);
        assert!(
            line.ends_with("14:23:01"),
            "wide terminal must right-align the clock; got {line:?}"
        );
        assert!(
            line.starts_with(" raqib"),
            "leading-space gutter preserved; got {line:?}"
        );
        // No double spaces inside the contract label itself — padding
        // sits in the middle of the line, not at the seams.
        assert!(line.contains("press ? for help"));
    }

    #[test]
    fn mission_line_hides_time_on_narrow_terminal() {
        // 60 cols is below the §12 80-col floor anyway, but the gate
        // here is per-area-width: if the label + clock + gap doesn't
        // fit, drop the clock and render the label alone.
        let label = mission_line_text(SymbolSet::Unicode, 3, 1);
        let line = assemble_mission_line(&label, "14:23:01", 60);
        assert!(
            !line.contains("14:23:01"),
            "narrow terminal must hide the clock; got {line:?}"
        );
        assert_eq!(line, format!(" {label}"));
    }

    #[test]
    fn mission_line_format_is_hh_mm_ss() {
        // Clock string must be exactly 8 chars wide (HH:MM:SS) so the
        // right-align math is stable across the day. Confirm by
        // re-formatting `Local::now()` through the same format string
        // the render path uses.
        let now = Local::now().format(TIME_FORMAT).to_string();
        assert_eq!(
            now.chars().count(),
            TIME_WIDTH_COLS as usize,
            "clock string must be HH:MM:SS (8 cols); got {now:?}"
        );
        for (i, c) in now.chars().enumerate() {
            if i == 2 || i == 5 {
                assert_eq!(c, ':', "expected ':' at index {i} of {now:?}");
            } else {
                assert!(c.is_ascii_digit(), "expected digit at index {i} of {now:?}");
            }
        }
    }

    // ── TUI header web-link — URL builder + mission-line-with-URL ────

    /// Loopback IPs render as `localhost` — nicer text than `127.0.0.1`
    /// and identical to any local browser. The two IPv4 loopback
    /// forms (127.0.0.1 explicitly + Ipv4Addr::LOCALHOST) both hit
    /// the same branch.
    #[test]
    fn web_display_url_loopback_ipv4_renders_localhost() {
        let url = web_display_url(IpAddr::V4(Ipv4Addr::LOCALHOST), 7070);
        assert_eq!(url, "http://localhost:7070");
    }

    /// 0.0.0.0 is a listen-on-any-interface bind, NOT a routable
    /// target — a browser can't fetch http://0.0.0.0:PORT. Render as
    /// `localhost` so the operator gets a URL that WORKS locally.
    /// LAN operators know their host IP through other channels; we
    /// don't over-engineer per-interface discovery here.
    #[test]
    fn web_display_url_zero_ipv4_renders_localhost_not_zero() {
        let url = web_display_url(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7070);
        assert_eq!(url, "http://localhost:7070",
            "0.0.0.0 must render as `localhost` (0.0.0.0 is not browsable)");
    }

    /// A specific bind IP (LAN address the operator chose) renders
    /// verbatim — that IS the URL the operator meant.
    #[test]
    fn web_display_url_specific_ipv4_renders_verbatim() {
        let url = web_display_url(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 21)), 7070);
        assert_eq!(url, "http://192.168.1.21:7070");
    }

    /// Custom port propagates faithfully — operator ran `--port 7099`
    /// and the header must reflect that, not the 7070 default.
    #[test]
    fn web_display_url_custom_port_appears_verbatim() {
        let url = web_display_url(IpAddr::V4(Ipv4Addr::LOCALHOST), 7099);
        assert_eq!(url, "http://localhost:7099");
    }

    /// IPv6 loopback (::1) + unspecified (::) both map to `localhost`.
    #[test]
    fn web_display_url_loopback_and_zero_ipv6_render_localhost() {
        let loopback = web_display_url(IpAddr::V6(Ipv6Addr::LOCALHOST), 7070);
        assert_eq!(loopback, "http://localhost:7070");
        let unspec = web_display_url(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 7070);
        assert_eq!(unspec, "http://localhost:7070");
    }

    /// A specific IPv6 address is bracketed per RFC 3986 §3.2.2 —
    /// `http://[fe80::1]:7070` not `http://fe80::1:7070` (which
    /// parses as a different address).
    #[test]
    fn web_display_url_specific_ipv6_is_bracketed() {
        let addr = "fe80::1".parse::<Ipv6Addr>().unwrap();
        let url = web_display_url(IpAddr::V6(addr), 7070);
        assert_eq!(url, "http://[fe80::1]:7070");
    }

    /// The mission line WITH a web URL appends " <sep> web: <url>"
    /// to the base template. Verifies the append happens on the
    /// Unicode symbol set with the `·` separator.
    #[test]
    fn mission_line_with_web_url_appends_via_separator() {
        let text = mission_line_text_with_web(
            SymbolSet::Unicode,
            3,
            1,
            Some("http://localhost:7070"),
        );
        assert!(
            text.contains("press ? for help · web: http://localhost:7070"),
            "URL must append after the help hint via the ` · ` separator; got {text:?}",
        );
    }

    /// ASCII fallback: the `·` separator stays `-`, so the appended
    /// URL uses ` - ` too (glyph consistency).
    #[test]
    fn mission_line_with_web_url_ascii_uses_hyphen_separator() {
        let text = mission_line_text_with_web(
            SymbolSet::Ascii,
            3,
            1,
            Some("http://localhost:7070"),
        );
        assert!(
            text.contains("press ? for help - web: http://localhost:7070"),
            "ASCII fallback must use ` - ` throughout; got {text:?}",
        );
    }

    /// `None` (the `--no-web` case) emits the pre-dispatch mission
    /// line unchanged — no "web:" tail, no trailing separator. The
    /// honesty rule: don't advertise a server that isn't running.
    #[test]
    fn mission_line_with_no_web_url_hides_the_tail() {
        let text = mission_line_text_with_web(SymbolSet::Unicode, 3, 1, None);
        assert!(
            !text.contains("web:"),
            "no web URL → no 'web:' tail; got {text:?}",
        );
        // And it MUST equal the classic mission_line_text output.
        assert_eq!(text, mission_line_text(SymbolSet::Unicode, 3, 1));
    }

    /// The URL text is emitted as-is (no ANSI/OSC 8 escape junk in
    /// the string). This pins the "plain-text URL" decision from
    /// the dispatch STOP #1: ratatui can't cleanly emit OSC 8, and
    /// visible escape garbage would be worse than a plain URL. Most
    /// modern terminals auto-linkify bare URLs for Ctrl-click, so
    /// the operator still gets clickability on iTerm2/Kitty/WezTerm/
    /// GNOME Terminal/Konsole/Windows Terminal without escape bytes.
    #[test]
    fn mission_line_web_url_has_no_ansi_escape_bytes() {
        let text = mission_line_text_with_web(
            SymbolSet::Unicode,
            0,
            0,
            Some("http://localhost:7070"),
        );
        assert!(
            !text.contains('\x1b'),
            "URL text must be plain (no ANSI/OSC 8 escapes — ratatui filters them + visible garbage would be worse than plain); got {text:?}",
        );
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
