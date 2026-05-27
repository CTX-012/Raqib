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
//! flicker Active↔Idle on every inter-burst gap. Instead we hold
//! Active for a duration-based window after the last
//! above-threshold reading per PID:
//!
//! ```text
//! cpu_pct ≥ EMBEDDINGS_ACTIVE_CPU_PCT            → Active (refresh window)
//! else, within EMBEDDINGS_IDLE_WINDOW of last    → Active (burst-tolerance)
//!   above-threshold reading
//! else                                           → Idle
//! ```
//!
//! ## Thresholds
//!
//! `EMBEDDINGS_ACTIVE_CPU_PCT = 60.0` — VALIDATED (P5 DISPATCH 9B).
//! Tester-B confirmed the CPU-percent signal is correct: idle
//! embeddings workloads read ~0% and active ones read 170-800% on
//! the raw `0-(100×cores)` scale, so 60.0 sits well inside the
//! empty band between idle and active. Foundation-pinned in the
//! `ProcessSnapshot::cpu_pct` doc-comment (`src/telemetry/
//! source.rs`). Preserved unchanged from v1.1.2.
//!
//! `EMBEDDINGS_IDLE_WINDOW = 12 s` — v1.1.3 (P5 DISPATCH 9B),
//! replaces the v1.1.2 `EMBEDDINGS_WINDOW_SAMPLES = 3` count-based
//! window. EMPIRICAL: bursty embeddings workloads show ~0.4 s
//! active spikes with ~5 s inter-burst gaps. The 3-sample (~3 s
//! at 1 Hz) window could not bridge a 5 s gap, so it flickered
//! Active↔Idle every gap. A 12 s duration hold-window bridges the
//! realistic burst cadence while still transitioning to Idle for
//! genuinely-idle workloads. Shape borrowed from B2's
//! `AGENT_IDLE_WINDOW` (60 s for claude tool-call patterns; 12 s
//! here for embeddings' tighter cadence). PROVISIONAL only insofar
//! as a future deployment with a different burst rhythm may want a
//! different window — the signal + magnitude are validated.
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
//! ## Per-PID state lifecycle (v1.1.x candidate)
//!
//! Same shape as B2: the dispatcher does not invoke a per-PID
//! cleanup hook on `SourceError::Permanent`, so `last_active_at`
//! entries accumulate until the source struct is dropped. Bounded
//! slow leak: ~50 bytes per observed embeddings PID; not a memory
//! crisis. Deferred to the same dispatcher-cleanup-hook refinement
//! tracked for B2.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::telemetry::source::{
    ActivityState, ProcessSnapshot, SourceResult, TelemetryFrame, TelemetrySource,
};

/// VALIDATED (P5 DISPATCH 9B). Raw `0-(100×cores)` scale; idle
/// embeddings ~0%, active 170-800%, so 60.0 is firmly in the empty
/// band. Preserved unchanged from v1.1.2.
const EMBEDDINGS_ACTIVE_CPU_PCT: f32 = 60.0;

/// v1.1.3 (P5 DISPATCH 9B) — duration hold-window. After an
/// above-threshold reading, Active is held for this long so the
/// ~5 s inter-burst gaps in real embeddings workloads don't
/// flicker the row to Idle. Replaces the v1.1.2 count-based
/// `EMBEDDINGS_WINDOW_SAMPLES = 3` (~3 s at 1 Hz, too short to
/// bridge the gaps). Shape mirrors B2's `AGENT_IDLE_WINDOW`.
const EMBEDDINGS_IDLE_WINDOW: Duration = Duration::from_secs(12);

/// Cmdline substring markers indicating an embeddings workload.
/// Lowercased + substring-matched against the joined cmdline. Kept
/// narrow: the classifier's stricter signals at
/// `src/classifier/script_sniff.rs` are the authoritative source;
/// these are the cmdline-observable subset reachable at sampler
/// dispatch time without a `workload_category` field on
/// `ProcessSnapshot`.
// v1.1.4 P5-B4-CLASSIFY — kept in sync with the classifier's
// embeddings keyword coverage (src/classifier/keyword_match.rs) so a
// process the classifier tags as Embeddings is also one this sampler
// will sample. Added flagembedding / nomic-embed / all-minilm /
// jina-embeddings to match the classifier.
const EMBEDDINGS_CMDLINE_MARKERS: &[&str] = &[
    "sentence_transformers",
    "sentence-transformers",
    "flagembedding",
    "bge-",
    "gte-",
    "e5-base",
    "e5-large",
    "e5-small",
    "multilingual-e5",
    "nomic-embed",
    "all-minilm",
    "jina-embeddings",
];

pub struct EmbeddingsCpuSource {
    /// PID → last `Instant` the workload read at or above
    /// `EMBEDDINGS_ACTIVE_CPU_PCT`. Active is held for
    /// `EMBEDDINGS_IDLE_WINDOW` after this timestamp so burst gaps
    /// don't flicker the row. Same shape as B2's `last_active_at`.
    last_active_at: HashMap<u32, Instant>,
}

impl EmbeddingsCpuSource {
    pub fn new() -> Self {
        Self {
            last_active_at: HashMap::new(),
        }
    }

    /// v1.1.3 — apply the bimodal threshold + duration hold-window
    /// for one PID at time `now`. Split out (with an injectable
    /// `now`) so the burst-pattern tests can drive simulated time
    /// without real sleeps, the same way B2's idle-window tests
    /// backdate `last_active_at`.
    fn classify_at(&mut self, pid: u32, cpu_pct: f32, now: Instant) -> ActivityState {
        if cpu_pct >= EMBEDDINGS_ACTIVE_CPU_PCT {
            self.last_active_at.insert(pid, now);
            ActivityState::Active
        } else {
            match self.last_active_at.get(&pid) {
                Some(last) if now.saturating_duration_since(*last) < EMBEDDINGS_IDLE_WINDOW => {
                    // Below threshold but inside the hold-window —
                    // a burst gap, not genuine idle.
                    ActivityState::Active
                }
                _ => ActivityState::Idle,
            }
        }
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
        let activity = self.classify_at(proc.pid, proc.cpu_pct, Instant::now());
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

    /// Threshold boundary: exactly at the locked
    /// `EMBEDDINGS_ACTIVE_CPU_PCT` (60.0) fires Active.
    #[tokio::test]
    async fn sample_at_threshold_emits_active() {
        let mut s = EmbeddingsCpuSource::new();
        let p = snap(100, 60.0, &["python3", "-m", "sentence_transformers"]);
        let frame = s.sample(&p).await.expect("at-threshold sample");
        assert_eq!(frame.activity_state, Some(ActivityState::Active));
    }

    /// Constants pin: `EMBEDDINGS_ACTIVE_CPU_PCT = 60.0` (VALIDATED,
    /// preserved) + `EMBEDDINGS_IDLE_WINDOW = 12 s` (v1.1.3 P5
    /// refinement). A refactor that drifts either trips this first.
    #[test]
    fn locked_constants_match_p5_refinement() {
        assert_eq!(EMBEDDINGS_ACTIVE_CPU_PCT, 60.0);
        assert_eq!(EMBEDDINGS_IDLE_WINDOW, Duration::from_secs(12));
    }

    // ─────────────────────────────────────────────────────────────
    // v1.1.3 (P5 DISPATCH 9B) — duration hold-window tests. These
    // drive `classify_at` with a simulated `now` so burst patterns
    // spanning many seconds run without real sleeps (same approach
    // as B2's idle-window tests backdating `last_active_at`).
    // ─────────────────────────────────────────────────────────────

    /// Test 1 — Active held across burst gaps inside the 12 s
    /// window. Burst at t=0, then sub-threshold at t=2/5/10 s — all
    /// still Active because each is within 12 s of the t=0 burst.
    #[tokio::test]
    async fn sample_active_during_burst_window() {
        let mut s = EmbeddingsCpuSource::new();
        let t0 = Instant::now();
        // Burst.
        assert_eq!(s.classify_at(100, 200.0, t0), ActivityState::Active);
        // Gaps within the 12 s hold-window.
        for secs in [2u64, 5, 10] {
            let t = t0 + Duration::from_secs(secs);
            assert_eq!(
                s.classify_at(100, 0.0, t),
                ActivityState::Active,
                "t={secs}s is within the 12 s hold-window — must stay Active",
            );
        }
    }

    /// Test 2 — Idle after the hold-window expires. Burst at t=0,
    /// sustained idle, checked at t=15 s (> 12 s) → Idle.
    #[tokio::test]
    async fn sample_idle_after_hold_window() {
        let mut s = EmbeddingsCpuSource::new();
        let t0 = Instant::now();
        assert_eq!(s.classify_at(100, 200.0, t0), ActivityState::Active);
        let t15 = t0 + Duration::from_secs(15);
        assert_eq!(
            s.classify_at(100, 0.0, t15),
            ActivityState::Idle,
            "15 s > 12 s hold-window — must transition to Idle",
        );
    }

    /// Test 3 — continuous Active. Sustained high CPU for 30 s
    /// always reads Active (threshold magnitude check still works).
    #[tokio::test]
    async fn sample_continuous_active() {
        let mut s = EmbeddingsCpuSource::new();
        let t0 = Instant::now();
        for secs in [0u64, 5, 10, 15, 20, 25, 30] {
            let t = t0 + Duration::from_secs(secs);
            assert_eq!(
                s.classify_at(100, 400.0, t),
                ActivityState::Active,
                "sustained 400% must always be Active (t={secs}s)",
            );
        }
    }

    /// Test 4 — REGRESSION-PIN for v1.1.2 → v1.1.3. Five burst
    /// cycles (200% spike, then 0% two seconds later) over ~25 s.
    /// Must emit Active throughout — NEVER Idle during the bursts.
    ///
    /// With the v1.1.2 3-sample (~3 s) window, the 0% reading 2 s
    /// after each spike, followed by the next sample, rolled the
    /// spike off the window and flickered the row to Idle in each
    /// ~5 s gap. With the v1.1.3 12 s hold-window, the row holds
    /// Active across the whole burst train.
    #[tokio::test]
    async fn sample_no_flicker_under_burst_pattern() {
        let mut s = EmbeddingsCpuSource::new();
        let t0 = Instant::now();
        // 5 cycles, each: spike at 5k seconds, idle at 5k+2 seconds.
        for cycle in 0u64..5 {
            let spike_t = t0 + Duration::from_secs(cycle * 5);
            let gap_t = t0 + Duration::from_secs(cycle * 5 + 2);
            assert_eq!(
                s.classify_at(100, 200.0, spike_t),
                ActivityState::Active,
                "spike at cycle {cycle} must be Active",
            );
            assert_eq!(
                s.classify_at(100, 0.0, gap_t),
                ActivityState::Active,
                "gap at cycle {cycle} (2 s after spike, well inside 12 s \
                 hold-window) must stay Active — v1.1.2's 3-sample window \
                 flickered to Idle here",
            );
        }
    }

    /// Per-PID isolation under the new hold-window: PID A's burst
    /// must not hold PID B Active. (Preserved invariant from v1.1.2,
    /// re-expressed against `classify_at`.)
    #[tokio::test]
    async fn hold_window_is_per_pid() {
        let mut s = EmbeddingsCpuSource::new();
        let t0 = Instant::now();
        assert_eq!(s.classify_at(100, 200.0, t0), ActivityState::Active);
        // PID 200 idle at the same instant — no prior burst of its
        // own → Idle.
        assert_eq!(
            s.classify_at(200, 0.0, t0),
            ActivityState::Idle,
            "PID B has no burst of its own — A's hold-window must not leak",
        );
    }
}
