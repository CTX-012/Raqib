mod keyword_match;
mod model_extract;
mod ros2;
mod saas_llm;
mod script_sniff;

use crate::model::{ClassificationResult, ProcessSample};

/// Classifies a process sample into an AI workload category.
///
/// Priority order (first match wins):
/// 0. **ROS2 signals** (env vars / cmdline / linked libraries) →
///    `WorkloadCategory::ROS2`. Runs first so a perception node that
///    also imports `torch` for inference still classifies as ROS2 —
///    grouping it under LLM would hide it from the operator's ROS2
///    section per UX_CONTRACT.md §1 region 4.
/// 1. Model file in cmdline or strong model env var → Inference (most specific signal)
/// 2. **SaaS-LLM CLI** path fragment (Claude Code, Cursor, Aider,
///    Continue) → Inference + LLM. Sits between `model_extract` and
///    `NAME_KEYWORDS` because these processes have no local model
///    file (model_extract correctly skips) and run as bare `node` or
///    `python` (NAME_KEYWORDS correctly skips) — only the publisher-
///    qualified path fragment is a reliable signal.
/// 3. Known AI process name → category from NAME_KEYWORDS table
/// 4. AI keyword in any cmdline token → category from CMDLINE_KEYWORDS table
/// 5. Python script source contains AI import/call → category from AI_PATTERNS
/// 6. NotAi
pub fn classify_process(sample: &ProcessSample) -> ClassificationResult {
    if let Some(result) = ros2::classify(sample) {
        return result;
    }
    if let Some(result) = model_extract::classify(sample) {
        return result;
    }
    if let Some(result) = saas_llm::classify(sample) {
        return result;
    }
    if let Some(result) = keyword_match::classify_by_name(&sample.name) {
        return augment_with_model_name(result, sample);
    }
    if let Some(result) = keyword_match::classify_by_cmdline(&sample.cmdline) {
        return augment_with_model_name(result, sample);
    }
    if let Some(result) = script_sniff::classify(sample) {
        return result;
    }
    ClassificationResult::not_ai()
}

/// F3 — when a keyword-based classification fires for an LLM workload
/// (ollama, vLLM, llama-server, …) but no `model_name` was populated
/// (because the cmdline didn't carry a strong-extension file path),
/// run the dedicated runtime extractors at
/// [`model_extract::extract_model_name`]. They know about
/// `ollama run <model>` and `--model <HF-id-or-path>` shapes that the
/// strong-extension path detector doesn't catch. Non-LLM workloads
/// pass through unchanged.
fn augment_with_model_name(
    mut result: ClassificationResult,
    sample: &ProcessSample,
) -> ClassificationResult {
    use crate::model::WorkloadCategory;
    if result.workload_category == WorkloadCategory::LLM && result.model_name.is_none() {
        result.model_name = model_extract::extract_model_name(&sample.cmdline);
    }
    result
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

    // ── B5 — vLLM + path-marker collision (Sprint-2 investigation) ──
    //
    // model_extract::classify runs before cmdline keyword matching, so
    // when a vLLM cmdline passes a strong-extension model path the
    // workload_category is derived from the basename. Pre-fix the
    // basename markers ran [Vision, Embeddings, else Unknown]; an LLM
    // family marker now runs first so `stable-beluga-13b.safetensors`
    // (a real LLM that contains the "stable" Vision substring) stays
    // LLM instead of misclassifying as Vision.

    #[test]
    fn vllm_safetensors_no_marker_classifies_correctly() {
        // Plain LLM filename + no path marker → LLM via the family
        // pass. Pre-fix the basename had no marker and returned
        // Unknown; the new "llama" / "qwen" / etc. pre-check resolves
        // it to LLM directly.
        let s = sample(
            "python3",
            &[
                "python3",
                "-m",
                "vllm.entrypoints.openai.api_server",
                "--model",
                "/m/llama-3-8b.safetensors",
            ],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::LLM,
        );
    }

    #[test]
    fn vllm_safetensors_with_stable_in_path_does_not_classify_vision() {
        // The original B5 failure case: Stable Beluga is an LLM but
        // the basename contains "stable" (a Vision marker). The
        // LLM-family marker "beluga" runs before the Vision check
        // and pins this to LLM.
        let s = sample(
            "python3",
            &[
                "python3",
                "-m",
                "vllm.entrypoints.openai.api_server",
                "--model",
                "/m/stable-beluga-13b.safetensors",
            ],
        );
        let wc = classify_process(&s).workload_category;
        assert_ne!(
            wc,
            WorkloadCategory::Vision,
            "Stable Beluga is an LLM; the 'stable' Vision marker must \
             not win against the 'beluga' LLM marker"
        );
        assert_eq!(wc, WorkloadCategory::LLM);
    }

    #[test]
    fn vllm_safetensors_with_llama_basename_classifies_llm() {
        // Sanity check for the family-marker pass: a bare "llama"
        // basename (no Vision-marker interference) still routes to
        // LLM through the new pre-check, NOT through the .gguf shortcut
        // or the keyword pass.
        let s = sample(
            "python3",
            &[
                "python3",
                "-m",
                "vllm.entrypoints.openai.api_server",
                "--model",
                "/models/llama3-instruct.safetensors",
            ],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::LLM,
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

    // L9 — ROS2 detection lives in `classifier/ros2.rs` and runs
    // first in dispatch. The L11a "no leakage" defensive guard is
    // retired; the tests below cover the real signals.

    // v1.1.4 P5-ENV-ROS — flip of the prior "ROS_DOMAIN_ID alone
    // classifies as ROS2" test, mirroring the Fix-1 flip done for
    // RMW_IMPLEMENTATION just below. Tester-B verified (P5 DISPATCH
    // 9B) that a process merely INHERITING ROS_DOMAIN_ID from a
    // ROS-sourced shell was false-classified ROS2. With no cmdline
    // marker and no library signal, ROS_DOMAIN_ID alone must no
    // longer classify — it's now necessary-but-not-sufficient.
    #[test]
    fn ros_domain_id_env_alone_does_not_classify_as_ros2() {
        let s = sample_with_env(
            "python3",
            &["python3", "perception_node.py"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        assert_ne!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2,
            "ROS_DOMAIN_ID inherited from a ROS-sourced shell must not \
             classify a bare python script as ROS2 (P5-ENV-ROS)",
        );
    }

    // Fix-1 — flip of the prior "RMW_IMPLEMENTATION alone classifies
    // as ROS2" test. The audit found that `RMW_IMPLEMENTATION` is set
    // by `/opt/ros/<distro>/setup.bash` and inherited by every child
    // process, so trusting it standalone misclassified user shells,
    // browsers, and CLI tools (including the Claude Code agents
    // running this session). With Fix-1 it requires cmdline OR
    // library corroboration to count.
    #[test]
    fn rmw_implementation_alone_does_not_classify_as_ros2() {
        let s = sample_with_env(
            "rclcpp_component_container",
            // Note: the process name `rclcpp_component_container` is
            // ITSELF a cmdline marker per `ROS2_CMDLINE_MARKERS`.
            // To isolate the env-only path we use a non-matching
            // cmdline.
            &["some-binary"],
            &[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")],
        );
        assert_ne!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2,
            "RMW_IMPLEMENTATION alone must not classify as ROS2 \
             (set by setup.bash; every shell child inherits it)",
        );
    }

    #[test]
    fn rmw_implementation_plus_cmdline_marker_classifies_as_ros2() {
        // The intended ROS2 path: env var is corroborated by the
        // standalone-trustworthy cmdline marker. The cmdline alone
        // would have classified it anyway; this test pins that the
        // env-var presence doesn't accidentally suppress classification.
        let s = sample_with_env(
            "rclcpp_component_container",
            &["rclcpp_component_container", "--ros-args"],
            &[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2
        );
    }

    #[test]
    fn ros_distro_alone_does_not_classify_as_ros2() {
        let s = sample_with_env("bash", &["bash"], &[("ROS_DISTRO", "humble")]);
        assert_ne!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2,
        );
    }

    // Fix-1 + Fix-2 acceptance test — the user-facing case the
    // audit surfaced. A Claude Code CLI process inherits the
    // user-shell ROS env (because the user's `.bashrc` sources
    // `/opt/ros/humble/setup.bash`) but is not actually a ROS2
    // node.
    //
    // Linux base merge sweep #1 / post-merge update: prior to the
    // merge, this test asserted `AICategory::NotAi` because Fix-1
    // alone only prevents the ROS misclassification; nothing on
    // the wp5 branch positively identified Claude Code as AI. Post-
    // merge, Fix-2 (`saas_llm::classify`, from l14) recognises the
    // `cli.js` path and promotes the process to AI/LLM. The
    // combined behaviour is the user-visible target: Claude Code
    // appears in the AI section, NOT the ROS section.
    //
    // The Fix-1-only invariant we keep asserting is the negative:
    // `WorkloadCategory != ROS2`. The Fix-2 invariant we layer on
    // is the positive: `AICategory::Inference + WorkloadCategory::LLM`.
    #[test]
    fn claude_code_with_inherited_ros_env_classified_as_ai_agent_not_ros2() {
        use crate::model::AICategory;
        let s = sample_with_env(
            "node",
            &[
                "node",
                "/home/u/.vscode-server/extensions/anthropic.claude-code/cli.js",
            ],
            &[
                ("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp"),
                ("ROS_DISTRO", "humble"),
                ("AMENT_PREFIX_PATH", "/opt/ros/humble"),
                ("ROS_VERSION", "2"),
            ],
        );
        let result = classify_process(&s);
        // Fix-1 invariant: setup.bash-inherited env vars are not
        // standalone-trustworthy ROS2 signals.
        assert_ne!(
            result.workload_category,
            WorkloadCategory::ROS2,
            "Fix-1 invariant: setup.bash-inherited env must not \
             classify a non-ROS process as ROS2",
        );
        // Fix-2 invariant: the cli.js path is a SaaS-LLM marker.
        assert_eq!(
            result.category,
            AICategory::Inference,
            "Fix-2 invariant: Claude Code's cli.js path should \
             classify as AI inference",
        );
        // Sprint-7.5 / CAR-18 — was LLM pre-7.5, now Agent.
        assert_eq!(
            result.workload_category,
            WorkloadCategory::Agent,
            "Sprint-7.5: Claude Code's workload category is Agent",
        );
    }

    #[test]
    fn ros2_cli_command_classified_as_ros2() {
        let s = sample("ros2", &["ros2", "run", "demo_nodes_cpp", "talker"]);
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2
        );
    }

    #[test]
    fn ros2_priority_over_torch_imports() {
        // A perception node that ALSO has torch loaded in the
        // process — without ROS2 priority, the script-sniff
        // classifier would pick up a generic torch signal and
        // mis-classify. With ROS2 detection running first in
        // dispatch and a REAL ROS2 signal present (the `rclpy`
        // cmdline marker), the row groups under ROS2.
        //
        // v1.1.4 P5-ENV-ROS — the fixture now carries a real
        // cmdline signal (`-m rclpy.node`) instead of relying on
        // ROS_DOMAIN_ID alone, which no longer classifies. This
        // still exercises the dispatch-order contract (ROS2 before
        // the torch/script-sniff classifier) — the point of the
        // test — without depending on the removed env-alone path.
        let s = sample_with_env(
            "python3",
            &["python3", "-m", "rclpy.node", "perception_node.py"],
            &[
                ("ROS_DOMAIN_ID", "0"),
                ("PYTHONPATH", "/opt/ros/humble/lib/python3.10/site-packages"),
            ],
        );
        assert_eq!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2
        );
    }

    #[test]
    fn non_ros2_process_with_python_not_classified_as_ros2() {
        // A regular Python ML process must fall through to its
        // existing classifier (Inference / Framework / etc.) —
        // ROS2 detection mustn't false-positive on bare Python.
        let s = sample("python3", &["python3", "train.py"]);
        assert_ne!(
            classify_process(&s).workload_category,
            WorkloadCategory::ROS2
        );
    }

    #[test]
    fn missing_ros2_signals_falls_through_to_other_classifier() {
        // An LLM process (Ollama) without any ROS2 signals must
        // continue to land in `WorkloadCategory::LLM`. Confirms
        // the dispatch fall-through path the L9 reordering
        // preserves.
        let s = sample("ollama", &["ollama", "serve"]);
        let result = classify_process(&s);
        assert_eq!(result.category, AICategory::Inference);
        assert_eq!(result.workload_category, WorkloadCategory::LLM);
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

    // ════════════════════════════════════════════════════════════════════════
    // Fix 2 — SaaS-LLM CLI recognition dispatch tests.
    //
    // The saas_llm module has its own unit tests for the pattern
    // matcher; these verify the dispatch order — running between
    // model_extract and NAME_KEYWORDS — actually routes Claude Code
    // / Cursor / Aider as Inference + LLM end-to-end.
    // ════════════════════════════════════════════════════════════════════════

    #[test]
    fn claude_code_node_dispatch_classifies_as_inference_agent() {
        // Sprint-7.5 / CAR-18 — claude-code lands in Agent, not LLM.
        // Pre-fix (Sprint-1 audit) the symptom was `node`-from-VS-Code
        // falling through to NotAi; that fix arrived as
        // `WorkloadCategory::LLM`. The Sprint-7.5 follow-up swaps
        // LLM → Agent so the dashboard's LLM subsection stays for
        // inference servers (ollama/vllm/llama.cpp) only.
        let s = sample(
            "node",
            &[
                "node",
                "/home/faiz/.vscode-server/extensions/anthropic.claude-code-2.1.0/cli.js",
            ],
        );
        let r = classify_process(&s);
        assert_eq!(r.category, AICategory::Inference);
        assert_eq!(r.workload_category, WorkloadCategory::Agent);
        assert!(r.evidence.contains("SaaS-LLM CLI"));
    }

    #[test]
    fn cursor_dispatch_classifies_as_inference_agent() {
        let s = sample(
            "node",
            &[
                "node",
                "/root/.vscode-server/extensions/cursor-0.5.0/extension.js",
            ],
        );
        let r = classify_process(&s);
        assert!(r.is_ai(), "cursor must classify as AI");
        assert_eq!(r.workload_category, WorkloadCategory::Agent);
    }

    #[test]
    fn aider_dispatch_classifies_as_inference_agent() {
        let s = sample("aider-chat", &["aider-chat", "--model", "claude-3.5"]);
        let r = classify_process(&s);
        assert!(r.is_ai());
        assert_eq!(r.workload_category, WorkloadCategory::Agent);
    }

    #[test]
    fn continue_dispatch_classifies_as_inference_agent() {
        // Sprint-7.5 — `continue.continue` was added to the
        // SaaS-LLM allowlist in Sprint-1 Fix-2. Pin that it lands
        // in Agent like the other developer-assistant CLIs.
        let s = sample(
            "node",
            &[
                "node",
                "/home/dev/.vscode-server/extensions/continue.continue-0.9.0/out/extension.js",
            ],
        );
        let r = classify_process(&s);
        assert_eq!(r.workload_category, WorkloadCategory::Agent);
    }

    #[test]
    fn saas_llm_classification_leaves_model_name_unset() {
        // Sprint-7.5 Fix 1 — the SaaS-LLM classifier must NOT
        // populate model_name. The augment_with_model_name post-pass
        // ONLY fires for LLM category; routing claude to Agent
        // bypasses it, but the classifier's own
        // `ClassificationResult::ai()` constructor also leaves
        // model_name `None`. Pin both invariants so a future
        // refactor can't accidentally inject a category-tag string
        // (the user-reported "agents" / similar regression).
        let s = sample(
            "node",
            &[
                "node",
                "/home/faiz/.vscode-server/extensions/anthropic.claude-code-2.1.0/cli.js",
            ],
        );
        let r = classify_process(&s);
        assert_eq!(r.workload_category, WorkloadCategory::Agent);
        assert!(
            r.model_name.is_none(),
            "Agent-classified workload must not carry a model_name; \
             got {:?}",
            r.model_name
        );
    }

    // v1.1.4 P5-ENV-ROS — flip of the prior "ROS2 env priority beats
    // saas_llm" test. The old fixture (a claude-code agent that
    // merely INHERITED ROS_DOMAIN_ID from a ROS-sourced shell
    // classifying as ROS2) was EXACTLY the env-inheritance
    // false-positive this release fixes — and exactly the dispatch's
    // severity rationale ("non-ROS AI workloads inheriting the env
    // misclassify"). A claude agent with only inherited ROS_DOMAIN_ID
    // and no real ROS2 signal must now classify as the Agent
    // (saas_llm) workload, NOT ROS2.
    #[test]
    fn saas_llm_wins_over_inherited_ros_domain_id() {
        let s = sample_with_env(
            "node",
            &[
                "node",
                "/home/dev/.vscode-server/extensions/anthropic.claude-code/cli.js",
            ],
            &[("ROS_DOMAIN_ID", "0")],
        );
        let r = classify_process(&s);
        assert_eq!(
            r.workload_category,
            WorkloadCategory::Agent,
            "a claude agent that merely inherited ROS_DOMAIN_ID must \
             classify as Agent, not ROS2 (P5-ENV-ROS): {r:?}",
        );
    }

    #[test]
    fn saas_llm_does_not_override_model_extract_priority() {
        // model_extract sits at priority 1. A process whose cmdline
        // contains BOTH a SaaS-LLM extension path AND a concrete
        // model file should classify by the model file — the file
        // is the more specific signal, and saas_llm running second
        // means we never overwrite a real model-path match.
        let s = sample(
            "node",
            &[
                "node",
                "/home/dev/.vscode-server/extensions/anthropic.claude-code/cli.js",
                "--model",
                "/models/llama3.gguf",
            ],
        );
        let r = classify_process(&s);
        // model_extract wins → model_name is populated; saas_llm
        // would have left model_name None.
        assert!(
            r.model_name.is_some(),
            "model_extract priority must win when both signals fire: {r:?}"
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
