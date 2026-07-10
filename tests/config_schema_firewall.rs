//! v1.3.1 / DISPATCH 60 / C1 — config-schema authority-lock guard.
//!
//! ## What this pins
//!
//! The `[[workloads]]` rule schema (when it lands in v1.3.2) and the
//! existing config / threshold sections must remain a
//! **discriminator, not a callable**. The `INSPECTOR_PHASE4_IMPL.md`
//! §8 audit confirmed every Phase 4 element is observation-side; the
//! schema-level firewall is the strongest expression of that lock —
//! even an operator editing TOML cannot configure auto-action because
//! the field doesn't exist.
//!
//! Two complementary firewalls already pin observe-only in code:
//!
//!     Firewall 1 (TYPE):   ux_contract::SuggestedAction is Copy
//!                          (pinned by `suggested_action_is_copy`)
//!     Firewall 2 (WIRING): tests/recommendation_observe_only_guard.rs
//!                          (no actuation imports in src/recommend.rs)
//!     Firewall 3 (SCHEMA): THIS TEST — no action-verb fields in the
//!                          config schema source
//!
//! Until this commit, Firewall 3 was documentary-only — the schema
//! happened to lack action-verb fields because nobody wrote them.
//! `docs/PHASE4_DESIGN.md` §2 and §4 enumerate which fields are
//! deliberately absent (`action_on_breach`, `auto_kill`, `priority`,
//! …) but enforcement lived in human review. This test makes
//! Firewall 3 CI-enforced: a future contributor who adds
//! `pub action_on_breach: …` to `Config`/`ThresholdsConfig`/
//! `PolicyConfig`/`WorkloadRule` breaks this test before they break
//! production. The diff is visible; the operator-signed boundary
//! stays inviolate.
//!
//! ## What's forbidden
//!
//! Any of the following **literal field-name tokens** appearing as
//! Rust code (i.e. NOT inside a comment or string literal) in the
//! config schema source files:
//!
//! - `action_on_breach` — the canonical "on breach do X" field name
//! - `enforce_kill` — verb-as-bool gate on a kill action
//! - `kill_when` — predicate-pattern actuation gate
//! - `auto_kill` — boolean automation gate (distinct from the
//!   ALLOWED `auto_actuate` — see below)
//! - `on_breach` — short form of `action_on_breach`
//! - `then_kill` — declarative actuation arm
//!
//! **`auto_actuate` is DELIBERATELY ALLOWED** as a noun-shaped opt-in
//! gate. It names a boolean that GATES a future actuation site; it
//! is not itself an action verb. The dispatch-60 step-2 commit adds
//! `governor.auto_actuate: bool` (default false) as the named gate.
//! When the future actuation site lands, it reads this gate but the
//! field itself remains observation-side.
//!
//! ## What's scanned
//!
//! Currently: `src/config.rs` (the home of `Config`, `PolicyConfig`,
//! `ThresholdsConfig`, all existing schema). When v1.3.2 lands
//! `WorkloadRule` (and if it lives outside `src/config.rs`),
//! `SCHEMA_PATHS` extends to cover the new file. The scanner is path-
//! generic; updating the list is a one-line change.
//!
//! Comments and string literals are stripped before scanning so the
//! `//!` discussion of forbidden fields in `docs/PHASE4_DESIGN.md`
//! references (and in this test) does NOT trip the guard.

use std::fs;
use std::path::PathBuf;

/// Schema source files scanned by this guard.
///
/// v1.3.2 / DISPATCH 108 ITEM 3 — extended from the pre-D108
/// single-entry list. The completeness pin
/// `schema_paths_covers_every_schema_defining_file` walks `src/`,
/// finds every file that defines a schema-shaped struct
/// (Deserialize + a `*Config` name or a known schema type), and
/// asserts each is registered here. Broadening the coverage (vs.
/// narrowing the pin) is the safer robustness call — the
/// forbidden-token scan on a file that has no forbidden tokens is
/// a no-op; the scan on a file that WOULD have grown one is the
/// win. `src/config.rs` remains the load-bearing entry (the TOML
/// root); the others are Config-suffixed structs that participate
/// in serialization surfaces.
const SCHEMA_PATHS: &[&str] = &[
    "src/config.rs",
    "src/analysis/compare.rs",
    "src/storage/run_store.rs",
    "src/web/settings.rs",
];

/// Names that, if they appear as Rust code tokens (i.e. NOT inside a
/// comment or string literal) in any [`SCHEMA_PATHS`] file, indicate
/// the schema-level authority lock has been breached. See module
/// docs for the rationale on each entry and the deliberate exclusion
/// of `auto_actuate`.
const FORBIDDEN_TOKENS: &[&str] = &[
    "action_on_breach",
    "enforce_kill",
    "kill_when",
    "auto_kill",
    "on_breach",
    "then_kill",
];

/// v1.3.2 / DISPATCH 108 ITEM 3 — completeness pin for
/// `SCHEMA_PATHS`. Pre-D108 the list was a hardcoded single-entry
/// array; if the schema split into multiple files (a scenario the
/// module doc-comment explicitly anticipates), a future author
/// forgetting to extend the list would silently miss forbidden-
/// token drift in the new file. Robustness fix: walk `src/`, find
/// every `.rs` file that DEFINES a schema-shaped Rust struct
/// (`#[derive(...Deserialize...)]` + `pub struct <Name>Config` or a
/// known schema type name like `WorkloadRule`), and assert every
/// such file is in `SCHEMA_PATHS`. If a new schema-bearing file
/// appears without being registered, this test fires with a clear
/// "extend `SCHEMA_PATHS` to cover N" message.
///
/// This does NOT weaken the firewall — the forbidden-token scan
/// still runs against everything in `SCHEMA_PATHS`. This test is a
/// SEPARATE robustness pin that keeps the LIST honest.
#[test]
fn schema_paths_covers_every_schema_defining_file() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut discovered: Vec<String> = Vec::new();
    walk_rs_dir(&src, &src, &mut discovered);
    // A "schema-shaped" file: contains a `#[derive(...Deserialize...)]`
    // attribute AND a `pub struct <Name>Config` OR a known-schema
    // struct name. Deserialize-only structs (e.g. wire types) are
    // NOT schema; they don't drive TOML config parsing.
    let known_schema_names: &[&str] = &["WorkloadRule"];
    let mut schema_files: Vec<String> = Vec::new();
    for rel_path in &discovered {
        let source = match fs::read_to_string(root.join(rel_path)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Cheap heuristic — parse-free. A false positive here means
        // an already-scanned file is required to appear in
        // SCHEMA_PATHS, which fails LOUD (the operator adds it or
        // adjusts the heuristic).
        if !source.contains("Deserialize") {
            continue;
        }
        let has_config_struct = source.contains("pub struct ")
            && (source.contains("Config {") || source.contains("Config<"));
        let has_known_schema = known_schema_names
            .iter()
            .any(|n| source.contains(&format!("pub struct {n}")));
        if has_config_struct || has_known_schema {
            schema_files.push(rel_path.clone());
        }
    }
    let known: std::collections::HashSet<&&str> = SCHEMA_PATHS.iter().collect();
    let missing: Vec<&String> = schema_files
        .iter()
        .filter(|p| !known.contains(&p.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "SCHEMA_PATHS is out of date — the following files define \
         schema-shaped structs (Deserialize + a `*Config` struct or a \
         known schema type name) but are NOT registered:\n{}\n\
         Extend `SCHEMA_PATHS` in tests/config_schema_firewall.rs so \
         the forbidden-token scan covers them; a silently-uncovered \
         schema file is exactly the 62-G fragility this pin closes.",
        missing
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Walk `dir` recursively and push relative paths (relative to
/// `src_root`) of every `.rs` file into `out`. Skips hidden dirs
/// and target/ paths for hygiene; the whole walk stays inside
/// `src/` so those exclusions are defensive not load-bearing.
fn walk_rs_dir(src_root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs_dir(src_root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
            && let Ok(rel) = path.strip_prefix(src_root)
        {
            // Store as "src/<rel>" so it matches SCHEMA_PATHS entries.
            out.push(format!("src/{}", rel.display()));
        }
    }
}

#[test]
fn config_schema_has_no_action_verb_fields() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations: Vec<(String, usize, &'static str)> = Vec::new();

    // D108 ITEM 3 — additionally assert each SCHEMA_PATHS entry
    // exists on disk. The pre-D108 code panicked with a helpful
    // message via `.unwrap_or_else`, but only if the entry was
    // read; a typoed entry that resolved to a nonexistent path
    // still surfaced there. Loop entry: same behavior, kept
    // explicit for clarity.
    for rel in SCHEMA_PATHS {
        let path = root.join(rel);
        let source = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "failed to read {} (the schema firewall cannot \
                 enforce the boundary if a schema file moved or \
                 was deleted): {e}",
                path.display(),
            )
        });

        let code_only = strip_comments_and_strings(&source);
        for (lineno, line) in code_only.lines().enumerate() {
            for tok in FORBIDDEN_TOKENS {
                if line.contains(tok) {
                    violations.push(((*rel).to_string(), lineno + 1, tok));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "authority lock breached: the config schema must remain a \
         discriminator (no action-verb fields). `docs/PHASE4_DESIGN.md` \
         §2 and §4 enumerate the deliberately-absent fields; \
         `auto_actuate` (added in DISPATCH 60 step 2) is the ONLY \
         allowed boolean gate name, and it is not an action verb. \
         Found:\n{}",
        violations
            .iter()
            .map(|(p, l, t)| format!("  {p}:{l}: forbidden field-name token `{t}`"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn guard_would_catch_a_real_breach() {
    // Synthesize a schema source string with each forbidden token
    // present at a code position. Run the same scan. This is the
    // negative direction the lock-as-test pattern needs — without
    // it, a bug in the scanner could silently let the guard pass
    // forever.
    let synthetic = r#"
//! Doc mentions action_on_breach — this is fine.
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct WorkloadRule {
    pub name: String,
    pub action_on_breach: Option<String>,
    pub enforce_kill: bool,
    pub kill_when: Option<String>,
    pub auto_kill: bool,
    pub on_breach: Option<String>,
    pub then_kill: bool,
}
"#;
    let code_only = strip_comments_and_strings(synthetic);
    let mut caught: Vec<&'static str> = Vec::new();
    for line in code_only.lines() {
        for tok in FORBIDDEN_TOKENS {
            if line.contains(tok) {
                caught.push(tok);
            }
        }
    }
    // Every forbidden token must be detected by the scanner. If any
    // are missed, the FORBIDDEN_TOKENS list and the scanner are out
    // of sync.
    for tok in FORBIDDEN_TOKENS {
        assert!(
            caught.contains(tok),
            "scanner missed `{tok}` in synthetic breach — the lock \
             guard would be a no-op against real drift. caught: \
             {caught:?}",
        );
    }
}

#[test]
fn auto_actuate_is_not_flagged() {
    // Twin to the negative test: confirm that the DELIBERATELY
    // allowed `auto_actuate` field name does NOT trip any forbidden
    // token, even when adjacent to action-verb prose in comments.
    // This pins the C2 step-2 contract: `governor.auto_actuate` is
    // the named opt-in gate.
    let synthetic = r#"
/// `auto_actuate` gates the future actuation site (DISPATCH 60+
/// step 5). Default false preserves the v1.0.1 phantom-kill scar.
#[derive(Debug, Default)]
pub struct GovernorConfig {
    pub auto_actuate: bool,
}
"#;
    let code_only = strip_comments_and_strings(synthetic);
    let mut violations: Vec<&'static str> = Vec::new();
    for line in code_only.lines() {
        for tok in FORBIDDEN_TOKENS {
            if line.contains(tok) {
                violations.push(tok);
            }
        }
    }
    assert!(
        violations.is_empty(),
        "`auto_actuate` must NOT match any forbidden token. If this \
         fails, FORBIDDEN_TOKENS includes a substring of `auto_actuate` \
         — the list is wrong; do NOT rename `auto_actuate`. Violations: \
         {violations:?}",
    );
}

#[test]
fn comment_stripper_does_not_clobber_code() {
    // Self-test for the stripper: a forbidden token inside a comment
    // or string must be stripped, but a real code token must survive.
    let src = r#"
// auto_kill here is in a comment, should be stripped
let x = "auto_kill in string literal";
fn run() { let auto_kill = true; let _ = auto_kill; }
/* block comment with action_on_breach inside */
"#;
    let stripped = strip_comments_and_strings(src);
    // The code occurrence survives (one in `let auto_kill = true;`,
    // one in `let _ = auto_kill;` → two `auto_kill` tokens).
    assert!(
        stripped.contains("auto_kill"),
        "code token must survive stripping: {stripped}",
    );
    assert_eq!(
        stripped.matches("auto_kill").count(),
        2,
        "comments/strings holding `auto_kill` must be stripped while \
         the two code occurrences survive: {stripped}",
    );
    // Block-comment occurrence stripped.
    assert!(
        !stripped.contains("action_on_breach"),
        "block-comment token must be stripped: {stripped}",
    );
}

/// Strip line + block comments and quoted strings from `source`,
/// preserving line numbers (each stripped span is replaced with
/// spaces so newlines stay aligned). Forbidden tokens that appear in
/// docstrings or string literals are allowed; only code tokens trip
/// the guard.
///
/// Hand-written, not a full Rust tokenizer. Handles the cases that
/// appear in `src/config.rs`: `//`, `/* */`, `"..."`, `r"..."`, and
/// `r#"..."#`. Mirrors the stripper in
/// `tests/recommendation_observe_only_guard.rs` so a fix in either
/// place can be ported directly. (Future tightening: factor the
/// stripper into a shared `tests/common/` module if a third guard
/// arrives.)
fn strip_comments_and_strings(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // Line comment
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        // Block comment
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            out.push(' ');
            out.push(' ');
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
                i += 1;
            }
            if i + 1 < bytes.len() {
                out.push(' ');
                out.push(' ');
                i += 2;
            }
            continue;
        }
        // Raw string r"..." / r#"..."# / r##"..."##
        if c == b'r' && i + 1 < bytes.len() && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') {
            out.push(' ');
            i += 1;
            let mut hashes = 0;
            while i < bytes.len() && bytes[i] == b'#' {
                hashes += 1;
                out.push(' ');
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                out.push(' ');
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        let mut closes = 0;
                        let mut j = i + 1;
                        while j < bytes.len() && bytes[j] == b'#' && closes < hashes {
                            closes += 1;
                            j += 1;
                        }
                        if closes == hashes {
                            for _ in 0..=closes {
                                out.push(' ');
                            }
                            i = j;
                            break;
                        }
                    }
                    out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            continue;
        }
        // Plain string "..."
        if c == b'"' {
            out.push(' ');
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
                i += 1;
            }
            if i < bytes.len() {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}
