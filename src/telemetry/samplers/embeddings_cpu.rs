//! Embeddings activity sampler (Phase 2 / DISPATCH 2B / B4).
//!
//! Embeddings workloads (sentence-transformers, BGE, ONNX runtime
//! used for vector encoding, etc.) don't expose a Prometheus
//! endpoint and don't have a daemon-style API to poll. The activity
//! signal is the workload's own CPU% on the raw `0-(100×cores)`
//! scale plumbed through [`ProcessSnapshot::cpu_pct`] by DISPATCH
//! 1.5 — pure compute, no new I/O, no shellout.
//!
//! Heuristic: embeddings work is **bursty**. A short batch encodes
//! in ~100-300 ms and the process goes idle between requests.
//! Sampling once per 1 Hz tick and gating on a single point would
//! miss many active windows. Instead we keep a rolling window of
//! the last `EMBEDDINGS_WINDOW_SAMPLES` CPU% readings per PID and
//! report:
//!
//! ```text
//! max(window) ≥ EMBEDDINGS_ACTIVE_CPU_PCT  → ActivityState::Active
//! otherwise                                → ActivityState::Idle
//! ```
//!
//! ## Thresholds
//!
//! `EMBEDDINGS_ACTIVE_CPU_PCT = 60.0` — PROVISIONAL: refined
//! post-v1.1.0 sampler validation (v1.1.1). Foundation-pinned in
//! the `ProcessSnapshot::cpu_pct` doc-comment (`src/telemetry/
//! source.rs:31`) ahead of B4 implementation. No Tester-B-style
//! empirical capture exists for embeddings at v1.1.0 — the
//! threshold is calibrated against the assumption that a busy
//! embeddings inference loop pins ≥0.6 cores during the burst.
//! Adjust in v1.1.1 if P5 validation surfaces a different band.
//!
//! `EMBEDDINGS_WINDOW_SAMPLES = 3` — PROVISIONAL. The original
//! DISPATCH 2A draft called for "any of last 3 ticks" Active
//! detection because embeddings bursts are typically short. At
//! 1 Hz that's a 3-second window. A future deployment with shorter
//! bursts may need a longer (5-tick) window or per-PID adaptive
//! sizing — surfaced as a CAR-candidate alongside the threshold.
//!
//! ## Detection (`applies_to`)
//!
//! `ProcessSnapshot` does not carry `workload_category`
//! (foundation kept it minimal). B4 mirrors the embeddings
//! signals the classifier uses
//! (`src/classifier/script_sniff.rs`): cmdline / argv tokens that
//! indicate sentence-transformers, BGE / GTE / E5 model paths,
//! the `sentence-transformers` CLI, or python invocations
//! against known embeddings entry points. Library-signal-only
//! embeddings workloads (rare) are NOT covered by v1.1.0 B4;
//! v1.1.1+ can plumb `workload_category` onto `ProcessSnapshot`
//! to close this gap — same acceptance note as B3's
//! ROS_DOMAIN_ID-free workloads.
//!
//! ## Per-PID state lifecycle (v1.1.1 candidate)
//!
//! Same shape as B2: the dispatcher does not invoke a per-PID
//! cleanup hook on `SourceError::Permanent`, so `per_pid_cpu_window`
//! entries accumulate until the source struct is dropped. Bounded
//! by ring-buffer size (`EMBEDDINGS_WINDOW_SAMPLES * 4 bytes` per
//! observed embeddings PID) plus HashMap overhead; not a memory
//! crisis. PROVISIONAL: refined post-v1.1.0 sampler validation
//! (v1.1.1).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;

use async_trait::async_trait;

use crate::telemetry::source::{
    ActivityState, ProcessSnapshot, SourceResult, TelemetryFrame, TelemetrySource,
};

/// PROVISIONAL: refined post-v1.1.0 sampler validation (v1.1.1).
/// Foundation-pinned in `ProcessSnapshot::cpu_pct` doc-comment at
/// raw `0-(100×cores)` scale.
const EMBEDDINGS_ACTIVE_CPU_PCT: f32 = 60.0;

/// PROVISIONAL: refined post-v1.1.0 sampler validation (v1.1.1).
/// Rolling-window size used to absorb burstiness.
const EMBEDDINGS_WINDOW_SAMPLES: usize = 3;

/// Cmdline substring markers indicating an embeddings workload.
/// Lowercased + substring-matched against the joined cmdline. Kept
/// narrow: the classifier's stricter signals at
/// `src/classifier/script_sniff.rs` are the authoritative source;
/// these are the cmdline-observable subset reachable at sampler
/// dispatch time without a `workload_category` field on
/// `ProcessSnapshot`.
const EMBEDDINGS_CMDLINE_MARKERS: &[&str] = &[
    "sentence_transformers",
    "sentence-transformers",
    "bge-",
    "gte-",
    "e5-",
];

pub struct EmbeddingsCpuSource {
    /// PID → ring buffer of the last
    /// `EMBEDDINGS_WINDOW_SAMPLES` CPU% readings. Pushed at every
    /// `sample` invocation; older readings drop off the front.
    per_pid_cpu_window: HashMap<u32, VecDeque<f32>>,
}

impl EmbeddingsCpuSource {
    pub fn new() -> Self {
        Self {
            per_pid_cpu_window: HashMap::new(),
        }
    }

    /// Push `cpu_pct` onto the window for `pid`, evicting the
    /// oldest reading when the window is full. Returns the
    /// window's `max` after the push.
    fn push_and_max(&mut self, pid: u32, cpu_pct: f32) -> f32 {
        let window = self.per_pid_cpu_window.entry(pid).or_default();
        if window.len() == EMBEDDINGS_WINDOW_SAMPLES {
            window.pop_front();
        }
        window.push_back(cpu_pct);
        window.iter().copied().fold(0.0_f32, f32::max)
    }
}

impl Default for EmbeddingsCpuSource {
    fn default() -> Self {
        Self::new()
    }
}

/// True when `cmdline` carries a token / substring that the
/// classifier would treat as an embeddings signal. Matches the
/// `script_sniff` markers reachable via cmdline (file paths,
/// `python -m` invocations against known modules). Library-only
/// signal coverage is deliberately not duplicated here.
fn is_embeddings_cmdline(cmdline: &[String]) -> bool {
    if cmdline.is_empty() {
        return false;
    }
    let joined = cmdline.join(" ").to_ascii_lowercase();
    if EMBEDDINGS_CMDLINE_MARKERS
        .iter()
        .any(|m| joined.contains(m))
    {
        return true;
    }
    // `python -m sentence_transformers …` is the most common live
    // shape; explicit token scan covers cases where the joined
    // lowercase substring scan above missed (e.g. quoting that
    // splits the package name across argv elements).
    cmdline.iter().enumerate().any(|(i, arg)| {
        if arg == "-m"
            && let Some(next) = cmdline.get(i + 1)
        {
            let lowered = next.to_ascii_lowercase();
            return lowered.starts_with("sentence_transformers")
                || lowered.starts_with("sentence-transformers");
        }
        // Path-style markers: a model file argument like
        // `models/bge-large-en-v1.5/` or `--model bge-base-en`.
        let basename = Path::new(arg)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(arg)
            .to_ascii_lowercase();
        EMBEDDINGS_CMDLINE_MARKERS
            .iter()
            .any(|m| basename.contains(m))
    })
}

#[async_trait]
impl TelemetrySource for EmbeddingsCpuSource {
    fn name(&self) -> &str {
        "embeddings-cpu"
    }

    fn applies_to(&self, proc: &ProcessSnapshot) -> bool {
        is_embeddings_cmdline(&proc.cmdline)
    }

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        let max_in_window = self.push_and_max(proc.pid, proc.cpu_pct);
        let activity = if max_in_window >= EMBEDDINGS_ACTIVE_CPU_PCT {
            ActivityState::Active
        } else {
            ActivityState::Idle
        };
        Ok(TelemetryFrame {
            pid: proc.pid,
            activity_state: Some(activity),
            ..TelemetryFrame::new(proc.pid)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdMap;

    fn snap(pid: u32, cpu_pct: f32, cmdline: &[&str]) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: "python3".into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            environ: StdMap::new(),
            model_name: None,
            cpu_pct,
            ppid: None,
        }
    }

    /// `python -m sentence_transformers …` is the typical live
    /// invocation. Joined-substring scan catches it.
    #[test]
    fn applies_to_python_dash_m_sentence_transformers() {
        let s = EmbeddingsCpuSource::new();
        let p = snap(
            100,
            0.0,
            &["python3", "-m", "sentence_transformers", "encode"],
        );
        assert!(s.applies_to(&p));
    }

    /// `--model bge-…` style invocation against a model path.
    #[test]
    fn applies_to_bge_model_path() {
        let s = EmbeddingsCpuSource::new();
        let p = snap(
            100,
            0.0,
            &[
                "python3",
                "encode.py",
                "--model",
                "/models/bge-large-en-v1.5",
            ],
        );
        assert!(s.applies_to(&p));
    }

    /// Non-embeddings python invocations must NOT classify.
    #[test]
    fn applies_to_rejects_non_embeddings_python() {
        let s = EmbeddingsCpuSource::new();
        let p = snap(100, 0.0, &["python3", "train.py", "--lr", "0.001"]);
        assert!(!s.applies_to(&p));
    }

    /// Empty cmdline (kernel thread shape) returns false.
    #[test]
    fn applies_to_empty_cmdline_returns_false() {
        let s = EmbeddingsCpuSource::new();
        let p = snap(0, 0.0, &[]);
        assert!(!s.applies_to(&p));
    }

    /// A single high-CPU sample fires Active immediately — the
    /// rolling-window max is the high reading on the first push.
    #[tokio::test]
    async fn sample_high_cpu_emits_active() {
        let mut s = EmbeddingsCpuSource::new();
        let p = snap(100, 95.0, &["python3", "-m", "sentence_transformers"]);
        let frame = s.sample(&p).await.expect("sample should succeed");
        assert_eq!(frame.activity_state, Some(ActivityState::Active));
    }

    /// A sustained low-CPU run emits Idle.
    #[tokio::test]
    async fn sample_low_cpu_emits_idle() {
        let mut s = EmbeddingsCpuSource::new();
        let p = snap(100, 0.5, &["python3", "-m", "sentence_transformers"]);
        let frame = s.sample(&p).await.expect("sample should succeed");
        assert_eq!(frame.activity_state, Some(ActivityState::Idle));
    }

    /// Burst absorption: a single spike inside the last 3 ticks
    /// keeps the row Active even after the spike returns to idle.
    /// This is the design rationale for the rolling window — a
    /// single-point gate would have flipped to Idle on the second
    /// tick.
    #[tokio::test]
    async fn sample_burst_held_active_through_window() {
        let mut s = EmbeddingsCpuSource::new();
        let cmdline = &["python3", "-m", "sentence_transformers"];

        // Tick 1: burst (95% — well above threshold).
        let f = s
            .sample(&snap(100, 95.0, cmdline))
            .await
            .expect("burst sample");
        assert_eq!(f.activity_state, Some(ActivityState::Active));

        // Tick 2: idle (0.5%) — but window still has the burst.
        let f = s
            .sample(&snap(100, 0.5, cmdline))
            .await
            .expect("idle after burst");
        assert_eq!(
            f.activity_state,
            Some(ActivityState::Active),
            "single-burst Active must persist while the spike is in window",
        );

        // Tick 3: still idle — the burst is at the front of the
        // 3-sample window. Next tick will evict it.
        let f = s
            .sample(&snap(100, 0.5, cmdline))
            .await
            .expect("second idle");
        assert_eq!(f.activity_state, Some(ActivityState::Active));

        // Tick 4: the burst rolls off → Idle.
        let f = s
            .sample(&snap(100, 0.5, cmdline))
            .await
            .expect("burst rolled off");
        assert_eq!(f.activity_state, Some(ActivityState::Idle));
    }

    /// Per-PID isolation: window state for PID A does NOT bleed
    /// into PID B. Two concurrent embeddings workloads.
    #[tokio::test]
    async fn sample_per_pid_window_isolation() {
        let mut s = EmbeddingsCpuSource::new();
        let cmdline = &["python3", "-m", "sentence_transformers"];

        // PID 100 bursts.
        let f = s
            .sample(&snap(100, 95.0, cmdline))
            .await
            .expect("PID A burst");
        assert_eq!(f.activity_state, Some(ActivityState::Active));

        // PID 200 idles — must NOT be infected by PID 100's burst.
        let f = s
            .sample(&snap(200, 0.5, cmdline))
            .await
            .expect("PID B idle");
        assert_eq!(
            f.activity_state,
            Some(ActivityState::Idle),
            "PID B's window must not inherit PID A's burst",
        );
    }

    /// Threshold boundary: exactly at the locked
    /// `EMBEDDINGS_ACTIVE_CPU_PCT` (60.0) fires Active.
    #[tokio::test]
    async fn sample_at_threshold_emits_active() {
        let mut s = EmbeddingsCpuSource::new();
        let p = snap(100, 60.0, &["python3", "-m", "sentence_transformers"]);
        let frame = s.sample(&p).await.expect("at-threshold sample");
        assert_eq!(frame.activity_state, Some(ActivityState::Active));
    }

    /// Constants pin: foundation doc-comment named
    /// `EMBEDDINGS_ACTIVE_CPU_PCT = 60.0`. A casual refactor that
    /// lowers the threshold without re-pinning the doc-comment
    /// trips this test first.
    #[test]
    fn locked_constants_match_doc_comment_anchor() {
        assert_eq!(EMBEDDINGS_ACTIVE_CPU_PCT, 60.0);
        assert_eq!(EMBEDDINGS_WINDOW_SAMPLES, 3);
    }
}
