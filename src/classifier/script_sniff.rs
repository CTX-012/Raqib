use std::path::{Path, PathBuf};

use crate::model::{AICategory, ClassificationResult, ProcessSample, WorkloadCategory};

/// Limits how much of a script is read so a 500 MB generated file doesn't stall
/// the classifier tick. Real AI import blocks appear in the first few KB anyway.
const MAX_SCRIPT_READ_BYTES: u64 = 65_536; // 64 KiB

/// Python-constructor shapes that take a model literal as their first arg or
/// a named `model_path=`/`model=` kwarg. Matched against source lines so the
/// classifier can surface the actual weight file (e.g. "yolov8n.pt",
/// "mistral-7b-instruct-q4.gguf") instead of a generic category name.
static MODEL_LITERAL_CALLS: &[&str] = &[
    "YOLO(",
    "Llama(",
    "AutoModelForCausalLM.from_pretrained(",
    "AutoModelForSeq2SeqLM.from_pretrained(",
    "AutoModel.from_pretrained(",
    "pipeline(",
    "whisper.load_model(",
    "torch.load(",
    "onnxruntime.InferenceSession(",
    "from_pretrained(",
];

/// L11a — `(pattern, AICategory, WorkloadCategory)` triple. See
/// `keyword_match.rs` and `model::WorkloadCategory` for the
/// rationale on the dual-axis taxonomy. Matched line-by-line
/// (trimmed) against script source. More specific patterns first
/// so the returned evidence is maximally descriptive.
static AI_PATTERNS: &[(&str, AICategory, WorkloadCategory)] = &[
    // HuggingFace LLM model loading — high confidence
    ("AutoModelForCausalLM", AICategory::Inference, WorkloadCategory::LLM),
    ("AutoModelForSeq2SeqLM", AICategory::Inference, WorkloadCategory::LLM),
    // Generic AutoModel could be LLM, embeddings, or vision — Unknown.
    ("AutoModel.from_pretrained", AICategory::Inference, WorkloadCategory::Unknown),
    ("pipeline(", AICategory::Inference, WorkloadCategory::Unknown),
    // vLLM
    ("from vllm", AICategory::Inference, WorkloadCategory::LLM),
    ("import vllm", AICategory::Inference, WorkloadCategory::LLM),
    // LlamaCPP
    ("from llama_cpp", AICategory::Inference, WorkloadCategory::LLM),
    ("import llama_cpp", AICategory::Inference, WorkloadCategory::LLM),
    // Ultralytics / YOLO
    ("from ultralytics", AICategory::Inference, WorkloadCategory::Vision),
    ("import ultralytics", AICategory::Inference, WorkloadCategory::Vision),
    ("YOLO(", AICategory::Inference, WorkloadCategory::Vision),
    // Sentence-transformers / embedding models
    ("from sentence_transformers", AICategory::Inference, WorkloadCategory::Embeddings),
    ("import sentence_transformers", AICategory::Inference, WorkloadCategory::Embeddings),
    ("SentenceTransformer(", AICategory::Inference, WorkloadCategory::Embeddings),
    // ONNX Runtime — agnostic; could be LLM, Vision, or Embeddings.
    ("onnxruntime.InferenceSession", AICategory::Inference, WorkloadCategory::Unknown),
    ("import onnxruntime", AICategory::Inference, WorkloadCategory::Unknown),
    // TensorRT — same; agnostic.
    ("import tensorrt", AICategory::Inference, WorkloadCategory::Unknown),
    // Whisper (speech-to-text) — Vision tier per the perceptual-model bucket.
    ("whisper.load_model", AICategory::Inference, WorkloadCategory::Vision),
    ("import whisper", AICategory::Inference, WorkloadCategory::Vision),
    // Diffusion models → Vision
    ("from diffusers", AICategory::Inference, WorkloadCategory::Vision),
    ("import diffusers", AICategory::Inference, WorkloadCategory::Vision),
    // Generic torch model loading — agnostic.
    ("torch.load(", AICategory::Inference, WorkloadCategory::Unknown),
    ("tf.saved_model", AICategory::Inference, WorkloadCategory::Unknown),
    // HuggingFace (generic — lower precedence than specific calls above)
    ("from transformers", AICategory::Framework, WorkloadCategory::Unknown),
    ("import transformers", AICategory::Framework, WorkloadCategory::Unknown),
    // Training-specific patterns — collapse to Unknown on the
    // workload-type axis (training is a phase, not a type).
    ("from deepspeed", AICategory::Training, WorkloadCategory::Unknown),
    ("import deepspeed", AICategory::Training, WorkloadCategory::Unknown),
    ("torch.distributed", AICategory::Training, WorkloadCategory::Unknown),
    ("trainer.train()", AICategory::Training, WorkloadCategory::Unknown),
    ("model.fit(", AICategory::Training, WorkloadCategory::Unknown),
    // Torch / TF framework presence — Unknown.
    ("import torch", AICategory::Framework, WorkloadCategory::Unknown),
    ("from torch", AICategory::Framework, WorkloadCategory::Unknown),
    ("import tensorflow", AICategory::Framework, WorkloadCategory::Unknown),
    ("from tensorflow", AICategory::Framework, WorkloadCategory::Unknown),
    ("import jax", AICategory::Framework, WorkloadCategory::Unknown),
    ("from jax", AICategory::Framework, WorkloadCategory::Unknown),
    // LangChain → LLM (its primary use case is LLM orchestration).
    ("from langchain", AICategory::Framework, WorkloadCategory::LLM),
    ("import langchain", AICategory::Framework, WorkloadCategory::LLM),
];

/// Scans `content` for AI usage patterns.
///
/// Pattern priority wins over line order: if the file contains both a low-priority
/// `import torch` (Framework) on line 1 and a high-priority `pipeline(` (Inference)
/// on line 10, the result is Inference. AI_PATTERNS is ordered from most to least
/// specific, so iterating patterns first and lines second gives the right semantics.
pub(crate) fn analyze_script_content(
    content: &str,
) -> Option<(AICategory, WorkloadCategory, String)> {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    AI_PATTERNS
        .iter()
        .find_map(|&(pattern, category, workload_category)| {
            lines
                .iter()
                .any(|line| line.contains(pattern))
                .then(|| {
                    (
                        category,
                        workload_category,
                        format!("script contains {:?}", pattern),
                    )
                })
        })
}

/// Returns the script path for Python interpreter processes.
/// Returns None for `python -c "..."`, `python -m module`, and non-Python processes.
pub(crate) fn python_script_path(sample: &ProcessSample) -> Option<&Path> {
    if !is_python_interpreter(&sample.name) {
        return None;
    }
    // argv[1] is the script when it doesn't start with '-'
    let argv1 = sample.cmdline.get(1)?;
    if argv1.starts_with('-') {
        return None;
    }
    let path = Path::new(argv1.as_str());
    // Only descend into .py files; other extensions aren't Python source
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
    {
        Some(path)
    } else {
        None
    }
}

pub(crate) fn is_python_interpreter(name: &str) -> bool {
    name == "python"
        || name == "python3"
        || name.starts_with("python3.")
        || name.starts_with("python2.")
}

pub(crate) fn classify(sample: &ProcessSample) -> Option<ClassificationResult> {
    let script_path = python_script_path(sample)?;

    let content = read_limited(script_path, MAX_SCRIPT_READ_BYTES)
        .map_err(|e| {
            tracing::debug!(
                pid = sample.pid,
                path = %script_path.display(),
                error = %e,
                "could not read script for sniffing"
            );
            e
        })
        .ok()?;

    let (category, workload_category, evidence) = analyze_script_content(&content)?;
    tracing::debug!(
        pid = sample.pid,
        script = %script_path.display(),
        %evidence,
        "script sniff classified process"
    );

    // If the same script embeds a literal model path / identifier we prefer
    // the richer classification so the UI shows e.g. "yolov8n" rather than a
    // bare "Inference" category. The model-literal path may also refine the
    // workload type (e.g. a "yolov8n.pt" literal in an `AutoModel` script
    // upgrades the Unknown classification to Vision); keep whichever is
    // more specific.
    if let Some(model_literal) = extract_model_literal(&content) {
        let path = resolve_model_path(&model_literal, sample.cwd.as_deref(), script_path);
        let evidence = format!("{} + model literal `{}`", evidence, model_literal);
        let workload_from_path = crate::model::workload_category_from_model_path(&path);
        // Prefer the per-pattern workload_category when it's
        // already specific (LLM/Vision/Embeddings); fall back to
        // the path-derived one if the pattern was Unknown.
        let resolved_workload = if matches!(workload_category, WorkloadCategory::Unknown) {
            workload_from_path
        } else {
            workload_category
        };
        return Some(ClassificationResult::ai_with_model(
            category,
            resolved_workload,
            evidence,
            path,
        ));
    }

    Some(ClassificationResult::ai(
        category,
        workload_category,
        evidence,
    ))
}

/// Scans `content` for the first literal-string argument passed to a known
/// AI constructor (YOLO, Llama, pipeline, from_pretrained, ...). Returns the
/// literal as-is so the caller can decide whether to treat it as a filesystem
/// path, a HuggingFace repo id, or an Ollama tag.
pub(crate) fn extract_model_literal(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for call in MODEL_LITERAL_CALLS {
            if let Some(after) = line.find(call) {
                let tail = &line[after + call.len()..];
                if let Some(lit) = first_string_literal(tail) {
                    return Some(lit.to_string());
                }
            }
        }
    }
    None
}

/// Grabs the first `"..."` or `'...'` literal from `src`, ignoring escaped
/// quotes inside the literal. Returns None if no matching pair is found.
fn first_string_literal(src: &str) -> Option<&str> {
    let mut chars = src.char_indices();
    let (start_byte, quote) = loop {
        match chars.next()? {
            (i, c) if c == '"' || c == '\'' => break (i + c.len_utf8(), c),
            _ => continue,
        }
    };
    let mut prev = '\0';
    for (i, c) in chars {
        if c == quote && prev != '\\' {
            return Some(&src[start_byte..i]);
        }
        prev = c;
    }
    None
}

/// Resolves a model literal to a `PathBuf` the UI can display. Relative paths
/// are joined against the process cwd (or the script's directory as a
/// fallback) so the file-stem extraction yields the real model filename.
/// Repo-style identifiers ("meta-llama/Llama-3-8B") stay as-is — we
/// intentionally keep the slash so the UI can distinguish them.
fn resolve_model_path(literal: &str, cwd: Option<&Path>, script: &Path) -> PathBuf {
    let path = PathBuf::from(literal);
    if path.is_absolute() {
        return path;
    }
    if let Some(cwd) = cwd {
        return cwd.join(&path);
    }
    if let Some(parent) = script.parent() {
        return parent.join(&path);
    }
    path
}

fn read_limited(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buf = String::new();
    file.take(max_bytes).read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    // ── analyze_script_content ────────────────────────────────────────────────

    #[test]
    fn detects_torch_import() {
        let src = "import os\nimport torch\nmodel = torch.nn.Linear(10, 1)\n";
        let (cat, _, _) = analyze_script_content(src).expect("should detect");
        assert_eq!(cat, AICategory::Framework);
    }

    #[test]
    fn detects_pipeline_call() {
        let src = "from transformers import AutoTokenizer\npipe = pipeline(\"text-generation\", model=\"gpt2\")\n";
        let (cat, _, _) = analyze_script_content(src).expect("should detect");
        assert_eq!(cat, AICategory::Inference);
    }

    #[test]
    fn detects_from_transformers_import() {
        let src = "from transformers import AutoTokenizer, AutoModelForCausalLM\n";
        // AutoModelForCausalLM appears in the import → Inference beats Framework
        let (cat, _, _) = analyze_script_content(src).expect("should detect");
        assert_eq!(cat, AICategory::Inference);
    }

    #[test]
    fn detects_llama_cpp() {
        let src = "from llama_cpp import Llama\nllm = Llama(model_path=\"model.gguf\")\n";
        let (cat, _, _) = analyze_script_content(src).expect("should detect");
        assert_eq!(cat, AICategory::Inference);
    }

    #[test]
    fn detects_deepspeed_training() {
        let src = "import deepspeed\nengine, _, _, _ = deepspeed.initialize(model=model)\n";
        let (cat, _, _) = analyze_script_content(src).expect("should detect");
        assert_eq!(cat, AICategory::Training);
    }

    #[test]
    fn comments_are_ignored() {
        let src = "# import torch\n# from transformers import pipeline\nprint('hello')\n";
        assert!(analyze_script_content(src).is_none());
    }

    #[test]
    fn blank_lines_are_ignored() {
        let src = "\n\n   \nimport math\n";
        assert!(analyze_script_content(src).is_none());
    }

    #[test]
    fn non_ai_script_returns_none() {
        let src = "import os\nimport sys\nprint(sys.argv)\n";
        assert!(analyze_script_content(src).is_none());
    }

    // ── python_script_path ────────────────────────────────────────────────────

    #[test]
    fn extracts_py_script_path() {
        let sample = sample(
            "python3",
            &["python3", "/home/user/train.py", "--epochs", "10"],
        );
        let path = python_script_path(&sample).expect("should find script");
        assert_eq!(path, Path::new("/home/user/train.py"));
    }

    #[test]
    fn python_c_flag_returns_none() {
        let sample = sample("python3", &["python3", "-c", "import torch"]);
        assert!(python_script_path(&sample).is_none());
    }

    #[test]
    fn python_m_flag_returns_none() {
        let sample = sample("python3", &["python3", "-m", "vllm.entrypoints.api_server"]);
        assert!(python_script_path(&sample).is_none());
    }

    #[test]
    fn non_py_argv1_returns_none() {
        let sample = sample("python3", &["python3", "script.sh"]);
        assert!(python_script_path(&sample).is_none());
    }

    #[test]
    fn non_python_process_returns_none() {
        let sample = sample("node", &["node", "server.py"]);
        assert!(python_script_path(&sample).is_none());
    }

    // ── is_python_interpreter ─────────────────────────────────────────────────

    #[test]
    fn python_variants_recognised() {
        assert!(is_python_interpreter("python"));
        assert!(is_python_interpreter("python3"));
        assert!(is_python_interpreter("python3.11"));
        assert!(is_python_interpreter("python3.12"));
        assert!(is_python_interpreter("python2.7"));
    }

    #[test]
    fn non_python_names_rejected() {
        assert!(!is_python_interpreter("ruby"));
        assert!(!is_python_interpreter("node"));
        assert!(!is_python_interpreter("pythonista")); // not a prefix match for "python3."
        // "pythonista" starts with "python" but none of our conditions match it
        assert!(!is_python_interpreter(""));
    }

    // ── extract_model_literal ─────────────────────────────────────────────────

    #[test]
    fn extracts_yolo_literal() {
        let src = "from ultralytics import YOLO\nmodel = YOLO(\"yolov8n.pt\")\n";
        assert_eq!(extract_model_literal(src).as_deref(), Some("yolov8n.pt"));
    }

    #[test]
    fn extracts_llama_cpp_kwarg_literal() {
        let src = "from llama_cpp import Llama\nllm = Llama(model_path='/models/phi3-mini.gguf')\n";
        assert_eq!(
            extract_model_literal(src).as_deref(),
            Some("/models/phi3-mini.gguf")
        );
    }

    #[test]
    fn extracts_from_pretrained_repo_id() {
        let src = "from transformers import AutoModelForCausalLM\nm = AutoModelForCausalLM.from_pretrained(\"meta-llama/Llama-3-8B\")\n";
        assert_eq!(
            extract_model_literal(src).as_deref(),
            Some("meta-llama/Llama-3-8B")
        );
    }

    #[test]
    fn extracts_whisper_size() {
        let src = "import whisper\nmodel = whisper.load_model('small.en')\n";
        assert_eq!(extract_model_literal(src).as_deref(), Some("small.en"));
    }

    #[test]
    fn comment_lines_do_not_yield_literal() {
        let src = "# YOLO(\"ghost.pt\")\nprint('hello')\n";
        assert!(extract_model_literal(src).is_none());
    }

    #[test]
    fn returns_none_without_constructor() {
        let src = "x = \"yolov8n.pt\"\n";
        assert!(extract_model_literal(src).is_none());
    }

    // ── full classify() path with model literal surfaces model_name ───────────

    #[test]
    fn classify_script_exposes_model_name_for_yolo() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("infer.py");
        let mut f = std::fs::File::create(&script).unwrap();
        writeln!(
            f,
            "from ultralytics import YOLO\nmodel = YOLO(\"yolov8n.pt\")\n"
        )
        .unwrap();

        let sample = ProcessSample {
            pid: 7,
            ppid: Some(1),
            name: "python3".into(),
            cmdline: vec!["python3".into(), script.to_string_lossy().into_owned()],
            environ: HashMap::new(),
            cwd: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let result = classify(&sample).expect("should classify");
        assert_eq!(result.category, AICategory::Inference);
        assert_eq!(result.model_name.as_deref(), Some("yolov8n"));
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn sample(name: &str, argv: &[&str]) -> ProcessSample {
        ProcessSample {
            pid: 1,
            ppid: None,
            name: name.into(),
            cmdline: argv.iter().map(|s| s.to_string()).collect(),
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        }
    }
}
