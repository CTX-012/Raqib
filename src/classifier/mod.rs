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
    use crate::model::{AICategory, WorkloadCategory};
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

    // ════════════════════════════════════════════════════════════════════════
    // L11a / UX_CONTRACT.md §1 region 4 — WorkloadCategory dispatch tests.
    //
    // These verify that the dispatch produces the contract-aligned
    // workload-type axis (LLM/Vision/ROS2/Embeddings/Unknown) alongside the
    // existing AICategory, with no overlap leakage between rows that
    // should map to different types.
    // ════════════════════════════════════════════════════════════════════════

    #[test]
    fn ollama_is_llm() {
        let s = sample("ollama", &["ollama", "serve"]);
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::LLM
        );
    }

    #[test]
    fn llama_cpp_is_llm() {
        let s = sample("llama-server", &["llama-server", "--port", "8080"]);
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::LLM
        );
    }

    #[test]
    fn vllm_via_cmdline_is_llm() {
        let s = sample(
            "python3",
            &["python3", "-m", "vllm.entrypoints.openai.api_server"],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::LLM
        );
    }

    #[test]
    fn yolo_via_cmdline_is_vision() {
        let s = sample(
            "python3",
            &[
                "python3",
                "-c",
                "from ultralytics import YOLO; YOLO('y.pt')",
            ],
        );
        // The cmdline keyword "ultralytics" matches before script-sniff
        // even has a chance to read disk, so this is a CMDLINE_KEYWORDS
        // hit. Still resolves to Vision.
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::Vision
        );
    }

    #[test]
    fn yolo_model_file_is_vision() {
        // Bare "yolov8n.pt" has a weak extension (.pt) and won't fire
        // the model_extract path — but a strong-extension model with
        // "yolo" in the basename does. Use .onnx to exercise that
        // path; the basename "yolov8n.onnx" hits the Vision marker
        // in workload_category_from_model_path.
        let s = sample(
            "trtllm-runner",
            &[
                "trtllm-runner",
                "--model",
                "/models/yolov8n.onnx",
            ],
        );
        // Note: trtllm-runner is also a name match for trtllm → LLM,
        // but model_extract runs FIRST in dispatch. The model_extract
        // path picks up the .onnx + "yolo" basename → Vision.
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::Vision
        );
    }

    #[test]
    fn sentence_transformers_via_cmdline_is_embeddings() {
        let s = sample(
            "python3",
            &[
                "python3",
                "-c",
                "from sentence_transformers import SentenceTransformer",
            ],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::Embeddings
        );
    }

    #[test]
    fn bge_model_path_is_embeddings() {
        // .safetensors with "bge-" in the basename → Embeddings via
        // workload_category_from_model_path's marker check.
        let s = sample(
            "python3",
            &[
                "python3",
                "serve.py",
                "--model",
                "/models/bge-large-en-v1.5.safetensors",
            ],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::Embeddings
        );
    }

    #[test]
    fn generic_torch_without_specific_marker_is_unknown() {
        // `import torch` alone is Framework on the AICategory axis
        // and Unknown on the workload-type axis — there's no
        // discriminating signal for LLM vs Vision vs Embeddings.
        let s = sample(
            "python3",
            &["python3", "-c", "import torch; print(torch.__version__)"],
        );
        // Note: `python3 -c` doesn't have a script file to sniff, so
        // this falls through to NotAi (no cmdline keyword fires for
        // bare `import torch`). Confirm the axis is Unknown either
        // way — for NotAi rows the panel filters them out.
        let result = classify_process(&s);
        assert!(
            !result.is_ai() || matches!(result.workload_category, WorkloadCategory::Unknown),
            "{result:?}"
        );
    }

    #[test]
    fn deepspeed_training_is_unknown_workload_type() {
        // Training is a workflow phase, not a workload type — the
        // workload-type axis collapses it to Unknown per the L11a
        // design comment on `WorkloadCategory`.
        let s = sample("deepspeed", &["deepspeed", "--num_gpus=4", "train.py"]);
        let result = classify_process(&s);
        assert_eq!(result.category, AICategory::Training);
        assert_eq!(result.workload_category, WorkloadCategory::Unknown);
    }

    #[test]
    fn ros2_processes_are_not_yet_detected() {
        // Defensive — L9 wires ROS2 detection. Until then no
        // classifier path returns ROS2; an rclpy-flavored process
        // either falls through to NotAi or, if a generic Python
        // import keyword fires, lands in Unknown. This test pins
        // "no ROS2 leakage from existing signals" so a future
        // accidental match doesn't slip in before L9.
        let s = sample_with_env(
            "python3",
            &["python3", "perception_node.py"],
            &[
                ("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp"),
                ("ROS_DOMAIN_ID", "0"),
            ],
        );
        let result = classify_process(&s);
        assert_ne!(
            result.workload_category,
            WorkloadCategory::ROS2,
            "ROS2 must not fire from any classifier path until L9 wires detection"
        );
    }

    #[test]
    fn workload_category_from_gguf_basename_is_llm() {
        use crate::model::workload_category_from_model_path;
        use std::path::Path;
        assert_eq!(
            workload_category_from_model_path(Path::new("/models/llama3-8b.gguf")),
            WorkloadCategory::LLM
        );
        assert_eq!(
            workload_category_from_model_path(Path::new("/models/phi3-mini.ggml")),
            WorkloadCategory::LLM
        );
    }

    #[test]
    fn workload_category_from_yolo_basename_is_vision() {
        use crate::model::workload_category_from_model_path;
        use std::path::Path;
        assert_eq!(
            workload_category_from_model_path(Path::new("/models/yolov8n.pt")),
            WorkloadCategory::Vision
        );
        assert_eq!(
            workload_category_from_model_path(Path::new("/models/sdxl-1.0.safetensors")),
            WorkloadCategory::Vision
        );
    }

    #[test]
    fn workload_category_from_ambiguous_safetensors_is_unknown() {
        // .safetensors alone doesn't disambiguate — could be LLM,
        // diffusion, or embeddings. Without a marker in the
        // basename, fall through to Unknown so the panel groups it
        // honestly rather than guessing wrong.
        use crate::model::workload_category_from_model_path;
        use std::path::Path;
        assert_eq!(
            workload_category_from_model_path(Path::new("/models/some-model.safetensors")),
            WorkloadCategory::Unknown
        );
    }
}
