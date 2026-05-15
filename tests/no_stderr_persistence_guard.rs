//! L18 privacy guard. Walks `src/storage/` and fails the build if any
//! field literally named `stderr` or `stderr_*` is added to a struct
//! or enum that derives `Serialize`.
//!
//! Rationale: the on-disk schema owned by `src/storage/` deliberately
//! does not persist process stderr — see the "Privacy stance: no
//! stderr persistence" doc block at the top of
//! `src/storage/run_store.rs`. Live stderr access for the post-mortem
//! card lives on a transient `PostMortem::stderr_tail` buffer in
//! `src/ui/panels/postmortem.rs`, dropped on card dismissal (UI
//! Contract v2 "Stderr is ephemeral").
//!
//! Scope is limited to `src/storage/` on purpose. Stderr in
//! `src/runtime.rs` (the transient `ExitContext`), `src/exit_classify.rs`,
//! and `src/ui/panels/postmortem.rs` is legitimate non-persisted usage
//! and out of scope for this guard.
//!
//! Heuristics, kept deliberately simple (regex over raw source, no
//! `syn`):
//!
//! * test-vs-prod split: once a line starts with `#[cfg(test)]`,
//!   everything below in the same file is treated as test code and
//!   skipped — matches the convention used by `expect_rule_guard.rs`.
//! * `Serialize`-derive tracking: a `#[derive(... Serialize ...)]`
//!   attribute arms the next opening brace of a `struct`/`enum`
//!   declaration. We then track brace depth until the type body
//!   closes, and inside that body every field whose identifier is
//!   exactly `stderr` or matches `stderr_<word>` triggers a violation.
//!
//! If a future feature genuinely needs persistent stderr that is a
//! deliberate schema change: amend UI Contract v2, update the doc
//! block in `run_store.rs`, and adjust or remove this guard with
//! reviewer signoff — do not silently add the field.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

fn rs_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir src/storage/") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension() == Some(OsStr::new("rs")) {
                out.push(path);
            }
        }
    }
    out
}

/// Strip everything from the first `//` on the line so doc-comments
/// and trailing comments cannot mask or fake a field declaration.
/// Naive — does not understand `//` inside string literals — but
/// `src/storage/` does not put `//` inside field positions, so this
/// is sufficient for the guard.
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(p) => &line[..p],
        None => line,
    }
}

/// True if `code` is a struct/enum field declaration whose identifier
/// is exactly `stderr` or begins with `stderr_`. Accepts the
/// `pub`/`pub(crate)`/`pub(super)`/`pub(in path)` visibility prefix
/// and tolerates `#[serde(...)]`-style attributes appearing on a
/// preceding line (we look at the field line itself).
fn is_stderr_field(code: &str) -> bool {
    let after_vis = {
        let trimmed = code.trim_start();
        if let Some(rest) = trimmed.strip_prefix("pub") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('(') {
                match rest.find(')') {
                    Some(p) => rest[p + 1..].trim_start(),
                    None => return false,
                }
            } else {
                rest
            }
        } else {
            trimmed
        }
    };
    let ident_end = after_vis
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(after_vis.len());
    let ident = &after_vis[..ident_end];
    let after_ident = after_vis[ident_end..].trim_start();
    // Field syntax is `name: Type`; reject `name::path` (single `:`
    // followed by anything that isn't another `:` is the marker).
    if !after_ident.starts_with(':') || after_ident.starts_with("::") {
        return false;
    }
    ident == "stderr" || ident.starts_with("stderr_")
}

#[test]
fn no_stderr_field_in_serialize_types_under_storage() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let storage = crate_root.join("src").join("storage");
    assert!(storage.is_dir(), "{:?} must exist", storage);

    let mut violations: Vec<String> = Vec::new();

    for file in rs_files_under(&storage) {
        let body = fs::read_to_string(&file).expect("read source file");
        let lines: Vec<&str> = body.lines().collect();

        let mut in_test = false;
        let mut derive_has_serialize = false;
        let mut in_serialize_block = false;
        let mut depth: i32 = 0;

        for (idx, raw_line) in lines.iter().enumerate() {
            let line = strip_line_comment(raw_line);
            let trimmed = line.trim_start();

            if trimmed.starts_with("#[cfg(test)]") {
                in_test = true;
            }
            if in_test {
                continue;
            }

            // Arm on a `#[derive(... Serialize ...)]` attribute. The
            // codebase keeps derives on a single line, so we don't try
            // to handle multi-line attributes here.
            if trimmed.starts_with("#[derive(") && line.contains("Serialize") {
                derive_has_serialize = true;
            }

            // Detect entry into a `struct`/`enum` body. When a body
            // opens after a Serialize-armed derive, start counting
            // braces so the field scan stops at the closing `}`.
            if !in_serialize_block {
                let is_type_decl = trimmed.starts_with("struct ")
                    || trimmed.starts_with("pub struct ")
                    || trimmed.starts_with("pub(crate) struct ")
                    || trimmed.starts_with("pub(super) struct ")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("pub enum ")
                    || trimmed.starts_with("pub(crate) enum ")
                    || trimmed.starts_with("pub(super) enum ");
                if is_type_decl && line.contains('{') {
                    if derive_has_serialize {
                        in_serialize_block = true;
                        depth = 0;
                    }
                    derive_has_serialize = false;
                }
            }

            if in_serialize_block {
                for ch in line.chars() {
                    match ch {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if is_stderr_field(line) {
                    let rel = file.strip_prefix(&crate_root).unwrap_or(&file);
                    violations.push(format!(
                        "{}:{}: forbidden stderr-named field in Serialize-deriving type: {}",
                        rel.display(),
                        idx + 1,
                        raw_line.trim()
                    ));
                }
                if depth <= 0 {
                    in_serialize_block = false;
                    depth = 0;
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "L18 privacy stance violated — `stderr*` field added to a \
         Serialize-deriving type under src/storage/:\n{}\n\nSee the \
         \"Privacy stance: no stderr persistence\" doc block at the \
         top of src/storage/run_store.rs for the rationale. Live \
         stderr access belongs on the transient `PostMortem::stderr_tail` \
         buffer in src/ui/panels/postmortem.rs, not the on-disk \
         schema. If you genuinely need persistent stderr that is a \
         deliberate schema change: amend UI Contract v2, update the \
         doc block, and adjust this guard intentionally.",
        violations.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::is_stderr_field;

    #[test]
    fn matches_bare_stderr_field() {
        assert!(is_stderr_field("    pub stderr: String,"));
    }

    #[test]
    fn matches_stderr_underscore_field() {
        assert!(is_stderr_field("    pub stderr_lines: Vec<String>,"));
        assert!(is_stderr_field("    stderr_tail: Option<Vec<String>>,"));
    }

    #[test]
    fn matches_pub_crate_visibility() {
        assert!(is_stderr_field("    pub(crate) stderr_x: u8,"));
        assert!(is_stderr_field("    pub(super) stderr_buf: String,"));
    }

    #[test]
    fn ignores_other_field_names() {
        assert!(!is_stderr_field("    pub stdout_lines: Vec<String>,"));
        assert!(!is_stderr_field("    pub exit_code: Option<i32>,"));
        assert!(!is_stderr_field("    pub stderrish_value: u8,"));
    }

    #[test]
    fn ignores_path_expressions() {
        // `stderr::Foo` is a path, not a field declaration.
        assert!(!is_stderr_field("    use std::io::stderr::Foo;"));
    }

    #[test]
    fn ignores_non_field_lines() {
        assert!(!is_stderr_field("fn stderr_helper() -> String {"));
        assert!(!is_stderr_field("let stderr_buf = String::new();"));
    }
}
