//! L20 / UX_CONTRACT.md §13 — three-theme adapter for the ratatui side.
//!
//! `ux_contract::Theme` carries hex strings so the contract stays
//! platform-neutral (the same struct is consumed by the Windows mirror
//! and any future renderer). This module converts those hex values to
//! `ratatui::style::Color` once at TUI startup and exposes a `UiTheme`
//! the render layer can use without re-parsing.
//!
//! L20 only installs the plumbing — theme selection via CLI / config,
//! a hex-to-Color helper, and a single demonstrative panel-fill point
//! at the status-bar title so `tests/theme_switching.rs` can pin the
//! switch end-to-end. The full color-usage audit lives in L21.

use ratatui::style::Color;
use ux_contract::{DARK, HIGH_CONTRAST, LIGHT, Theme, ThemeName};

/// Theme palette in ratatui-native form. Mirrors `ux_contract::Theme`
/// field-for-field but with `Color::Rgb` values pre-parsed from the
/// hex strings.
#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub name: ThemeName,
    pub background: Color,
    pub background_raised: Color,
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    pub healthy: Color,
    pub attention: Color,
    pub critical: Color,
}

impl UiTheme {
    /// Convert a contract `Theme` into the renderer-side `UiTheme`.
    pub fn from_contract(theme: &Theme) -> Self {
        Self {
            name: theme.name,
            background: parse_hex(theme.background),
            background_raised: parse_hex(theme.background_raised),
            foreground: parse_hex(theme.foreground),
            muted: parse_hex(theme.muted),
            accent: parse_hex(theme.accent),
            healthy: parse_hex(theme.healthy),
            attention: parse_hex(theme.attention),
            critical: parse_hex(theme.critical),
        }
    }
}

impl Default for UiTheme {
    fn default() -> Self {
        Self::from_contract(&DARK)
    }
}

/// Resolve a theme by case-insensitive name. Accepts `dark`, `light`,
/// `high-contrast`, or `high_contrast` (the underscore form matches
/// the contract's `ThemeName::HighContrast` casing for users who copy
/// it from documentation). Unknown values fall back to Dark — the
/// `--theme` clap parser rejects bad input earlier, but the config
/// path is more permissive (operators sometimes type freehand), so a
/// silent fallback keeps the TUI usable when a TOML typo would
/// otherwise refuse to launch.
pub fn current_theme(name: &str) -> UiTheme {
    UiTheme::from_contract(resolve_contract(name))
}

/// String → contract `Theme` resolver. Exposed (`pub(crate)`) so the
/// CLI/config wiring can validate names without round-tripping
/// through the ratatui conversion.
pub(crate) fn resolve_contract(name: &str) -> &'static Theme {
    match name.to_ascii_lowercase().replace('_', "-").as_str() {
        "light" => &LIGHT,
        "high-contrast" => &HIGH_CONTRAST,
        _ => &DARK,
    }
}

/// Parse `#RRGGBB` into `Color::Rgb`. The contract's hex strings are
/// compile-time constants validated by `ux_contract`'s own tests, so
/// in production this never sees malformed input — the explicit
/// fallback is a deterministic safety net for hand-edited config and
/// for tests that pass synthetic palettes.
fn parse_hex(hex: &str) -> Color {
    let s = hex.trim().trim_start_matches('#');
    if s.len() != 6 {
        return Color::Reset;
    }
    let Ok(r) = u8::from_str_radix(&s[0..2], 16) else {
        return Color::Reset;
    };
    let Ok(g) = u8::from_str_radix(&s[2..4], 16) else {
        return Color::Reset;
    };
    let Ok(b) = u8::from_str_radix(&s[4..6], 16) else {
        return Color::Reset;
    };
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_basic() {
        assert_eq!(parse_hex("#000000"), Color::Rgb(0, 0, 0));
        assert_eq!(parse_hex("#ffffff"), Color::Rgb(255, 255, 255));
        assert_eq!(parse_hex("#1a1b26"), Color::Rgb(0x1a, 0x1b, 0x26));
    }

    #[test]
    fn parse_hex_strips_hash_and_whitespace() {
        assert_eq!(parse_hex("#7aa2f7"), Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(parse_hex(" #7aa2f7 "), Color::Rgb(0x7a, 0xa2, 0xf7));
    }

    #[test]
    fn parse_hex_is_case_insensitive_on_digits() {
        // The contract uses lowercase hex; accept either so a
        // hand-edited theme constant doesn't silently fall back.
        assert_eq!(parse_hex("#ABCDEF"), Color::Rgb(0xab, 0xcd, 0xef));
    }

    #[test]
    fn parse_hex_malformed_falls_back_to_reset() {
        assert_eq!(parse_hex(""), Color::Reset);
        assert_eq!(parse_hex("#zzz"), Color::Reset);
        assert_eq!(parse_hex("not-hex"), Color::Reset);
        assert_eq!(parse_hex("#12345"), Color::Reset); // 5 chars
        assert_eq!(parse_hex("#1234567"), Color::Reset); // 7 chars
    }

    #[test]
    fn current_theme_dark() {
        let t = current_theme("dark");
        assert_eq!(t.name, ThemeName::Dark);
        assert_eq!(t.background, Color::Rgb(0x1a, 0x1b, 0x26));
        assert_eq!(t.foreground, Color::Rgb(0xc0, 0xca, 0xf5));
        assert_eq!(t.accent, Color::Rgb(0x7a, 0xa2, 0xf7));
    }

    #[test]
    fn current_theme_light() {
        let t = current_theme("light");
        assert_eq!(t.name, ThemeName::Light);
        assert_eq!(t.background, Color::Rgb(0xe6, 0xe2, 0xcf));
        assert_eq!(t.foreground, Color::Rgb(0x2c, 0x2c, 0x2a));
    }

    #[test]
    fn current_theme_high_contrast_dash_or_underscore() {
        let dash = current_theme("high-contrast");
        let under = current_theme("high_contrast");
        assert_eq!(dash.name, ThemeName::HighContrast);
        assert_eq!(under.name, ThemeName::HighContrast);
        assert_eq!(dash.background, Color::Rgb(0, 0, 0));
        assert_eq!(dash.foreground, Color::Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn current_theme_is_case_insensitive() {
        assert_eq!(current_theme("DARK").name, ThemeName::Dark);
        assert_eq!(current_theme("Light").name, ThemeName::Light);
        assert_eq!(current_theme("High_Contrast").name, ThemeName::HighContrast);
    }

    #[test]
    fn current_theme_unknown_falls_back_to_dark() {
        assert_eq!(current_theme("solarized").name, ThemeName::Dark);
        assert_eq!(current_theme("").name, ThemeName::Dark);
    }

    #[test]
    fn three_themes_have_distinct_backgrounds() {
        let dark = current_theme("dark").background;
        let light = current_theme("light").background;
        let hc = current_theme("high-contrast").background;
        assert_ne!(dark, light);
        assert_ne!(light, hc);
        assert_ne!(dark, hc);
    }

    #[test]
    fn three_themes_have_distinct_foregrounds() {
        let dark = current_theme("dark").foreground;
        let light = current_theme("light").foreground;
        let hc = current_theme("high-contrast").foreground;
        assert_ne!(dark, light);
        assert_ne!(light, hc);
        assert_ne!(dark, hc);
    }

    #[test]
    fn default_is_dark() {
        // §13 default — Dark. Wire-protocol level guarantee for any
        // path that constructs `UiTheme` without a name (e.g. tests
        // that don't care about theme).
        assert_eq!(UiTheme::default().name, ThemeName::Dark);
    }
}
