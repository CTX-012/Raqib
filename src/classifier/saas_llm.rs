//! Fix 2 — SaaS-LLM CLI recognition.
//!
//! Catches the "agent / chat CLI" class of processes — Claude Code,
//! Cursor, Aider, Continue — that route through a hosted LLM rather
//! than running a local inference server. Their cmdline carries a
//! distinctive publisher/extension path fragment (e.g.
//! `vscode-server/extensions/anthropic.claude-code/...`) which is a
//! more reliable signal than short keywords against bare process
//! names (most of these run as `node`).
//!
//! ## Why a sibling module rather than `CMDLINE_KEYWORDS`
//!
//! `keyword_match::CMDLINE_KEYWORDS` uses `smart_keyword_match` tuned
//! for short tokens (≤3 chars require word boundaries). The SaaS-LLM
//! signals are 30–50-character path fragments — substring matching is
//! the right primitive there, and conflating the two would force the
//! keyword matcher to grow special cases. A dedicated allowlist
//! keeps the table easy to extend as more CLIs ship.
//!
//! ## WorkloadCategory choice
//!
//! Returns `WorkloadCategory::LLM` for now. A future
//! `WorkloadCategory::SaasLLM` variant would require a CAR
//! (`ux_contract::workload_category::GROUP_HEADER_*` is contract-
//! owned), and the §2 per-category metric template ("KV {pct}% · queue
//! {n} · p99 {ms}ms · …") doesn't fit a remote-LLM client cleanly
//! anyway — the CAR work is filed for whenever the §2 mismatch
//! becomes a real UX problem. From the operator's viewpoint these
//! ARE LLM-related activity, so LLM grouping at §1 region 4 reads
//! correctly.

use crate::model::{AICategory, ClassificationResult, ProcessSample, WorkloadCategory};

/// `(path_fragment, short_label)` allowlist. Path fragments are
/// matched case-insensitively as substrings of the joined cmdline.
/// The label is surfaced via `ClassificationResult::evidence` so
/// post-mortem cards and logs read e.g. "SaaS-LLM CLI detected:
/// claude-code" rather than the raw fragment.
///
/// Order doesn't matter — first match wins, but the fragments are
/// mutually exclusive in practice (different publisher prefixes).
/// Add new entries here as the SaaS-LLM CLI category expands.
pub(crate) const SAAS_LLM_CLI_PATTERNS: &[(&str, &str)] = &[
    // Claude Code (Anthropic's VS Code extension + CLI).
    ("vscode-server/extensions/anthropic.claude-code", "claude-code"),
    // Cursor (forked VS Code distribution; their extension folder).
    ("vscode-server/extensions/cursor", "cursor"),
    // Continue.dev (open-source autocomplete + chat extension).
    ("vscode-server/extensions/continue.continue", "continue"),
    // Aider — runs out of a pip package directory rather than VS
    // Code, so the path fragment looks like `site-packages/aider`
    // or the standalone binary name `aider-chat`. Either signal is
    // enough; the short form covers the standalone install path.
    ("aider-chat", "aider"),
    ("site-packages/aider", "aider"),
];

/// Returns the short label for the first SaaS-LLM CLI pattern that
/// substring-matches the joined cmdline (case-insensitive). `None`
/// when no pattern fires — the classifier dispatch falls through to
/// the next predicate.
pub(crate) fn saas_llm_signal(sample: &ProcessSample) -> Option<&'static str> {
    if sample.cmdline.is_empty() {
        return None;
    }
    // Join + lowercase once; substring-match each pattern against the
    // single lowered string. Cheaper than lowering inside the loop,
    // and the joined form catches multi-token fragments like
    // "vscode-server/extensions/anthropic.claude-code/cli.js" that
    // span argv entries on some shells.
    let joined = sample.cmdline.join(" ").to_ascii_lowercase();
    SAAS_LLM_CLI_PATTERNS
        .iter()
        .find(|(frag, _)| joined.contains(&frag.to_ascii_lowercase()))
        .map(|(_, label)| *label)
}

/// Classifier entry point. Returns `Some(ClassificationResult)` when
/// a SaaS-LLM CLI pattern fires; `None` otherwise.
pub(crate) fn classify(sample: &ProcessSample) -> Option<ClassificationResult> {
    saas_llm_signal(sample).map(|label| {
        let evidence = format!("SaaS-LLM CLI detected: {label}");
        ClassificationResult::ai(AICategory::Inference, WorkloadCategory::LLM, evidence)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample(name: &str, argv: &[&str]) -> ProcessSample {
        ProcessSample {
            pid: 1234,
            ppid: Some(1),
            name: name.into(),
            cmdline: argv.iter().map(|s| s.to_string()).collect(),
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        }
    }

    #[test]
    fn claude_code_via_vscode_path_classified_as_ai() {
        // The audit symptom: `node` invoked from the VS Code
        // extensions tree for Anthropic's CLI. Pre-fix this fell
        // through every existing predicate to NotAi.
        let s = sample(
            "node",
            &[
                "node",
                "/home/faiz/.vscode-server/extensions/anthropic.claude-code-2.1.0/cli.js",
            ],
        );
        let r = classify(&s).expect("claude-code path must fire");
        assert_eq!(r.category, AICategory::Inference);
        assert_eq!(r.workload_category, WorkloadCategory::LLM);
        assert!(
            r.evidence.contains("claude-code"),
            "evidence should name the matched label: {}",
            r.evidence
        );
    }

    #[test]
    fn cursor_via_vscode_path_classified_as_ai() {
        let s = sample(
            "node",
            &[
                "node",
                "/home/dev/.vscode-server/extensions/cursor-1.2.3/dist/main.js",
            ],
        );
        let r = classify(&s).expect("cursor path must fire");
        assert_eq!(r.category, AICategory::Inference);
        assert_eq!(r.workload_category, WorkloadCategory::LLM);
    }

    #[test]
    fn aider_via_cmdline_classified_as_ai() {
        // Aider's standalone binary OR a `python -m aider` from a
        // site-packages install — both signals are in the
        // allowlist. Test the standalone form here.
        let s = sample(
            "aider-chat",
            &["aider-chat", "--model", "gpt-4o"],
        );
        let r = classify(&s).expect("aider must fire");
        assert_eq!(r.workload_category, WorkloadCategory::LLM);
    }

    #[test]
    fn aider_via_site_packages_path_classified_as_ai() {
        let s = sample(
            "python3",
            &[
                "/home/dev/.local/lib/python3.11/site-packages/aider/__main__.py",
            ],
        );
        let r = classify(&s).expect("aider site-packages must fire");
        assert!(r.evidence.contains("aider"), "{}", r.evidence);
    }

    #[test]
    fn continue_extension_classified_as_ai() {
        let s = sample(
            "node",
            &[
                "node",
                "/home/dev/.vscode-server/extensions/continue.continue-0.9.0/out/extension.js",
            ],
        );
        let r = classify(&s).expect("continue.dev must fire");
        assert_eq!(r.workload_category, WorkloadCategory::LLM);
    }

    #[test]
    fn bare_node_without_known_cli_falls_through() {
        // Defensive: a `node` process running a different
        // application (e.g. a generic JS server) must NOT be
        // mis-classified as SaaS-LLM. The allowlist is publisher-
        // qualified specifically to avoid this false positive.
        let s = sample(
            "node",
            &["node", "/srv/app/index.js", "--port", "3000"],
        );
        assert!(classify(&s).is_none(), "bare node must not fire");
    }

    #[test]
    fn case_insensitive_path_matching() {
        // VS Code on case-insensitive filesystems (macOS / some
        // network mounts) may surface the extensions directory with
        // mixed case. The allowlist already lowercases both sides;
        // pin the behaviour so a future refactor doesn't lose it.
        let s = sample(
            "node",
            &[
                "node",
                "/Users/Dev/.VSCode-Server/Extensions/Anthropic.Claude-Code-2.1.0/cli.js",
            ],
        );
        let r = classify(&s).expect("case-insensitive match must fire");
        assert!(r.evidence.contains("claude-code"), "{}", r.evidence);
    }

    #[test]
    fn empty_cmdline_returns_none() {
        // Kernel threads and a handful of system processes have an
        // empty cmdline. The matcher must short-circuit rather than
        // construct an empty `joined` string and substring-match
        // against it.
        let s = sample("kthreadd", &[]);
        assert!(classify(&s).is_none());
    }

    #[test]
    fn pattern_label_appears_in_evidence_field() {
        // Evidence string is what the post-mortem card and logs
        // surface to the operator — assert the label is there so a
        // future refactor doesn't accidentally drop it.
        let s = sample(
            "node",
            &[
                "node",
                "/.vscode-server/extensions/anthropic.claude-code/cli.js",
            ],
        );
        let r = classify(&s).expect("claude-code must fire");
        assert!(r.evidence.contains("SaaS-LLM CLI"));
        assert!(r.evidence.contains("claude-code"));
    }
}
