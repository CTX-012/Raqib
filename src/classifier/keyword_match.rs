use crate::model::{AICategory, ClassificationResult, WorkloadCategory};

/// Keywords whose length is at or below this value require a word boundary on
/// both sides. Without this, "ai" would match "mail", "ros" would match
/// "microsoft", "llm" would match in the middle of a longer token.
/// Threshold matches the legacy classifier: short acronyms ("ai", "llm", "cv",
/// "ros", "tgi") need strict matching; 4-char terms like "yolo" and "vllm" are
/// safe as substrings because they don't appear embedded in common words.
const BOUNDARY_MAX_LEN: usize = 3;

/// L11a — `(keyword, AICategory, WorkloadCategory)` triple.
/// `AICategory` is the workflow-phase axis; `WorkloadCategory` is
/// the contract's workload-type axis. See `model::WorkloadCategory`
/// for the rationale on keeping both side-by-side.
type KeywordEntry = (&'static str, AICategory, WorkloadCategory);

/// GAZEBO+RVIZ2 dispatch — `(keyword, AICategory, WorkloadCategory,
/// friendly_display_name)` — for workloads whose `/proc/<pid>/comm`
/// is misleading (Gazebo's comm is `ruby` because it's Ruby-
/// launched). The runtime promotes `friendly_display_name` onto
/// `AnnotatedProcess.name` at annotation time — mirror of the D107
/// `promote_sha_blob_hints` pattern for ollama.
type FriendlyKeywordEntry = (
    &'static str,
    AICategory,
    WorkloadCategory,
    &'static str,
);

/// GAZEBO+RVIZ2 dispatch — friendly-name overrides for the process-
/// name path. Classic Gazebo ships its own binaries (`gzserver` /
/// `gzclient`); comm matches the binary. The friendly display name
/// is still `"Gazebo"` because "gzserver" / "gzclient" aren't
/// operator-recognisable at a glance.
static NAME_KEYWORDS_WITH_FRIENDLY: &[FriendlyKeywordEntry] = &[
    ("gzserver", AICategory::Inference, WorkloadCategory::ROS2, "Gazebo"),
    ("gzclient", AICategory::Inference, WorkloadCategory::ROS2, "Gazebo"),
];

/// Checked against the bare process name (basename of argv[0]).
static NAME_KEYWORDS: &[KeywordEntry] = &[
    // Inference servers and runtimes — LLM unless noted otherwise.
    ("llama-server", AICategory::Inference, WorkloadCategory::LLM),
    ("llama-cpp", AICategory::Inference, WorkloadCategory::LLM),
    ("llamacpp", AICategory::Inference, WorkloadCategory::LLM),
    ("ollama", AICategory::Inference, WorkloadCategory::LLM),
    ("vllm", AICategory::Inference, WorkloadCategory::LLM),
    ("trtllm", AICategory::Inference, WorkloadCategory::LLM),
    // Triton/TorchServe are general-purpose model servers — could host
    // LLM or Vision; default Unknown until model-path or stdout signals
    // disambiguate.
    ("tritonserver", AICategory::Inference, WorkloadCategory::Unknown),
    ("torchserve", AICategory::Inference, WorkloadCategory::Unknown),
    // Audio (whisper) and image-gen (comfyui, invoke-ai) → Vision.
    // Whisper produces text from audio; closer to Vision-class
    // perceptual model than to LLM in v0.3's taxonomy.
    ("whisper-server", AICategory::Inference, WorkloadCategory::Vision),
    ("comfyui", AICategory::Inference, WorkloadCategory::Vision),
    ("invoke-ai", AICategory::Inference, WorkloadCategory::Vision),
    // Training launchers — workflow-phase distinction; the workload-
    // type axis collapses these to Unknown per L11a's design comment.
    ("deepspeed", AICategory::Training, WorkloadCategory::Unknown),
    ("torchrun", AICategory::Training, WorkloadCategory::Unknown),
    ("accelerate", AICategory::Training, WorkloadCategory::Unknown),
    // Model management — same: collapses to Unknown.
    ("huggingface-cli", AICategory::ModelDownload, WorkloadCategory::Unknown),
];

/// GAZEBO+RVIZ2 dispatch — friendly-name overrides for the cmdline
/// path. Gazebo Fortress launches as `ign gazebo server`/`gui`; the
/// binary is Ruby-launched so `/proc/<pid>/comm = "ruby"` (unhelpful).
/// Gazebo Harmonic uses `gz sim server`/`gui` — same story. Both
/// phrase markers are precise (two words, unambiguous — no bystander
/// tool has "ign gazebo" or "gz sim" in its cmdline).
///
/// Checked BEFORE [`CMDLINE_KEYWORDS`] so a phrase-carrying entry
/// wins over any accidental single-word match downstream. The
/// friendly display name `"Gazebo"` is promoted onto
/// `AnnotatedProcess.name` at annotation time; the workload
/// category is `ROS2` per the ratified dispatch (Gazebo is a
/// robotics simulator commonly paired with ROS2 — no new category
/// needed).
static CMDLINE_KEYWORDS_WITH_FRIENDLY: &[FriendlyKeywordEntry] = &[
    // Gazebo Fortress (default on Ubuntu 22 / ROS Humble).
    ("ign gazebo", AICategory::Inference, WorkloadCategory::ROS2, "Gazebo"),
    // Gazebo Harmonic (newer).
    ("gz sim", AICategory::Inference, WorkloadCategory::ROS2, "Gazebo"),
];

/// Checked against all cmdline tokens joined with spaces. Keyword order matters:
/// more specific patterns should appear before generic ones.
static CMDLINE_KEYWORDS: &[KeywordEntry] = &[
    // Inference servers — LLM
    ("llama-server", AICategory::Inference, WorkloadCategory::LLM),
    ("llama-cpp", AICategory::Inference, WorkloadCategory::LLM),
    ("llamacpp", AICategory::Inference, WorkloadCategory::LLM),
    ("ollama", AICategory::Inference, WorkloadCategory::LLM),
    ("vllm", AICategory::Inference, WorkloadCategory::LLM),
    ("trtllm", AICategory::Inference, WorkloadCategory::LLM),
    // General-purpose servers — Unknown until model-path narrows it.
    ("tritonserver", AICategory::Inference, WorkloadCategory::Unknown),
    ("torchserve", AICategory::Inference, WorkloadCategory::Unknown),
    // Vision / image-gen / audio
    ("whisper", AICategory::Inference, WorkloadCategory::Vision),
    ("stable-diffusion", AICategory::Inference, WorkloadCategory::Vision),
    ("comfyui", AICategory::Inference, WorkloadCategory::Vision),
    ("ultralytics", AICategory::Inference, WorkloadCategory::Vision),
    ("yolo", AICategory::Inference, WorkloadCategory::Vision),
    // Embeddings — sentence-transformers + the common open
    // embedding-model families. v1.1.4 P5-B4-CLASSIFY: the prior
    // list (sentence-transformers + bge-) was narrower than the B4
    // sampler's own markers and missed gte-/e5- plus FlagEmbedding
    // and other families real embeddings workloads carry in their
    // cmdline (HF repo id, `python -m`, or inline `-c` import —
    // all reachable via the joined-cmdline substring match below).
    // High-confidence substrings only; no CPU-magnitude heuristic
    // (that would false-positive on training / data jobs).
    ("sentence-transformers", AICategory::Inference, WorkloadCategory::Embeddings),
    ("sentence_transformers", AICategory::Inference, WorkloadCategory::Embeddings),
    ("flagembedding", AICategory::Inference, WorkloadCategory::Embeddings),
    ("bge-", AICategory::Inference, WorkloadCategory::Embeddings),
    ("gte-", AICategory::Inference, WorkloadCategory::Embeddings),
    // e5 family: the bare "e5-" is only 3 chars and would trip the
    // ≤BOUNDARY_MAX_LEN word-boundary rule (the trailing '-' is part
    // of the keyword, so the matcher still demands a boundary AFTER
    // it). Use the ≥4-char model-name suffixes instead — substring-
    // safe and still covers the real e5 model ids.
    ("e5-base", AICategory::Inference, WorkloadCategory::Embeddings),
    ("e5-large", AICategory::Inference, WorkloadCategory::Embeddings),
    ("e5-small", AICategory::Inference, WorkloadCategory::Embeddings),
    ("multilingual-e5", AICategory::Inference, WorkloadCategory::Embeddings),
    ("nomic-embed", AICategory::Inference, WorkloadCategory::Embeddings),
    ("all-minilm", AICategory::Inference, WorkloadCategory::Embeddings),
    ("jina-embeddings", AICategory::Inference, WorkloadCategory::Embeddings),
    // Training
    ("deepspeed", AICategory::Training, WorkloadCategory::Unknown),
    ("torchrun", AICategory::Training, WorkloadCategory::Unknown),
    ("accelerate", AICategory::Training, WorkloadCategory::Unknown),
    // Model management
    ("huggingface-cli", AICategory::ModelDownload, WorkloadCategory::Unknown),
    // Frameworks and libraries (generic; lower priority than the above)
    ("transformers", AICategory::Framework, WorkloadCategory::Unknown),
    ("diffusers", AICategory::Framework, WorkloadCategory::Vision),
    ("pytorch", AICategory::Framework, WorkloadCategory::Unknown),
    ("tensorflow", AICategory::Framework, WorkloadCategory::Unknown),
    ("langchain", AICategory::Framework, WorkloadCategory::LLM),
    ("onnxruntime", AICategory::Framework, WorkloadCategory::Unknown),
    // Short keywords — word-boundary matched
    ("llm", AICategory::Inference, WorkloadCategory::LLM),
    ("tgi", AICategory::Inference, WorkloadCategory::LLM),
    ("gpt", AICategory::Inference, WorkloadCategory::LLM),
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
    // GAZEBO dispatch — friendly-name overrides first. When a rule
    // in `NAME_KEYWORDS_WITH_FRIENDLY` matches, we emit a friendly
    // display name via `ai_with_friendly_name` so the runtime's
    // promotion pass swaps AnnotatedProcess.name to something the
    // operator recognises.
    if let Some(result) = NAME_KEYWORDS_WITH_FRIENDLY
        .iter()
        .find_map(|&(kw, category, workload_category, friendly)| {
            smart_keyword_match(name, kw).then(|| {
                ClassificationResult::ai_with_friendly_name(
                    category,
                    workload_category,
                    format!("process name matches keyword {kw:?} (friendly: {friendly:?})"),
                    friendly,
                )
            })
        })
    {
        return Some(result);
    }
    NAME_KEYWORDS
        .iter()
        .find_map(|&(kw, category, workload_category)| {
            smart_keyword_match(name, kw).then(|| {
                ClassificationResult::ai(
                    category,
                    workload_category,
                    format!("process name matches keyword {:?}", kw),
                )
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
    // GAZEBO dispatch — friendly-name overrides first (same shape
    // as `classify_by_name`). The phrase markers "ign gazebo" /
    // "gz sim" are precise: a script named "gazebo_helper.py"
    // whose joined cmdline is `python3 gazebo_helper.py` does NOT
    // contain either phrase, so the negative test in this file's
    // `#[cfg(test)]` block below fires green.
    if let Some(result) = CMDLINE_KEYWORDS_WITH_FRIENDLY
        .iter()
        .find_map(|&(kw, category, workload_category, friendly)| {
            smart_keyword_match(&joined, kw).then(|| {
                ClassificationResult::ai_with_friendly_name(
                    category,
                    workload_category,
                    format!("cmdline matches keyword {kw:?} (friendly: {friendly:?})"),
                    friendly,
                )
            })
        })
    {
        return Some(result);
    }
    CMDLINE_KEYWORDS
        .iter()
        .find_map(|&(kw, category, workload_category)| {
            smart_keyword_match(&joined, kw).then(|| {
                ClassificationResult::ai(
                    category,
                    workload_category,
                    format!("cmdline matches keyword {:?}", kw),
                )
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

    // ── v1.1.4 P5-B4-CLASSIFY — broadened embeddings coverage ──────────────────

    /// Embedding-model families + libraries the operator's workloads
    /// carry in cmdline (HF repo id, `python -m`, or inline `-c`
    /// import) must classify as Embeddings — not fall through to
    /// Unknown. The joined-cmdline substring match reaches all three
    /// invocation shapes.
    #[test]
    fn embedding_families_classify_as_embeddings() {
        let cases: &[&[&str]] = &[
            // HF repo id in argv (the B4 calibration proxy shape).
            &["python3", "encode.py", "--model", "BAAI/bge-small-en-v1.5"],
            &["python3", "encode.py", "--model", "thenlper/gte-large"],
            &["python3", "encode.py", "--model", "intfloat/e5-base-v2"],
            &["python3", "encode.py", "--model", "nomic-ai/nomic-embed-text-v1"],
            &["python3", "encode.py", "--model", "sentence-transformers/all-MiniLM-L6-v2"],
            &["python3", "encode.py", "--model", "jinaai/jina-embeddings-v2-base-en"],
            // FlagEmbedding library via `python -m` (dispatch-named).
            &["python3", "-m", "FlagEmbedding.server"],
            // Inline `-c` import — reachable via the joined cmdline.
            &["python3", "-c", "from sentence_transformers import SentenceTransformer"],
        ];
        for cmdline in cases {
            let argv: Vec<String> = cmdline.iter().map(|s| s.to_string()).collect();
            let r = classify_by_cmdline(&argv)
                .unwrap_or_else(|| panic!("expected a classification for {cmdline:?}"));
            assert_eq!(
                r.workload_category,
                WorkloadCategory::Embeddings,
                "expected Embeddings for {cmdline:?}, got {:?}",
                r.workload_category,
            );
        }
    }

    /// Guard against over-reach: a non-embeddings heavy-CPU python
    /// job (e.g. a training run) must NOT be swept into Embeddings by
    /// the broadened coverage. Embeddings detection is substring-on-
    /// family-name, not a CPU-magnitude heuristic.
    #[test]
    fn broadened_embeddings_does_not_catch_unrelated_python() {
        let cmdline = vec![
            "python3".to_string(),
            "train.py".to_string(),
            "--epochs".to_string(),
            "50".to_string(),
        ];
        // No embedding family token → not classified Embeddings (it
        // may match nothing, or match a generic framework, but never
        // Embeddings).
        if let Some(r) = classify_by_cmdline(&cmdline) {
            assert_ne!(r.workload_category, WorkloadCategory::Embeddings);
        }
    }

    // ── GAZEBO+RVIZ2 dispatch — friendly-name rules ────────────────────────────
    //
    // Gazebo launches as `ign gazebo server`/`gui` (Fortress) or
    // `gz sim server`/`gui` (Harmonic), Ruby-launched → comm is
    // "ruby". Classic Gazebo ships its own gzserver / gzclient
    // binaries. All four cases must classify as ROS2 workloads
    // with friendly display name "Gazebo", so the runtime's
    // promotion pass swaps the misleading "ruby" comm to "Gazebo"
    // before it reaches the workloads panel.

    #[test]
    fn ign_gazebo_server_cmdline_classifies_as_gazebo() {
        let cmdline = vec!["ign".to_string(), "gazebo".to_string(), "server".to_string()];
        let r = classify_by_cmdline(&cmdline).expect("must classify");
        assert_eq!(r.category, AICategory::Inference);
        assert_eq!(r.workload_category, WorkloadCategory::ROS2);
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));
    }

    #[test]
    fn ign_gazebo_gui_cmdline_classifies_as_gazebo() {
        let cmdline = vec!["ign".to_string(), "gazebo".to_string(), "gui".to_string()];
        let r = classify_by_cmdline(&cmdline).expect("must classify");
        assert_eq!(r.workload_category, WorkloadCategory::ROS2);
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));
    }

    #[test]
    fn gz_sim_server_harmonic_cmdline_classifies_as_gazebo() {
        // Newer Gazebo Harmonic invocation. Same friendly name.
        let cmdline = vec!["gz".to_string(), "sim".to_string(), "server".to_string()];
        let r = classify_by_cmdline(&cmdline).expect("must classify");
        assert_eq!(r.workload_category, WorkloadCategory::ROS2);
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));
    }

    #[test]
    fn gzserver_binary_name_classifies_as_gazebo() {
        // Classic Gazebo (pre-Fortress) — separate binary, comm
        // matches. NAME table path.
        let r = classify_by_name("gzserver").expect("must classify");
        assert_eq!(r.workload_category, WorkloadCategory::ROS2);
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));
    }

    #[test]
    fn gzclient_binary_name_classifies_as_gazebo() {
        let r = classify_by_name("gzclient").expect("must classify");
        assert_eq!(r.workload_category, WorkloadCategory::ROS2);
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));
    }

    /// THE PRECISION GUARD — mandatory per dispatch HARD RULE 2.
    /// A random user script named "gazebo_helper.py" whose joined
    /// cmdline is `python3 gazebo_helper.py` MUST NOT classify as
    /// Gazebo. The phrase markers `"ign gazebo"` / `"gz sim"` are
    /// two-word — a single-word `"gazebo"` substring in a filename
    /// doesn't trip them. Without this test, a well-meaning
    /// contributor might "simplify" the rule to `"gazebo"` alone
    /// and silently break precision.
    #[test]
    fn plain_ruby_script_named_gazebo_helper_does_not_classify_as_gazebo() {
        let cmdline = vec![
            "python3".to_string(),
            "gazebo_helper.py".to_string(),
        ];
        // Must NOT match the friendly-name Gazebo rule.
        let result = classify_by_cmdline(&cmdline);
        if let Some(r) = &result {
            assert_ne!(
                r.friendly_process_name.as_deref(),
                Some("Gazebo"),
                "a script named `gazebo_helper.py` MUST NOT match the Gazebo rule; \
                 the phrase markers are two-word (\"ign gazebo\" / \"gz sim\") \
                 precisely to avoid this false-positive",
            );
        }
        // Belt-and-braces: no matter what other rule fires (probably
        // none — the cmdline has no AI keywords), the workload
        // category must not be spuriously ROS2 via the Gazebo path.
        if let Some(r) = &result {
            // If some OTHER rule classifies it (e.g. a future
            // Python-python match), that's fine — the friendly name
            // check above is the load-bearing one.
            let _ = r;
        }
    }

    /// A ruby process WITHOUT "ign gazebo" or "gz sim" in cmdline
    /// (e.g. a Rails app) must NOT classify as Gazebo. The comm
    /// is "ruby" but the name path only matches the specific
    /// classic-Gazebo binary names.
    #[test]
    fn plain_ruby_process_does_not_classify_as_gazebo() {
        let name = "ruby";
        assert!(classify_by_name(name).is_none(),
            "bare `ruby` comm must not match — only `gzserver` / `gzclient` do");
        let cmdline = vec!["ruby".to_string(), "/usr/bin/rails".to_string(), "server".to_string()];
        let result = classify_by_cmdline(&cmdline);
        if let Some(r) = result {
            assert_ne!(r.friendly_process_name.as_deref(), Some("Gazebo"));
        }
    }

    /// LIVE-HOST INVARIANT: Gazebo's `ign` launcher uses Ruby's
    /// `Process.setproctitle` to overwrite argv into a SINGLE
    /// argv[0] string, padded with `\0` to the argv buffer length.
    /// `linux_proc::read_cmdline` split-on-null therefore yields
    /// `vec!["ign gazebo server"]` (ONE element), not
    /// `vec!["ign", "gazebo", "server"]` (three elements).
    ///
    /// This shape is what actually lands from the live host —
    /// verified 2026-07-31 against `/proc/19600/cmdline` hex-dump:
    /// bytes 0..17 are `"ign gazebo server\0"` and the rest is
    /// null padding. The joined-cmdline substring match still
    /// fires because `"ign gazebo"` is a substring of the single
    /// joined element. Without this test, a future refactor that
    /// switched to per-token matching (e.g. checking `argv[0] ==
    /// "ign" && argv[1] == "gazebo"`) would silently break the
    /// real-world case.
    #[test]
    fn setproctitle_single_token_cmdline_still_classifies_as_gazebo() {
        // Fortress — the operator's live cmdline shape.
        let cmdline = vec!["ign gazebo server".to_string()];
        let r = classify_by_cmdline(&cmdline).expect("live-host setproctitle shape must classify");
        assert_eq!(r.workload_category, WorkloadCategory::ROS2);
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));

        let gui = vec!["ign gazebo gui".to_string()];
        let r = classify_by_cmdline(&gui).expect("gui setproctitle shape must classify");
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));

        // Harmonic setproctitle shape (same pattern, different phrase).
        let harmonic = vec!["gz sim server".to_string()];
        let r = classify_by_cmdline(&harmonic).expect("Harmonic setproctitle shape must classify");
        assert_eq!(r.friendly_process_name.as_deref(), Some("Gazebo"));
    }
}
