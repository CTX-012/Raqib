mod keyword_match;
mod model_extract;
mod script_sniff;

use crate::model::{ClassificationResult, ProcessSample};

/// Classifies a process sample into an AI workload category.
///
/// Priority order (first match wins):
/// 1. Model file in cmdline or strong model env var → Inference (most specific signal)
/// 2. Known AI process name → category from NAME_KEYWORDS table
/// 3. AI keyword in any cmdline token → category from CMDLINE_KEYWORDS table
/// 4. Python script source contains AI import/call → category from AI_PATTERNS
/// 5. NotAi
pub fn classify_process(sample: &ProcessSample) -> ClassificationResult {
    if let Some(result) = model_extract::classify(sample) {
        return result;
    }
    if let Some(result) = keyword_match::classify_by_name(&sample.name) {
        return result;
    }
    if let Some(result) = keyword_match::classify_by_cmdline(&sample.cmdline) {
        return result;
    }
    if let Some(result) = script_sniff::classify(sample) {
        return result;
    }
    ClassificationResult::not_ai()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AICategory;
    use pretty_assertions::assert_eq;
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

    fn sample_with_env(name: &str, argv: &[&str], env: &[(&str, &str)]) -> ProcessSample {
        ProcessSample {
            environ: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..sample(name, argv)
        }
    }

    // ── model file in cmdline ─────────────────────────────────────────────────

    #[test]
    fn gguf_model_arg_is_inference() {
        let s = sample(
            "llama-server",
            &["llama-server", "--model", "/models/llama3.gguf"],
        );
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    #[test]
    fn safetensors_model_arg_is_inference() {
        let s = sample(
            "python3",
            &["python3", "serve.py", "--model=/models/mistral.safetensors"],
        );
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    #[test]
    fn tensorrt_engine_is_inference() {
        let s = sample(
            "trtllm-runner",
            &["trtllm-runner", "--engine", "/engines/llama.engine"],
        );
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    // ── model env var ─────────────────────────────────────────────────────────

    #[test]
    fn model_path_env_var_is_inference() {
        let s = sample_with_env(
            "python3",
            &["python3", "serve.py"],
            &[("MODEL_PATH", "/models/phi2.gguf")],
        );
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    // ── process name keywords ─────────────────────────────────────────────────

    #[test]
    fn ollama_process_is_inference() {
        let s = sample("ollama", &["ollama", "serve"]);
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    #[test]
    fn deepspeed_process_is_training() {
        let s = sample("deepspeed", &["deepspeed", "--num_gpus=4", "train.py"]);
        assert_eq!(classify_process(&s).category, AICategory::Training);
    }

    // ── cmdline keyword ───────────────────────────────────────────────────────

    #[test]
    fn vllm_in_python_module_path_is_inference() {
        let s = sample(
            "python3",
            &["python3", "-m", "vllm.entrypoints.openai.api_server"],
        );
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    #[test]
    fn whisper_in_cmdline_is_inference() {
        let s = sample("python3", &["python3", "run_whisper.py", "--model", "tiny"]);
        assert_eq!(classify_process(&s).category, AICategory::Inference);
    }

    // ── non-AI processes ──────────────────────────────────────────────────────

    #[test]
    fn nginx_is_not_ai() {
        let s = sample("nginx", &["nginx", "-c", "/etc/nginx/nginx.conf"]);
        assert_eq!(classify_process(&s).category, AICategory::NotAi);
    }

    #[test]
    fn bash_is_not_ai() {
        let s = sample("bash", &["bash", "--login"]);
        assert_eq!(classify_process(&s).category, AICategory::NotAi);
    }

    #[test]
    fn sshd_is_not_ai() {
        let s = sample("sshd", &["sshd", "-D"]);
        assert_eq!(classify_process(&s).category, AICategory::NotAi);
    }

    // ── evidence field is populated on AI matches ─────────────────────────────

    #[test]
    fn ai_result_has_non_empty_evidence() {
        let s = sample("ollama", &["ollama", "serve"]);
        let result = classify_process(&s);
        assert!(!result.evidence.is_empty());
    }

    #[test]
    fn not_ai_result_has_empty_evidence() {
        let s = sample("bash", &["bash"]);
        let result = classify_process(&s);
        assert!(result.evidence.is_empty());
    }
}
