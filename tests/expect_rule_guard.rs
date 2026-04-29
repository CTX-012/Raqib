//! S.3 guard test (Builder A). Walks `src/` and verifies that every
//! `.expect(...)` call sitting outside a `#[cfg(test)]` block is
//! preceded within 8 lines by an `// ok: expect — <reason>` comment.
//!
//! CLAUDE.md documents the three accepted invariants: mutex-poison on
//! critical writers, OnceLock-static `Regex::new`, and
//! `reqwest::Client::builder().build()` in sampler constructors. Any
//! new `expect()` site in production code MUST add the comment OR be
//! refactored to return `Result`.
//!
//! Heuristic for the test/prod split: once we see `#[cfg(test)]` in a
//! file, everything below is treated as test code. This matches how the
//! repo lays modules out (`#[cfg(test)] mod tests { ... }` at the
//! bottom). It is the same heuristic the manual smoke uses.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

fn rs_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir src/") {
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

#[test]
fn every_prod_expect_is_annotated() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = crate_root.join("src");
    assert!(src.is_dir(), "{:?} must exist", src);

    let mut violations: Vec<String> = Vec::new();

    for file in rs_files_under(&src) {
        let body = fs::read_to_string(&file).expect("read source file");
        let lines: Vec<&str> = body.lines().collect();
        let mut in_test = false;

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                in_test = true;
            }
            if in_test {
                continue;
            }
            // Skip the line if "expect(" appears only inside a comment
            // or string literal — naive but sufficient for this repo.
            if !line.contains("expect(") {
                continue;
            }
            // Strip line-comment text from the search target so we don't
            // false-positive on doc-comments that mention "expect(".
            let code_part = match line.find("//") {
                Some(p) => &line[..p],
                None => line,
            };
            if !code_part.contains("expect(") {
                continue;
            }

            // Look back up to 8 lines for an `// ok: expect` marker.
            let lo = idx.saturating_sub(8);
            let annotated = lines[lo..idx].iter().any(|l| l.contains("ok: expect"));
            if !annotated {
                let rel = file.strip_prefix(&crate_root).unwrap_or(&file);
                violations.push(format!(
                    "{}:{}: unannotated `expect(` in non-test code: {}",
                    rel.display(),
                    idx + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "S.3 expect() rule violated:\n{}\n\nAdd an `// ok: expect — <reason>` comment within 8 \
         lines above the call, refactor to return Result, or — for genuinely new invariants — \
         extend CLAUDE.md's accepted-pattern list and explain why in review.",
        violations.join("\n")
    );
}
