use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::{
    AICategory, ClassificationResult, ProcessSample, workload_category_from_model_path,
};

/// Extensions that are unambiguously model weight files.
/// Seeing one of these in a cmdline arg → high-confidence Inference.
static STRONG_MODEL_EXTENSIONS: &[&str] = &[
    ".gguf",        // llama.cpp / GGML format
    ".safetensors", // HuggingFace preferred format
    ".onnx",        // ONNX interchange format
    ".engine",      // TensorRT compiled engine (Jetson / desktop)
    ".plan",        // TensorRT serialised plan (alias for .engine)
    ".tflite",      // TensorFlow Lite (edge devices)
];

/// Extensions that are likely model files but also appear in non-AI contexts.
/// Weaker signal; only used when combined with other evidence (future work).
static WEAK_MODEL_EXTENSIONS: &[&str] = &[
    ".pb",   // TensorFlow protobuf — also used for other protobufs
    ".pt",   // PyTorch checkpoint — also arbitrary Python pickles
    ".pth",  // PyTorch checkpoint variant
    ".ckpt", // Generic checkpoint
    ".bin",  // Raw weights — very common name in non-AI software
];

/// CLI flags that are followed (or `=`-separated) by a model path.
static MODEL_FLAGS: &[&str] = &[
    "--model",
    "-m",
    "--model-path",
    "--model-dir",
    "--model_path",
    "--model_name_or_path",
    "--checkpoint",
    "--weights",
    "--load",
];

/// F3 — extract a human-readable model name from an LLM-runtime
/// cmdline when the strong-extension path detector at
/// [`find_model_in_cmdline`] didn't fire. Three runtimes covered:
///
/// 1. **Ollama** — `ollama run <model>` (also `ollama pull`, `ollama
///    show`) → the model token. The bare `ollama serve` daemon has
///    no model in cmdline and returns `None`.
/// 2. **vLLM** — `python -m vllm.entrypoints.* --model <X>` where X
///    can be a HuggingFace repo ID (`meta-llama/Llama-3-8B`) or a
///    local directory. The repo basename is returned (no extension
///    stripping — HF IDs don't have extensions).
/// 3. **llama.cpp** — `llama-server / llama-cli -m <X>.gguf`. Path
///    basename + extension stripped. Strong-extension cmdlines also
///    get caught by [`find_model_in_cmdline`] earlier in the
///    classifier pipeline; this branch covers the rest.
///
/// Returns `None` when none of the runtimes' patterns match — the
/// caller treats that as "model name unknown, render empty column."
pub fn extract_model_name(cmdline: &[String]) -> Option<String> {
    if let Some(model) = extract_ollama_run_model(cmdline) {
        return Some(model);
    }
    if let Some(model) = extract_flag_model(cmdline) {
        return Some(model);
    }
    None
}

/// Match `ollama run <model>` / `ollama pull <model>` /
/// `ollama show <model>`. Returns the model token verbatim.
///
/// `ollama serve` and bare `ollama` (no subcommand) return `None` —
/// the daemon doesn't know which model is loaded from cmdline alone
/// (the dispatcher's `/api/ps` sampler covers that case at runtime).
fn extract_ollama_run_model(cmdline: &[String]) -> Option<String> {
    // Find the basename of argv[0] — accept `ollama`, `/usr/local/bin/ollama`,
    // etc. Reject containers / wrappers that happen to have "ollama" as a
    // non-argv0 token (e.g. `bash -c 'ollama run ...'`).
    let argv0 = cmdline.first()?;
    let basename = argv0.rsplit('/').next().unwrap_or(argv0);
    if basename != "ollama" {
        return None;
    }
    let subcommand = cmdline.get(1)?;
    if !matches!(subcommand.as_str(), "run" | "pull" | "show") {
        return None;
    }
    let model = cmdline.get(2)?;
    if model.is_empty() || model.starts_with('-') {
        return None;
    }
    Some(model.clone())
}

/// Generic `--model <X>` / `--model=X` extractor for vLLM-style
/// cmdlines. The value can be:
/// - HuggingFace repo ID (`meta-llama/Llama-3-8B`) → returns basename
///   (`Llama-3-8B`).
/// - Local path with no strong extension (`/m/llama3.safetensors` IS
///   handled by [`find_model_in_cmdline`] before this function runs;
///   `.pt` / `.pth` / `.bin` weak extensions fall through here) →
///   returns the file stem (extension stripped).
/// - Bare token (`tinyllama:1.5b`) → returned verbatim.
///
/// The short `-m` flag is intentionally excluded when argv[0] is a
/// Python interpreter, because `python -m <module>` collides with our
/// MODEL_FLAGS table on `-m`. For llama.cpp's `llama-server -m
/// path.gguf` we still honour `-m` (argv[0] isn't python). The
/// strong-extension path of `find_model_in_cmdline` covers the .gguf
/// case independently anyway.
fn extract_flag_model(cmdline: &[String]) -> Option<String> {
    let argv0_is_python = cmdline
        .first()
        .map(|s| s.rsplit('/').next().unwrap_or(s.as_str()))
        .map(|basename| basename.starts_with("python"))
        .unwrap_or(false);
    for (i, token) in cmdline.iter().enumerate() {
        if let Some(value) = extract_flag_eq_value(token) {
            return Some(strip_path_and_extension(value));
        }
        // Skip `-m` for python interpreters — that's the module flag,
        // not a model path.
        if token == "-m" && argv0_is_python {
            continue;
        }
        if MODEL_FLAGS.contains(&token.as_str())
            && let Some(next) = cmdline.get(i + 1)
            && !next.is_empty()
            && !next.starts_with('-')
        {
            return Some(strip_path_and_extension(next));
        }
    }
    None
}

/// Strip directory components and a single file extension. HuggingFace
/// IDs (`meta-llama/Llama-3-8B`) get their publisher prefix removed
/// (returns `Llama-3-8B`); paths (`/m/llama3.safetensors`) get both
/// directory and extension dropped (returns `llama3`); bare tokens
/// (`tinyllama:1.5b`) round-trip unchanged.
fn strip_path_and_extension(s: &str) -> String {
    let basename = s.rsplit('/').next().unwrap_or(s);
    let basename = basename.rsplit('\\').next().unwrap_or(basename);
    // Strip only the FINAL extension — model names like
    // "qwen2.5-0.5b-instruct-q8_0.gguf" must become
    // "qwen2.5-0.5b-instruct-q8_0", not "qwen2".
    if let Some(idx) = basename.rfind('.') {
        // Skip ext-strip when the dotted segment looks like part of a
        // version tag (e.g. `tinyllama:1.5b` shouldn't lose `5b`). A
        // pragmatic check: only strip when the post-dot suffix is
        // wholly alphabetic (txt-style extensions). This keeps
        // `Llama-3.1-8B` intact too.
        let suffix = &basename[idx + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_alphabetic()) {
            return basename[..idx].to_string();
        }
    }
    basename.to_string()
}

/// Environment variables whose presence (with a non-empty value) indicates that
/// a model path is explicitly configured for this process.
static STRONG_MODEL_ENV_VARS: &[&str] = &[
    "MODEL_PATH",
    "LLAMA_MODEL_PATH",
    "GGUF_MODEL",
    "OLLAMA_MODELS",
];

pub(crate) fn classify(sample: &ProcessSample) -> Option<ClassificationResult> {
    if let Some(path) = find_model_in_cmdline(&sample.cmdline) {
        let evidence = format!("model file referenced in cmdline: {}", path.display());
        tracing::debug!(pid = sample.pid, %evidence, "model path detected in cmdline");
        // L11a — derive the workload-type axis from the model file
        // itself: .gguf/.ggml → LLM, "yolo"/"diffusion" basenames →
        // Vision, "bge"/"minilm" → Embeddings, else Unknown.
        let workload_category = workload_category_from_model_path(&path);
        return Some(ClassificationResult::ai_with_model(
            AICategory::Inference,
            workload_category,
            evidence,
            path,
        ));
    }

    if let Some((var, value)) = find_strong_model_env_entry(&sample.environ) {
        let evidence = format!("model env var set: {}", var);
        tracing::debug!(pid = sample.pid, %evidence, "model env var detected");
        // The env-var value is typically a path (MODEL_PATH, LLAMA_MODEL_PATH,
        // GGUF_MODEL) but OLLAMA_MODELS points at a directory of models. Treat
        // anything ending in a strong extension as a path; otherwise skip the
        // model-name derivation but still classify as Inference.
        // L11a — derive workload-type from the var name as a hint:
        // LLAMA_MODEL_PATH / GGUF_MODEL / OLLAMA_MODELS all imply LLM;
        // generic MODEL_PATH falls through to path-based derivation.
        if has_strong_extension(&value) {
            let path = PathBuf::from(value);
            let workload_category = workload_category_from_var_name(&var)
                .unwrap_or_else(|| workload_category_from_model_path(&path));
            return Some(ClassificationResult::ai_with_model(
                AICategory::Inference,
                workload_category,
                evidence,
                path,
            ));
        }
        let workload_category =
            workload_category_from_var_name(&var).unwrap_or(crate::model::WorkloadCategory::Unknown);
        return Some(ClassificationResult::ai(
            AICategory::Inference,
            workload_category,
            evidence,
        ));
    }

    None
}

/// L11a — derive a workload type from a strong model env var name.
/// Returns `None` for the generic `MODEL_PATH` which is ambiguous.
fn workload_category_from_var_name(var: &str) -> Option<crate::model::WorkloadCategory> {
    match var {
        "LLAMA_MODEL_PATH" | "GGUF_MODEL" | "OLLAMA_MODELS" => {
            Some(crate::model::WorkloadCategory::LLM)
        }
        _ => None,
    }
}

/// Scans cmdline tokens for a strong-extension model file path, handling both
/// `--flag value` and `--flag=value` forms. Returns the path on a match.
pub(crate) fn find_model_in_cmdline(cmdline: &[String]) -> Option<PathBuf> {
    for (i, token) in cmdline.iter().enumerate() {
        // --flag=value form must be checked before the bare-path check: a token
        // like "--model=/models/foo.gguf" ends with a strong extension but is not
        // itself a valid path — only the value after '=' is.
        if let Some(value) = extract_flag_eq_value(token)
            && has_strong_extension(value)
        {
            return Some(PathBuf::from(value));
        }

        // --flag value form (look ahead one token)
        if MODEL_FLAGS.contains(&token.as_str())
            && let Some(next) = cmdline.get(i + 1)
            && has_strong_extension(next)
        {
            return Some(PathBuf::from(next));
        }

        // Bare path token: /models/llama3.gguf
        // Skip flag-like tokens (already handled above) to avoid false positives.
        if !token.starts_with('-') && has_strong_extension(token) {
            return Some(PathBuf::from(token));
        }
    }
    None
}

/// Returns Some(value) if `token` matches any MODEL_FLAG followed by `=`.
fn extract_flag_eq_value(token: &str) -> Option<&str> {
    MODEL_FLAGS
        .iter()
        .find_map(|&flag| token.strip_prefix(&format!("{}=", flag)))
}

pub(crate) fn has_strong_extension(path: &str) -> bool {
    let lower = path.to_lowercase();
    STRONG_MODEL_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
}

pub(crate) fn find_strong_model_env_var(environ: &HashMap<String, String>) -> Option<String> {
    find_strong_model_env_entry(environ).map(|(k, _)| k)
}

/// Returns the first (variable, value) pair that signals a configured model.
/// Preferred over `find_strong_model_env_var` when you need the path itself.
pub(crate) fn find_strong_model_env_entry(
    environ: &HashMap<String, String>,
) -> Option<(String, String)> {
    STRONG_MODEL_ENV_VARS.iter().find_map(|&var| {
        environ
            .get(var)
            .filter(|v| !v.is_empty())
            .map(|v| (var.to_string(), v.clone()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── find_model_in_cmdline ─────────────────────────────────────────────────

    #[test]
    fn bare_gguf_path_detected() {
        let cmdline = args(&["llama-server", "/models/llama3-8b.gguf"]);
        assert_eq!(
            find_model_in_cmdline(&cmdline),
            Some(PathBuf::from("/models/llama3-8b.gguf"))
        );
    }

    #[test]
    fn flag_space_value_form() {
        let cmdline = args(&[
            "llama-server",
            "--model",
            "/models/llama3.gguf",
            "-c",
            "4096",
        ]);
        assert_eq!(
            find_model_in_cmdline(&cmdline),
            Some(PathBuf::from("/models/llama3.gguf"))
        );
    }

    #[test]
    fn flag_eq_value_form() {
        let cmdline = args(&["llama-server", "--model=/models/mistral.safetensors"]);
        assert_eq!(
            find_model_in_cmdline(&cmdline),
            Some(PathBuf::from("/models/mistral.safetensors"))
        );
    }

    #[test]
    fn short_flag_m() {
        let cmdline = args(&["llama-server", "-m", "/models/phi2.gguf"]);
        assert_eq!(
            find_model_in_cmdline(&cmdline),
            Some(PathBuf::from("/models/phi2.gguf"))
        );
    }

    #[test]
    fn strong_extensions_detected() {
        for ext in STRONG_MODEL_EXTENSIONS {
            let path = format!("/models/model{}", ext);
            let cmdline = args(&["runner", &path]);
            assert!(
                find_model_in_cmdline(&cmdline).is_some(),
                "extension {:?} not detected",
                ext
            );
        }
    }

    #[test]
    fn weak_extensions_not_detected() {
        for ext in WEAK_MODEL_EXTENSIONS {
            let path = format!("/data/file{}", ext);
            let cmdline = args(&["some-tool", &path]);
            assert!(
                find_model_in_cmdline(&cmdline).is_none(),
                "weak extension {:?} should not trigger",
                ext
            );
        }
    }

    #[test]
    fn no_model_in_cmdline_returns_none() {
        let cmdline = args(&["python3", "train.py", "--lr", "0.001"]);
        assert!(find_model_in_cmdline(&cmdline).is_none());
    }

    #[test]
    fn empty_cmdline_returns_none() {
        assert!(find_model_in_cmdline(&[]).is_none());
    }

    // ── find_strong_model_env_var ─────────────────────────────────────────────

    #[test]
    fn model_path_env_var_detected() {
        let env = env_map(&[("MODEL_PATH", "/models/llama.gguf"), ("PATH", "/usr/bin")]);
        assert_eq!(
            find_strong_model_env_var(&env),
            Some("MODEL_PATH".to_string())
        );
    }

    #[test]
    fn empty_env_var_ignored() {
        let env = env_map(&[("MODEL_PATH", "")]);
        assert!(find_strong_model_env_var(&env).is_none());
    }

    #[test]
    fn framework_env_vars_not_detected() {
        // HF_HOME and TRANSFORMERS_CACHE are not strong model env vars.
        let env = env_map(&[
            ("HF_HOME", "/root/.cache/huggingface"),
            ("TRANSFORMERS_CACHE", "/tmp/models"),
        ]);
        assert!(find_strong_model_env_var(&env).is_none());
    }

    // ── classify (integration) ────────────────────────────────────────────────

    #[test]
    fn classify_finds_gguf_in_cmdline() {
        let sample = ProcessSample {
            pid: 42,
            ppid: Some(1),
            name: "llama-server".into(),
            cmdline: args(&["llama-server", "--model", "/models/llama3.gguf"]),
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };
        let result = classify(&sample).expect("should classify");
        assert_eq!(result.category, AICategory::Inference);
        assert_eq!(
            result.model_path,
            Some(PathBuf::from("/models/llama3.gguf"))
        );
        assert_eq!(result.model_name.as_deref(), Some("llama3"));
    }

    #[test]
    fn classify_extracts_model_name_from_dotted_filename() {
        // Real-world example: Qwen's file_stem is "qwen2.5-0.5b-instruct-q8_0",
        // not "qwen2" — file_stem only strips the final extension.
        let sample = ProcessSample {
            pid: 7,
            ppid: None,
            name: "llama-cli".into(),
            cmdline: args(&[
                "llama-cli",
                "-m",
                "/home/f/models/qwen2.5-0.5b-instruct-q8_0.gguf",
            ]),
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };
        let result = classify(&sample).expect("should classify");
        assert_eq!(
            result.model_name.as_deref(),
            Some("qwen2.5-0.5b-instruct-q8_0")
        );
    }

    #[test]
    fn classify_finds_strong_env_var() {
        let sample = ProcessSample {
            pid: 99,
            ppid: None,
            name: "python3".into(),
            cmdline: args(&["python3", "serve.py"]),
            environ: env_map(&[("LLAMA_MODEL_PATH", "/models/llama.gguf")]),
            cwd: None,
            ..Default::default()
        };
        let result = classify(&sample).expect("should classify");
        assert_eq!(result.category, AICategory::Inference);
        assert_eq!(result.model_path, Some(PathBuf::from("/models/llama.gguf")));
        assert_eq!(result.model_name.as_deref(), Some("llama"));
    }

    #[test]
    fn classify_ollama_models_dir_env_has_no_model_name() {
        // OLLAMA_MODELS points at a directory of models, not a single file.
        // Classification still fires (Inference) but there's no single model
        // name to surface.
        let sample = ProcessSample {
            pid: 100,
            ppid: None,
            name: "ollama".into(),
            cmdline: args(&["ollama", "serve"]),
            environ: env_map(&[("OLLAMA_MODELS", "/var/lib/ollama/models")]),
            cwd: None,
            ..Default::default()
        };
        let result = classify(&sample).expect("should classify");
        assert_eq!(result.category, AICategory::Inference);
        assert_eq!(result.model_path, None);
        assert_eq!(result.model_name, None);
    }

    #[test]
    fn classify_returns_none_for_non_ai() {
        let sample = ProcessSample {
            pid: 1,
            ppid: None,
            name: "nginx".into(),
            cmdline: args(&["nginx", "-c", "/etc/nginx.conf"]),
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };
        assert!(classify(&sample).is_none());
    }

    // ── F3 — extract_model_name (Sprint-3 model column) ─────────────

    #[test]
    fn extract_model_from_ollama_run_simple() {
        let cmdline = args(&["ollama", "run", "tinyllama"]);
        assert_eq!(
            extract_model_name(&cmdline),
            Some("tinyllama".to_string()),
        );
    }

    #[test]
    fn extract_model_from_ollama_run_with_version() {
        // Ollama tags are colon-separated; must not be misread as a
        // file extension to strip.
        let cmdline = args(&["ollama", "run", "llama3:8b-instruct-q4_0"]);
        assert_eq!(
            extract_model_name(&cmdline),
            Some("llama3:8b-instruct-q4_0".to_string()),
        );
    }

    #[test]
    fn extract_model_from_ollama_run_with_absolute_argv0() {
        // Real-world cmdline often has argv[0] as the full binary
        // path (e.g. from `which ollama`).
        let cmdline = args(&["/usr/local/bin/ollama", "run", "phi3"]);
        assert_eq!(extract_model_name(&cmdline), Some("phi3".to_string()));
    }

    #[test]
    fn extract_model_returns_none_for_ollama_serve_daemon() {
        // The daemon holds the loaded-model info in memory; cmdline
        // alone can't tell us which model is hot.
        let cmdline = args(&["ollama", "serve"]);
        assert_eq!(extract_model_name(&cmdline), None);
        // Bare `ollama` (no subcommand) also returns None.
        let cmdline = args(&["ollama"]);
        assert_eq!(extract_model_name(&cmdline), None);
    }

    #[test]
    fn extract_model_from_vllm_model_flag() {
        // HuggingFace repo ID — strip publisher prefix, keep the
        // model name verbatim (no extension to strip).
        let cmdline = args(&[
            "python3",
            "-m",
            "vllm.entrypoints.openai.api_server",
            "--model",
            "meta-llama/Llama-3-8B",
        ]);
        assert_eq!(
            extract_model_name(&cmdline),
            Some("Llama-3-8B".to_string()),
        );
    }

    #[test]
    fn extract_model_from_vllm_with_full_path() {
        // Local path with a weak extension (.bin) — drop the dir and
        // the extension. (Strong-extension paths like .safetensors
        // are handled by find_model_in_cmdline earlier in the
        // classifier pipeline and never reach this extractor.)
        let cmdline = args(&[
            "python3",
            "-m",
            "vllm.entrypoints.api_server",
            "--model",
            "/models/qwen2-7b-instruct",
        ]);
        assert_eq!(
            extract_model_name(&cmdline),
            Some("qwen2-7b-instruct".to_string()),
        );
    }

    #[test]
    fn extract_model_from_llama_cpp_gguf() {
        // `-m models/X.gguf` — strip dir AND extension. The full
        // model name preserves internal dots (qwen2.5-0.5b — see the
        // existing `classify_extracts_model_name_from_dotted_filename`
        // test); only the final ascii-alphabetic extension is
        // dropped.
        let cmdline = args(&[
            "llama-server",
            "-m",
            "/home/f/models/qwen2.5-0.5b-instruct-q8_0.gguf",
        ]);
        assert_eq!(
            extract_model_name(&cmdline),
            Some("qwen2.5-0.5b-instruct-q8_0".to_string()),
        );
    }

    #[test]
    fn extract_model_from_model_eq_value_form() {
        // `--model=value` form must work identically to `--model value`.
        let cmdline = args(&[
            "python3",
            "vllm_serve.py",
            "--model=meta-llama/Llama-3-8B",
        ]);
        assert_eq!(
            extract_model_name(&cmdline),
            Some("Llama-3-8B".to_string()),
        );
    }

    #[test]
    fn extract_model_skips_flag_value_that_looks_like_another_flag() {
        // Defensive: `--model --foo` shouldn't accept `--foo` as the
        // model name. (clap would reject this cmdline at the user's
        // process; we should match clap's strictness.)
        let cmdline = args(&[
            "python3",
            "vllm_serve.py",
            "--model",
            "--foo",
            "Llama-3-8B",
        ]);
        // The first --model is followed by --foo (rejected); the
        // extractor falls through with None rather than skipping to
        // the bare Llama-3-8B token (positional args aren't models).
        assert_eq!(extract_model_name(&cmdline), None);
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
}
