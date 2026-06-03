//! v1.2.0 / DISPATCH 45 / C5 — consumer-side authority-lock guard.
//!
//! ## What this pins
//!
//! The `ux_contract::recommendation` surface is designed as
//! **discriminator, not callable**. The contract enforces that
//! property at the TYPE level (`SuggestedAction: Copy`, no
//! method-on-value, no `Fn`-typed field). The consumer side has
//! a SECOND boundary to honour: the recommendation projection
//! module in `src/recommend.rs` must remain a pure derived-view
//! over `&RuntimeState`. It must NOT acquire an executor handle,
//! a signal-sending capability, or a callback that closes over
//! one.
//!
//! Together — contract type-firewall + consumer wiring-firewall
//! — they pin the operator-locked observe-only boundary at both
//! ends:
//!
//!     contract: `SuggestedAction: Copy` (no callable)
//!     consumer: `src/recommend.rs` has no actuation imports
//!
//! ## Why this is "lock-as-test", not type-system
//!
//! The current module structure already enforces this — `recommend.rs`
//! takes `&RuntimeState` and returns `Vec<Recommendation>`, with no
//! mutable handle in sight. But the test exists as a tripwire: a
//! future contributor who adds `use crate::executor::Executor;` to
//! `recommend.rs` (e.g. to "wire up auto-kill on critical recs")
//! breaks this test before they break production. The diff is
//! visible in CI; the operator-signed boundary stays inviolate.
//!
//! ## What's forbidden
//!
//! Any token in `src/recommend.rs` that names an actuation primitive:
//!
//! - `Executor` (the governor executor handle)
//! - `send_sigterm` / `SIGTERM` / `SIGKILL`
//! - `nix::sys::signal` / `libc::kill`
//! - `kill_pid` / `kill_workload`
//! - `Box<dyn Fn` (callback storage that could later be called)
//! - `crate::governor::audit` (audit writer — only the executor
//!   uses it; the projection has no business with it)
//!
//! Comments and docstrings are NOT scanned — only code. A
//! discussion of the lock in a `//!` doc is fine (and in fact
//! expected; see `src/recommend.rs`).

use std::fs;
use std::path::PathBuf;

const RECOMMEND_PATH: &str = "src/recommend.rs";

/// Names that, if they appear as Rust tokens (i.e. NOT inside a
/// comment) in the recommendation projection module, indicate
/// that the observe-only boundary has been breached.
const FORBIDDEN_TOKENS: &[&str] = &[
    "Executor",
    "send_sigterm",
    "SIGTERM",
    "SIGKILL",
    "nix::sys::signal",
    "libc::kill",
    "kill_pid",
    "kill_workload",
    "Box<dyn Fn",
    "crate::governor::audit",
    "governor::executor",
    "governor::policy",
];

#[test]
fn recommendation_path_has_no_actuation_handle() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join(RECOMMEND_PATH);
    let source = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "failed to read {} (the lock guard cannot enforce \
             the boundary if the projection module moved): {e}",
            path.display(),
        )
    });

    let code_only = strip_comments_and_strings(&source);

    let mut violations: Vec<(usize, &'static str)> = Vec::new();
    for (lineno, line) in code_only.lines().enumerate() {
        for tok in FORBIDDEN_TOKENS {
            if line.contains(tok) {
                violations.push((lineno + 1, tok));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "authority lock breached in {}: the recommendation \
         projection module must remain observe-only (no executor \
         handle, no signal sending, no kill plumbing). Found:\n{}",
        RECOMMEND_PATH,
        violations
            .iter()
            .map(|(l, t)| format!("  line {l}: forbidden token `{t}`"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Strip line + block comments and quoted strings from `source`,
/// preserving line numbers (each stripped span is replaced with
/// spaces so newlines stay aligned). Forbidden tokens that appear
/// in docstrings or string literals are allowed; only code tokens
/// trip the guard.
///
/// This is a deliberately small hand-written stripper — not a full
/// Rust tokenizer. It handles the cases that actually appear in
/// `src/recommend.rs`: `//`, `/* */`, `"..."`, and `r"..."` /
/// `r#"..."#`. If the file ever grows a raw-string token with `#`
/// inside, refresh this helper.
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

#[test]
fn guard_would_catch_a_real_breach() {
    // Synthesize a source string with a code-side actuation
    // import and run the same scan. This is the "negative
    // direction" the lock-as-test pattern needs — without it,
    // a bug in the scanner could silently let the guard pass
    // forever.
    let synthetic = r#"
//! Doc mentions Executor — this is fine.
use crate::executor::Executor;

pub fn project(state: &RuntimeState) -> Vec<Recommendation> {
    let _x = Executor::new();
    vec![]
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
        !violations.is_empty(),
        "scanner failed to detect a synthetic actuation breach; \
         the lock guard would be a no-op against real drift",
    );
    assert!(
        violations.contains(&"Executor"),
        "scanner missed the `Executor` token: {violations:?}",
    );
}

#[test]
fn comment_stripper_does_not_clobber_code() {
    // Self-test: a forbidden token inside a comment or string must
    // be stripped, but a real token must survive.
    let src = r#"
// Executor here is in a comment, should be stripped
let x = "Executor in string";
fn run() { Executor::new(); }
/* block: Executor */
"#;
    let stripped = strip_comments_and_strings(src);
    // The code call site survives.
    assert!(
        stripped.contains("Executor::new"),
        "code token must survive: {stripped}",
    );
    // The comment occurrences do not (rough check: only ONE
    // remaining `Executor`, the code one).
    assert_eq!(
        stripped.matches("Executor").count(),
        1,
        "comments/strings must be stripped: {stripped}",
    );
}
