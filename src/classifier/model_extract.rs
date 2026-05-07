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
