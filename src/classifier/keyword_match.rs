use crate::model::{AICategory, ClassificationResult};

/// Keywords whose length is at or below this value require a word boundary on
/// both sides. Without this, "ai" would match "mail", "ros" would match
/// "microsoft", "llm" would match in the middle of a longer token.
/// Threshold matches the legacy classifier: short acronyms ("ai", "llm", "cv",
/// "ros", "tgi") need strict matching; 4-char terms like "yolo" and "vllm" are
/// safe as substrings because they don't appear embedded in common words.
const BOUNDARY_MAX_LEN: usize = 3;

/// Checked against the bare process name (basename of argv[0]).
static NAME_KEYWORDS: &[(&str, AICategory)] = &[
    // Inference servers and runtimes
    ("llama-server", AICategory::Inference),
    ("llama-cpp", AICategory::Inference),
    ("llamacpp", AICategory::Inference),
    ("ollama", AICategory::Inference),
    ("vllm", AICategory::Inference),
    ("tritonserver", AICategory::Inference),
    ("torchserve", AICategory::Inference),
    ("trtllm", AICategory::Inference),
    ("whisper-server", AICategory::Inference),
    ("comfyui", AICategory::Inference),
    ("invoke-ai", AICategory::Inference),
    // Training launchers
    ("deepspeed", AICategory::Training),
    ("torchrun", AICategory::Training),
    ("accelerate", AICategory::Training),
    // Model management
    ("huggingface-cli", AICategory::ModelDownload),
];

/// Checked against all cmdline tokens joined with spaces. Keyword order matters:
/// more specific patterns should appear before generic ones.
static CMDLINE_KEYWORDS: &[(&str, AICategory)] = &[
    // Inference servers
    ("llama-server", AICategory::Inference),
    ("llama-cpp", AICategory::Inference),
    ("llamacpp", AICategory::Inference),
    ("ollama", AICategory::Inference),
    ("vllm", AICategory::Inference),
    ("tritonserver", AICategory::Inference),
    ("torchserve", AICategory::Inference),
    ("trtllm", AICategory::Inference),
    ("whisper", AICategory::Inference),
    ("stable-diffusion", AICategory::Inference),
    ("comfyui", AICategory::Inference),
    // Training
    ("deepspeed", AICategory::Training),
    ("torchrun", AICategory::Training),
    ("accelerate", AICategory::Training),
    // Model management
    ("huggingface-cli", AICategory::ModelDownload),
    // Frameworks and libraries (generic; lower priority than the above)
    ("transformers", AICategory::Framework),
    ("diffusers", AICategory::Framework),
    ("pytorch", AICategory::Framework),
    ("tensorflow", AICategory::Framework),
    ("langchain", AICategory::Framework),
    ("onnxruntime", AICategory::Framework),
    // Short keywords — word-boundary matched
    ("llm", AICategory::Inference),
    ("tgi", AICategory::Inference),
    ("gpt", AICategory::Inference),
];

/// Case-insensitive keyword search with optional word-boundary enforcement.
///
/// Short keywords (len ≤ BOUNDARY_MAX_LEN) require non-alphanumeric chars (or
/// string edges) on both sides so that e.g. "ai" does not fire on "mail".
/// Longer keywords match as plain substrings.
pub(crate) fn smart_keyword_match(text: &str, keyword: &str) -> bool {
    if keyword.is_empty() {
        return false;
    }
    let text_lower = text.to_lowercase();
    let kw_lower = keyword.to_lowercase();
    if keyword.len() <= BOUNDARY_MAX_LEN {
        has_word_boundary_match(&text_lower, &kw_lower)
    } else {
        text_lower.contains(kw_lower.as_str())
    }
}

/// Returns true if `needle` appears in `haystack` with non-alphanumeric (or
/// string-edge) chars on both sides. Both arguments must already be lowercase.
fn has_word_boundary_match(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(pos, matched)| {
        let bytes = haystack.as_bytes();
        let before_ok = pos == 0 || !is_word_char(bytes[pos - 1]);
        let end = pos + matched.len();
        let after_ok = end >= bytes.len() || !is_word_char(bytes[end]);
        before_ok && after_ok
    })
}

/// Alphanumerics are word characters; underscore and everything else is not.
/// This means "ollama_llm" has a boundary before "llm", which is intentional:
/// snake_case component names should each match as whole tokens.
fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

pub(crate) fn classify_by_name(name: &str) -> Option<ClassificationResult> {
    NAME_KEYWORDS.iter().find_map(|&(kw, category)| {
        smart_keyword_match(name, kw).then(|| {
            ClassificationResult::ai(category, format!("process name matches keyword {:?}", kw))
        })
    })
}

/// Joins all cmdline tokens with spaces before matching so that word-boundary
/// logic works naturally across token boundaries.
pub(crate) fn classify_by_cmdline(cmdline: &[String]) -> Option<ClassificationResult> {
    if cmdline.is_empty() {
        return None;
    }
    let joined = cmdline.join(" ");
    CMDLINE_KEYWORDS.iter().find_map(|&(kw, category)| {
        smart_keyword_match(&joined, kw).then(|| {
            ClassificationResult::ai(category, format!("cmdline matches keyword {:?}", kw))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── smart_keyword_match / word-boundary logic ────────────────────────────

    #[test]
    fn short_keyword_blocked_inside_word() {
        // "ai" must not fire on "mail" or embedded positions
        assert!(!smart_keyword_match("mail", "ai"));
        assert!(!smart_keyword_match("rainy", "ai"));
        assert!(!smart_keyword_match("main_loop", "ai"));
    }

    #[test]
    fn short_keyword_fires_at_boundaries() {
        assert!(smart_keyword_match("ai", "ai"));
        assert!(smart_keyword_match("ai-worker", "ai"));
        assert!(smart_keyword_match("run-ai", "ai"));
        assert!(smart_keyword_match("run.ai.service", "ai"));
        assert!(smart_keyword_match("the_ai_model", "ai"));
    }

    #[test]
    fn llm_word_boundary() {
        // len("llm") == 3 → needs boundaries
        assert!(smart_keyword_match("llm", "llm"));
        assert!(smart_keyword_match("run-llm", "llm"));
        assert!(smart_keyword_match("llm-server", "llm"));
        assert!(smart_keyword_match("/usr/bin/llm", "llm"));
        assert!(smart_keyword_match("ollama_llm", "llm"));

        assert!(!smart_keyword_match("nollm", "llm"));
        assert!(!smart_keyword_match("llmx", "llm"));
    }

    #[test]
    fn vllm_word_boundary() {
        // len("vllm") == 4 → matches as substring (no boundaries needed)
        // Only keywords <= 3 chars need word boundaries
        assert!(smart_keyword_match("vllm", "vllm"));
        assert!(smart_keyword_match("run-vllm", "vllm"));
        assert!(smart_keyword_match("/usr/bin/vllm", "vllm"));
        // "vllm" is long enough to match as substring
        assert!(smart_keyword_match("avllm", "vllm"));
    }

    #[test]
    fn long_keyword_matches_substring() {
        assert!(smart_keyword_match("run-ollama-server", "ollama"));
        assert!(smart_keyword_match("pytorch_lightning", "pytorch"));
        assert!(smart_keyword_match("use_transformers_v2", "transformers"));
    }

    #[test]
    fn case_insensitive() {
        assert!(smart_keyword_match("OLLama", "ollama"));
        assert!(smart_keyword_match("LLM_RUNNER", "llm"));
        assert!(smart_keyword_match("PyTorch", "pytorch"));
    }

    #[test]
    fn empty_keyword_never_matches() {
        assert!(!smart_keyword_match("anything", ""));
        assert!(!smart_keyword_match("", ""));
    }

    // ── classify_by_name ─────────────────────────────────────────────────────

    #[test]
    fn known_inference_server_names() {
        for name in &["ollama", "vllm", "llama-server", "tritonserver"] {
            let result = classify_by_name(name);
            assert_eq!(
                result.map(|r| r.category),
                Some(AICategory::Inference),
                "expected Inference for {:?}",
                name
            );
        }
    }

    #[test]
    fn training_launcher_names() {
        for name in &["deepspeed", "torchrun", "accelerate"] {
            let result = classify_by_name(name);
            assert_eq!(
                result.map(|r| r.category),
                Some(AICategory::Training),
                "expected Training for {:?}",
                name
            );
        }
    }

    #[test]
    fn non_ai_names_return_none() {
        for name in &["nginx", "bash", "systemd", "sshd", "cargo"] {
            assert!(
                classify_by_name(name).is_none(),
                "expected None for {:?}",
                name
            );
        }
    }

    // ── classify_by_cmdline ───────────────────────────────────────────────────

    #[test]
    fn vllm_module_in_python_cmdline() {
        let cmdline = vec![
            "python3".into(),
            "-m".into(),
            "vllm.entrypoints.api_server".into(),
        ];
        let result = classify_by_cmdline(&cmdline);
        assert_eq!(result.map(|r| r.category), Some(AICategory::Inference));
    }

    #[test]
    fn transformers_in_script_path() {
        let cmdline = vec!["python3".into(), "run_transformers_eval.py".into()];
        let result = classify_by_cmdline(&cmdline);
        assert_eq!(result.map(|r| r.category), Some(AICategory::Framework));
    }

    #[test]
    fn empty_cmdline_returns_none() {
        assert!(classify_by_cmdline(&[]).is_none());
    }

    #[test]
    fn non_ai_cmdline_returns_none() {
        let cmdline = vec!["nginx".into(), "-c".into(), "/etc/nginx/nginx.conf".into()];
        assert!(classify_by_cmdline(&cmdline).is_none());
    }
}
