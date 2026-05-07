use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Snapshot of a running process. Produced by the platform layer (Module 2);
/// consumed entirely by the classifier in Module 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct ProcessSample {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    /// argv, including argv[0] (the executable path or name).
    pub cmdline: Vec<String>,
    /// Process environment, key=value pairs from /proc/PID/environ.
    pub environ: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// Resident set size in bytes. 0 for kernel threads and permission-denied reads.
    /// Parsed from /proc/<pid>/status VmRSS line.
    pub rss_bytes: u64,
    /// Cumulative CPU time (user+system) in clock ticks, from /proc/<pid>/stat.
    /// Raw value; per-tick CPU% is computed by the runtime against the previous
    /// sample. 0 for permission-denied reads.
    pub cpu_time_ticks: u64,
}

/// Coarse category assigned to a process by the classifier.
/// Copy so it can appear in static tables without allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub enum AICategory {
    /// Running model inference — llama-server, ollama, vllm, triton, etc.
    Inference,
    /// Training or fine-tuning — torchrun, deepspeed, trainer.train(), etc.
    Training,
    /// Downloading or managing model weights — huggingface-cli, snapshot_download, etc.
    ModelDownload,
    /// AI framework process whose purpose is unclear from process info alone —
    /// a bare `python -c "import torch"` or similar.
    Framework,
    /// Not classified as an AI workload.
    NotAi,
}

/// L11a / UX_CONTRACT.md §1 region 4 — workload **type** taxonomy
/// used by the v0.3 workloads panel for grouping and per-row metric
/// formatting (§2). Distinct from [`AICategory`] which classifies
/// workflow **phase** (Inference / Training / etc.) — the two enums
/// measure different things on different axes and are intentionally
/// kept side-by-side rather than merged.
///
/// `Training` and `ModelDownload` from [`AICategory`] don't fit this
/// taxonomy (which is about inference shape, not workflow phase) and
/// collapse to `Unknown` — a model download IS something the user
/// wants to see in "what's running" without being miscategorized as
/// inference. `Framework` (a bare `python -c "import torch"`) also
/// collapses to `Unknown`.
///
/// `ROS2` has no detection logic in L11a; L9 wires it via env vars
/// (`RMW_IMPLEMENTATION`, `ROS_DOMAIN_ID`) and `librcl.so` linkage.
/// Until then, no classifier path returns `ROS2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub enum WorkloadCategory {
    /// Text generation — Ollama, vLLM, llama.cpp, ExLlama, TGI, etc.
    LLM,
    /// Image / video / audio inference — YOLO, Ultralytics, Stable
    /// Diffusion, ComfyUI, Whisper, MediaPipe, etc.
    Vision,
    /// ROS2 robotics nodes. Detection wires in L9.
    ROS2,
    /// Text embedding generation — sentence-transformers, BAAI/bge,
    /// minilm, etc.
    Embeddings,
    /// AI process whose shape isn't one of the above (training,
    /// model download, generic framework, or AI-flavored process
    /// without a discriminating signal). Per the contract, the
    /// workloads panel still renders these — just without a
    /// type-specific primary metric.
    Unknown,
}

/// Result of classifying a single process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassificationResult {
    pub category: AICategory,
    /// L11a — contract-aligned workload type for §1 region 4 grouping
    /// and §2 per-row metric formatting. `Unknown` whenever the
    /// classifier can't disambiguate (the panel renders these without
    /// a type-specific primary metric).
    pub workload_category: WorkloadCategory,
    /// Human-readable rationale; empty when category is NotAi.
    pub evidence: String,
    /// Full weight-file path when a model was extracted from cmdline or env.
    /// None when classification came from process name / keyword heuristics
    /// that don't surface a concrete file.
    pub model_path: Option<PathBuf>,
    /// Short display name for the UI — file stem of `model_path` with the
    /// extension stripped. Pre-computed so render code stays allocation-free.
    pub model_name: Option<String>,
}

impl ClassificationResult {
    /// AI classification without a known weight file (keyword / script-sniff match).
    pub fn ai(
        category: AICategory,
        workload_category: WorkloadCategory,
        evidence: String,
    ) -> Self {
        Self {
            category,
            workload_category,
            evidence,
            model_path: None,
            model_name: None,
        }
    }

    /// AI classification with a concrete model path; `model_name` is derived
    /// from the file stem so the UI can show "qwen2.5-0.5b-instruct-q8_0"
    /// instead of the full path. `workload_category` is supplied by the
    /// caller (typically derived from the path / extension via
    /// [`workload_category_from_model_path`]) since file extensions and
    /// name patterns disambiguate LLM/Vision/Embeddings.
    pub fn ai_with_model(
        category: AICategory,
        workload_category: WorkloadCategory,
        evidence: String,
        path: PathBuf,
    ) -> Self {
        let model_name = model_name_from_path(&path);
        Self {
            category,
            workload_category,
            evidence,
            model_path: Some(path),
            model_name,
        }
    }

    pub fn not_ai() -> Self {
        Self {
            category: AICategory::NotAi,
            // L11a — `WorkloadCategory::Unknown` is the natural "not
            // applicable" value for non-AI processes; the workloads
            // panel filters on `is_ai()` before reading this field, so
            // it's never rendered for NotAi rows.
            workload_category: WorkloadCategory::Unknown,
            evidence: String::new(),
            model_path: None,
            model_name: None,
        }
    }

    pub fn is_ai(&self) -> bool {
        self.category != AICategory::NotAi
    }

    pub fn category_if_ai(&self) -> Option<AICategory> {
        if self.is_ai() {
            Some(self.category)
        } else {
            None
        }
    }
}

/// File stem without extension. A .gguf, .safetensors, etc. path like
/// `/home/f/models/qwen2.5-0.5b-instruct-q8_0.gguf` becomes
/// `qwen2.5-0.5b-instruct-q8_0`. Returns None on malformed paths (e.g. `/`).
fn model_name_from_path(path: &std::path::Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// L11a — derive a [`WorkloadCategory`] from a model file path.
///
/// Used by the model-extract classifier path (it sees a model file
/// before any other discriminating signal fires) to set the
/// contract-aligned category up front. Falls back to `Unknown` when
/// the extension / name pattern doesn't disambiguate — `.safetensors`
/// alone could be an LLM, a diffusion model, or an embedding model.
///
/// Decision rules (first match wins, on the lower-cased basename):
/// - extension `.gguf` or `.ggml` → `LLM` (these formats are
///   llama.cpp-specific and effectively LLM-only in practice)
/// - basename contains "yolo" / "diffusion" / "sdxl" / "stable" /
///   "comfyui" → `Vision`
/// - basename contains "bge" / "minilm" / "embedding" / "sentence-"
///   → `Embeddings`
/// - otherwise → `Unknown`
pub fn workload_category_from_model_path(path: &std::path::Path) -> WorkloadCategory {
    let basename_lower = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let ext_lower = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if ext_lower == "gguf" || ext_lower == "ggml" {
        return WorkloadCategory::LLM;
    }

    let vision_markers = ["yolo", "diffusion", "sdxl", "stable", "comfyui"];
    if vision_markers
        .iter()
        .any(|m| basename_lower.contains(m))
    {
        return WorkloadCategory::Vision;
    }

    let embedding_markers = ["bge", "minilm", "embedding", "sentence-"];
    if embedding_markers
        .iter()
        .any(|m| basename_lower.contains(m))
    {
        return WorkloadCategory::Embeddings;
    }

    WorkloadCategory::Unknown
}
