//! L4 / UX_CONTRACT.md §15 — terminal-encoding detection.
//!
//! At startup we resolve a single `SymbolSet` (Unicode or ASCII) by
//! reading the process locale environment, and the TUI uses that
//! choice for the entire session. v0.3 §15 calls out three real
//! environments where the Unicode block / box-drawing characters
//! fail: older Windows ConHost, minimal SSH sessions, and `tmux`
//! with a broken `LANG`. The Linux binary handles the second and
//! third cases via `LC_ALL` / `LC_CTYPE` / `LANG`; the Windows
//! sibling crate handles the ConHost case separately.
//!
//! **Once-at-startup, never re-evaluated.** Per the contract, a user
//! who opens the TUI in a non-UTF-8 terminal and then resizes /
//! reconnects keeps the same symbol set for the remaining session.
//! Re-detection on every render would mask configuration mistakes
//! and add per-frame env-var lookups for no benefit.

use ux_contract::WorkloadStatus;

/// Whether the terminal supports the Unicode block characters used by
/// UX_CONTRACT.md §15 (`●⚠✕○`, sparkline blocks, box-drawing). Resolved
/// once at startup via [`detect`] and stored on `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymbolSet {
    /// `●⚠✕○` — the contract's preferred Unicode glyphs. Default for
    /// modern terminals.
    #[default]
    Unicode,
    /// `*!Xo` — ASCII fallback. Fires when the terminal advertises a
    /// non-UTF-8 locale (e.g. `LANG=C`, `LANG=POSIX`, or a stripped
    /// SSH session with no locale at all).
    Ascii,
}

impl SymbolSet {
    /// Resolve a status dot to the right glyph for this session.
    ///
    /// Render sites must route through this method (or one of the
    /// other accessors on `SymbolSet`) rather than calling
    /// [`WorkloadStatus::symbol`] / [`WorkloadStatus::symbol_ascii`]
    /// directly — otherwise a session running on a `LANG=C` SSH box
    /// would render boxes-and-question-marks for the dots while the
    /// rest of the screen used ASCII fallbacks.
    pub fn workload_status(self, status: WorkloadStatus) -> &'static str {
        match self {
            SymbolSet::Unicode => status.symbol(),
            SymbolSet::Ascii => status.symbol_ascii(),
        }
    }

    /// Mission-line separator glyph (UX_CONTRACT.md §0). Unicode is
    /// `·` (U+00B7 middle dot), the contract's locked separator;
    /// ASCII fallback is `-` so the line stays readable on `LANG=C`
    /// SSH sessions. Routed through the same `SymbolSet` the rest of
    /// the TUI uses so a session that fell back to Ascii at startup
    /// gets a coherent look top-to-bottom.
    pub fn header_separator(self) -> &'static str {
        match self {
            SymbolSet::Unicode => "·",
            SymbolSet::Ascii => "-",
        }
    }
}

/// Resolve the terminal's symbol capability from the process locale.
///
/// One-shot; call once at TUI startup and store the result on `App`.
/// Reads `LC_ALL`, `LC_CTYPE`, then `LANG` in POSIX precedence
/// order.
pub fn detect() -> SymbolSet {
    detect_from(|var| std::env::var(var).ok())
}

/// Pure detection: takes an env-var getter so tests can inject any
/// environment without mutating the process state.
///
/// Decision rules (in order):
/// 1. The first non-empty value wins (POSIX precedence — explicit
///    locale settings override later fallbacks even when they don't
///    name UTF-8).
/// 2. If that value mentions `utf8` or `utf-8` (case-insensitive),
///    `Unicode`. Otherwise `Ascii` — the user is explicitly on a
///    legacy locale.
/// 3. With no locale variables set, default to `Unicode`. Modern
///    terminals leave `LANG` unset only on stripped containers, and
///    those almost always still render UTF-8 correctly; ASCII
///    fallback would be the wrong default for them.
pub fn detect_from<F>(getter: F) -> SymbolSet
where
    F: Fn(&str) -> Option<String>,
{
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(val) = getter(var)
            && !val.is_empty()
        {
            return if mentions_utf8(&val) {
                SymbolSet::Unicode
            } else {
                SymbolSet::Ascii
            };
        }
    }
    SymbolSet::Unicode
}

fn mentions_utf8(locale: &str) -> bool {
    let normalized = locale.to_ascii_lowercase();
    normalized.contains("utf-8") || normalized.contains("utf8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |var: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == var)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn detects_unicode_from_lang_utf8() {
        assert_eq!(
            detect_from(env(&[("LANG", "en_US.UTF-8")])),
            SymbolSet::Unicode
        );
    }

    #[test]
    fn detects_unicode_from_lc_ctype_utf8_normalised() {
        // Common alternate spellings the locale machinery hands back.
        for value in ["en_US.utf8", "C.UTF-8", "en_GB.UTF-8@euro"] {
            assert_eq!(
                detect_from(env(&[("LC_CTYPE", value)])),
                SymbolSet::Unicode,
                "value {value:?} should resolve to Unicode"
            );
        }
    }

    #[test]
    fn detects_ascii_from_lang_c() {
        // The classic non-UTF-8 locales — what stripped containers
        // and minimal SSH bastions look like.
        for value in ["C", "POSIX", "en_US"] {
            assert_eq!(
                detect_from(env(&[("LANG", value)])),
                SymbolSet::Ascii,
                "value {value:?} should resolve to Ascii"
            );
        }
    }

    #[test]
    fn lc_all_takes_precedence_over_lang() {
        // POSIX precedence: LC_ALL wins even when LANG says UTF-8.
        let resolved = detect_from(env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]));
        assert_eq!(resolved, SymbolSet::Ascii);
    }

    #[test]
    fn empty_lc_all_falls_through_to_lc_ctype() {
        // An empty LC_ALL is treated as unset (POSIX) — the next
        // variable in precedence wins.
        let resolved = detect_from(env(&[
            ("LC_ALL", ""),
            ("LC_CTYPE", "en_US.UTF-8"),
            ("LANG", "C"),
        ]));
        assert_eq!(resolved, SymbolSet::Unicode);
    }

    #[test]
    fn no_locale_set_defaults_to_unicode() {
        // Stripped container with no LANG/LC_ALL/LC_CTYPE at all.
        // Defaulting to Ascii here would punish modern terminals that
        // happen to be missing a locale config; Unicode is the safer
        // default.
        let resolved = detect_from(env(&[]));
        assert_eq!(resolved, SymbolSet::Unicode);
    }

    #[test]
    fn workload_status_unicode_returns_contract_glyphs() {
        let s = SymbolSet::Unicode;
        assert_eq!(s.workload_status(WorkloadStatus::Healthy), "●");
        assert_eq!(s.workload_status(WorkloadStatus::Attention), "⚠");
        assert_eq!(s.workload_status(WorkloadStatus::Critical), "✕");
        assert_eq!(s.workload_status(WorkloadStatus::Loading), "○");
    }

    #[test]
    fn workload_status_ascii_returns_fallback_glyphs() {
        let s = SymbolSet::Ascii;
        assert_eq!(s.workload_status(WorkloadStatus::Healthy), "*");
        assert_eq!(s.workload_status(WorkloadStatus::Attention), "!");
        assert_eq!(s.workload_status(WorkloadStatus::Critical), "X");
        assert_eq!(s.workload_status(WorkloadStatus::Loading), "o");
    }

    #[test]
    fn header_separator_unicode_is_middle_dot() {
        assert_eq!(SymbolSet::Unicode.header_separator(), "·");
    }

    #[test]
    fn header_separator_ascii_is_hyphen_and_ascii_clean() {
        let sep = SymbolSet::Ascii.header_separator();
        assert_eq!(sep, "-");
        assert!(sep.is_ascii(), "ASCII fallback separator must be ASCII");
    }

    #[test]
    fn ascii_fallback_glyphs_are_actually_ascii() {
        // Defensive: if a future contract amendment sneaks a
        // non-ASCII character into the "ASCII" fallback set
        // (`symbol_ascii`), this test catches it before it ships.
        let s = SymbolSet::Ascii;
        for status in [
            WorkloadStatus::Healthy,
            WorkloadStatus::Attention,
            WorkloadStatus::Critical,
            WorkloadStatus::Loading,
        ] {
            let glyph = s.workload_status(status);
            assert!(
                glyph.is_ascii(),
                "ASCII fallback for {status:?} contains non-ASCII bytes: {glyph:?}"
            );
        }
    }
}
