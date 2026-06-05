//! v1.3.2 / DISPATCH 57 C2 — field-count guard on `WorkloadRule`.
//!
//! ## What this pins
//!
//! `WorkloadRule` is the per-workload schema for the `[[workloads]]`
//! TOML array. The schema is intentionally minimal: exactly THREE
//! fields, all suppress-flag nouns. This test pins the field count
//! at 3 so a contributor who adds a fourth field — even a benign
//! one — has to update this test deliberately. The intent is to
//! force a pause-and-think when growing the schema, because any
//! growth needs to clear two complementary guards:
//!
//!   1. `tests/config_schema_firewall.rs` (DISPATCH 60 C1) catches
//!      action-VERB field names (`auto_kill`, `action_on_breach`,
//!      …). It can't catch benign-shaped additions like
//!      `display_name` or `category_override`.
//!   2. THIS GUARD catches *any* schema growth, action-shaped or
//!      not. The forcing function is the field count, not the
//!      field name.
//!
//! Together: even an editor with the kindest intent who adds
//! `pub display_name: Option<String>` to `WorkloadRule` will
//! break this test. They can then either (a) acknowledge the
//! schema is growing and update the guard with a comment
//! explaining the deliberate decision, or (b) realise the field
//! belongs elsewhere. Without this guard, schema drift can land
//! silently because the action-firewall is field-NAME-based.
//!
//! `docs/PHASE4_DESIGN.md` §3 Q4 LOCKED that v1.3.2 ships ONLY
//! the suppress flags; additions like `display_name` /
//! `category_override` are explicitly deferred to v1.4.x with a
//! separate operator decision. This guard enforces that lock.
//!
//! ## How
//!
//! We can't use reflection in Rust at test time without macros, so
//! this test counts the field-declaration lines in the actual
//! `WorkloadRule` struct source by reading `src/config.rs`,
//! locating the struct, and counting `pub <name>:` lines until the
//! struct closes. Whitespace + comments don't trip the counter.
//!
//! The token-stripping helper mirrors the one in
//! `tests/recommendation_observe_only_guard.rs` and
//! `tests/config_schema_firewall.rs` so a future refactor that
//! shares them lives at one site.

use std::fs;
use std::path::PathBuf;

/// LOCKED field count. Bump deliberately if and only if an
/// operator-side decision opens additional `[[workloads]]` shape
/// (see `docs/PHASE4_DESIGN.md` §3 Q4 → v1.4.x).
const WORKLOAD_RULE_FIELD_COUNT: usize = 3;

#[test]
fn workload_rule_has_exactly_three_fields() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("src/config.rs");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("failed to read src/config.rs: {e}");
    });
    let code = strip_comments_and_strings(&source);
    let count = count_struct_fields(&code, "WorkloadRule").unwrap_or_else(|| {
        panic!(
            "could not locate `pub struct WorkloadRule {{ ... }}` in \
             src/config.rs — did the struct move? The guard cannot \
             enforce a count on a struct it can't find.",
        );
    });
    assert_eq!(
        count, WORKLOAD_RULE_FIELD_COUNT,
        "WorkloadRule field count drift: docs/PHASE4_DESIGN.md §3 Q4 \
         LOCKED v1.3.2 at exactly 3 suppress-flag fields. If you are \
         deliberately growing the schema, (a) update this constant, \
         (b) check `tests/config_schema_firewall.rs` against the new \
         field's name, AND (c) note the change in CHANGELOG + \
         docs/PHASE4_DESIGN.md.",
    );
}

#[test]
fn counter_finds_known_fields() {
    // Self-test the counter against a synthetic source: 4 pub
    // fields, 1 private, 1 commented-out, 1 in a doc block. Only
    // the 4 pub fields should count.
    let synthetic = r#"
pub struct WorkloadRule {
    /// pub doc_field: String,   <-- in a comment, ignored
    pub name: String,
    // pub commented_out: bool,
    pub suppress_alerts: bool,
    pub suppress_recommendations: bool,
    pub display_name: Option<String>,
    private_field: u32,
}
"#;
    let code = strip_comments_and_strings(synthetic);
    let n = count_struct_fields(&code, "WorkloadRule").unwrap();
    assert_eq!(
        n, 4,
        "synthetic source has 4 pub fields; counter must agree. \
         Code-after-strip:\n{code}",
    );
}

#[test]
fn counter_returns_none_when_struct_absent() {
    let synthetic = r#"
pub struct Something {
    pub a: String,
}
"#;
    let code = strip_comments_and_strings(synthetic);
    assert!(
        count_struct_fields(&code, "WorkloadRule").is_none(),
        "counter must return None for an absent struct so the main \
         test can panic with a clear 'struct moved' message rather \
         than silently passing.",
    );
}

/// Count `pub <ident>:` field declarations inside the named
/// struct's `{ ... }` block. Stops at the matching closing brace.
/// Returns `None` if the struct is not found.
fn count_struct_fields(code: &str, struct_name: &str) -> Option<usize> {
    let needle = format!("pub struct {struct_name}");
    let start = code.find(&needle)?;
    // Find the opening `{` after the struct name.
    let after = &code[start..];
    let brace = after.find('{')?;
    let mut depth = 1usize;
    let body_start = start + brace + 1;
    let bytes = code.as_bytes();
    let mut i = body_start;
    let mut body_end = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = i;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    let body = &code[body_start..body_end];
    // Count lines starting (after optional whitespace) with
    // `pub ` and containing `:`. Private fields and tuple fields
    // are excluded — the schema discipline requires every
    // serde-public field to be `pub`.
    let mut count = 0;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("pub ")
            && rest.contains(':')
            && !rest.starts_with("fn ")
            && !rest.starts_with("struct ")
            && !rest.starts_with("enum ")
        {
            count += 1;
        }
    }
    Some(count)
}

/// Strip line + block comments and quoted strings — mirror of the
/// same helper in `recommendation_observe_only_guard.rs` /
/// `config_schema_firewall.rs`. See module docs for the
/// shared-helper note.
fn strip_comments_and_strings(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
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
