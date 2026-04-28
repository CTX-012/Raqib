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

/// Result of classifying a single process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassificationResult {
    pub category: AICategory,
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
    pub fn ai(category: AICategory, evidence: String) -> Self {
        Self {
            category,
            evidence,
            model_path: None,
            model_name: None,
        }
    }

    /// AI classification with a concrete model path; `model_name` is derived
    /// from the file stem so the UI can show "qwen2.5-0.5b-instruct-q8_0"
    /// instead of the full path.
    pub fn ai_with_model(category: AICategory, evidence: String, path: PathBuf) -> Self {
        let model_name = model_name_from_path(&path);
        Self {
            category,
            evidence,
            model_path: Some(path),
            model_name,
        }
    }

    pub fn not_ai() -> Self {
        Self {
            category: AICategory::NotAi,
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
