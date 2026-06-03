//! Guard test for L1 (UX_CONTRACT.md §7).
//!
//! Every user-visible string literal that flows into a ratatui rendering
//! position must come from the `ux_contract` crate, not be hardcoded
//! inline. This test walks `src/ui/` and rejects bare string literals
//! at these positions:
//!
//!     Paragraph::new(...), Span::raw(...), Span::styled(...),
//!     Line::raw(...), Line::from(...), Spans::from(...), Cell::from(...)
//!
//! `DEFERRED_FILES` lists files whose migration is queued for a later
//! L-row PR. Each later row removes its file(s) from this list as part
//! of the diff. By the end of L26 the list should be empty.
//!
//! Limitation: literals nested inside `format!(...)` are not parsed by
//! this test (the immediate first argument is checked, not the format
//! string inside a wrapping macro). Future tightening can extend the
//! parser; the orchestrator scoped this guard at ~80 lines.

use std::fs;
use std::path::{Path, PathBuf};

const RENDER_CALL_PATTERNS: &[&str] = &[
    "Paragraph::new(",
    "Span::raw(",
    "Span::styled(",
    "Line::raw(",
    "Line::from(",
    "Spans::from(",
    "Cell::from(",
];

/// Files queued for migration by later L-row PRs. When a row migrates a
/// file's render-position strings to `ux_contract::*`, drop that file
/// from this list as part of the row's diff.
const DEFERRED_FILES: &[&str] = &[
    "src/ui/panels/mod.rs",          // L25 (header) + L25 (footer keymap)
    "src/ui/panels/help.rs",         // CAR-1 resolved in v0.3.2 (help::* module)
    "src/ui/panels/history_overlay.rs", // CAR-6: overlay header + column labels not in v0.3
    "src/ui/panels/postmortem.rs",   // L16 split + CAR-2 resolved in v0.3.2 (postmortem_labels::*)
    "src/ui/panels/vitals.rs",       // L11 (rename to System)
];

const SEPARATOR_ALLOWLIST: &[&str] = &[
    "", " ", "  ", "   ", "·", "—", "|", ":", "/", ",", "(", ")", "[", "]", " · ", " — ", " | ",
    // v1.2.0 / DISPATCH 45 — sub-bullet decoration used by the
    // alerts panel to attach a recommendation line under its
    // parent alert banner. Decoration glyph on par with `·` /
    // `—`; the actual rec text comes from `ux_contract::
    // recommendation::display::*`, and the disclaimer comes from
    // `RECOMMENDATION_NOT_ACTIONABLE`.
    "↳", "  ↳ ",
];

#[test]
fn render_positions_use_ux_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations: Vec<String> = Vec::new();
    walk_rs(&root.join("src/ui"), &root, &mut |path, rel| {
        if DEFERRED_FILES.contains(&rel) {
            return;
        }
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return,
        };
        for (i, line) in text.lines().enumerate() {
            for pat in RENDER_CALL_PATTERNS {
                let mut cursor = 0;
                while let Some(off) = line[cursor..].find(pat) {
                    let after = &line[cursor + off + pat.len()..];
                    if let Some(lit) = extract_string_literal(after)
                        && !literal_is_allowed(&lit)
                    {
                        violations.push(format!(
                            "{}:{}: hardcoded literal {:?} in render position `{}` — \
                             use a `ux_contract::*` const instead, or file a Contract \
                             Amendment Request if the string isn't covered by v0.3",
                            rel,
                            i + 1,
                            lit,
                            pat.trim_end_matches('('),
                        ));
                    }
                    cursor += off + pat.len();
                }
            }
        }
    });
    assert!(
        violations.is_empty(),
        "found {} hardcoded user-visible string literal(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

/// Extract a `"..."` literal at the start of `after` (after optional
/// whitespace). Returns the unescaped body, or None if the next non-
/// whitespace token isn't a `"`. Handles `\"` and `\\` escapes; treats
/// any other backslash escape as a literal pair (good enough for copy).
fn extract_string_literal(after: &str) -> Option<String> {
    // v1.2.0 / DISPATCH 45 — iterate chars, not bytes. The byte-
    // iterator approach mangled multi-byte UTF-8 glyphs (e.g.
    // `↳`) into `b as char` casts so the allowlist comparison
    // never matched. Chars give the correct semantic boundaries.
    let mut chars = after.chars();
    loop {
        let c = chars.next()?;
        if c == '"' {
            break;
        }
        if !c.is_whitespace() {
            return None;
        }
    }
    let mut out = String::new();
    let mut escape = false;
    for c in chars {
        if escape {
            out.push(c);
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            return Some(out);
        }
        out.push(c);
    }
    None
}

/// Allowlist: empty / all-whitespace / known separator / single
/// format placeholder like `{}`, `{pid}`, or `{:.0}`. Anything else
/// in a render position must come from `ux_contract::*`.
fn literal_is_allowed(s: &str) -> bool {
    if s.is_empty() || s.chars().all(char::is_whitespace) {
        return true;
    }
    if SEPARATOR_ALLOWLIST.contains(&s) {
        return true;
    }
    s.starts_with('{') && s.ends_with('}') && !s[1..s.len() - 1].contains('{')
}

#[test]
fn parser_recognises_violations_and_allowlist() {
    // Self-test: the guard above passes trivially when no non-deferred
    // file has render-position literals. These assertions verify the
    // helpers actually distinguish bad from allowed, so a regression in
    // the parser doesn't silently green-light future drift.
    assert_eq!(
        extract_string_literal(r#""hello world")"#).as_deref(),
        Some("hello world")
    );
    assert_eq!(extract_string_literal("ux_contract::empty::HISTORY)"), None);
    assert_eq!(extract_string_literal(r#"   "spaced")"#).as_deref(), Some("spaced"));
    assert!(literal_is_allowed(""));
    assert!(literal_is_allowed("·"));
    assert!(literal_is_allowed("{pid}"));
    assert!(literal_is_allowed("{:.0}"));
    assert!(!literal_is_allowed("hello world"));
    assert!(!literal_is_allowed("Kill this process? (PID {pid})"));
}

fn walk_rs(dir: &Path, root: &Path, visit: &mut dyn FnMut(&Path, &str)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, root, visit);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(rel) = path.strip_prefix(root)
        {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            visit(&path, &rel_str);
        }
    }
}
