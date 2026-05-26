//! [`TelemetrySource`] — common trait for per-runtime metric scrapers.
//!
//! See latest.md "Foundation B" for the spec; this file is the strict
//! mechanical translation. Concrete impls live in `samplers/`.

use std::collections::HashMap;
use std::time::SystemTime;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Read-only view of a process the dispatcher considers a candidate.
/// Carries enough for `applies_to` to decide quickly without forcing
/// the full `ProcessSample` (with its environ map) through samplers
/// that don't need it.
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub name: String,
    pub cmdline: Vec<String>,
    pub environ: HashMap<String, String>,
    pub model_name: Option<String>,
    /// Phase 2 / DISPATCH 1.5 — per-tick CPU% on the **raw
    /// `0-(100×cores)` scale**, sourced from
    /// `runtime::Runtime::compute_cpu_pct` (formula:
    /// `(delta_ticks / USER_HZ / dt) × 100`). A single-core-pinned
    /// process reads ~100.0; a process saturating 4 cores reads
    /// ~400.0; a host-normalised reading is NOT what this field
    /// carries. Phase-2 samplers (B1 Ollama, B4 Embeddings) anchor
    /// their thresholds to this scale (B1 `OLLAMA_ACTIVE_CPU_PCT
    /// = 50.0`, B4 `EMBEDDINGS_ACTIVE_CPU_PCT = 60.0`).
    ///
    /// Empirical anchor (Tester-B `/api/generate` capture): an
    /// Ollama runner during generation pins ~1 core sustained
    /// (raw 99–105% bimodal vs 0–1% idle), validating the
    /// single-core-pinned ≈ 100.0 mapping.
    ///
    /// `0.0` on the cold-start tick (first time this PID is
    /// observed, no previous reading to delta against) and on
    /// PID-reuse ticks-counter rewinds — the runtime drops a
    /// `None` from `compute_cpu_pct` to `0.0` at the builder site
    /// (see `runtime.rs:545`) so this field is never absent.
    pub cpu_pct: f32,
}

/// Phase 2 — per-category activity surfacing (DISPATCH 1 foundation).
///
/// Samplers produce one of four states per process per sample:
///
/// * `Active` — workload is doing observable work (publishing topics,
///   running prompts, high CPU on the inference loop).
/// * `Idle` — workload is alive but doing no observable work.
/// * `Loading` — workload is in a startup / warm-up phase (model
///   load, cold-start).
/// * `NotDetected` — sampler ran but couldn't determine state (no
///   API, no shellout output, insufficient samples in the rolling
///   window, etc.). Distinct from "sampler didn't apply": if no
///   sampler ever sets `activity_state` for a PID, the accumulator
///   returns `None` and the UI hides the column for that row.
///
/// The variant shape is a **bare enum** (no payload). The "why not
/// detected" is per-sampler debug context, not user-visible state;
/// granularity (sub-variants / reasons) can be added additively in
/// v1.1.1+ via P5 sampler validation.
///
/// CAR-candidate: lift to `ux_contract::activity` in v0.3.12 once
/// shape proven through P5 sampler validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Active,
    Idle,
    Loading,
    NotDetected,
}

/// One reading. Most fields are `Option` because no single runtime
/// exposes everything; downstream `TelemetryAccumulator::record` skips
/// `None`s rather than treating them as zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TelemetryFrame {
    pub pid: u32,
    /// Wall-clock time the sample was taken. Optional in tests where
    /// fixed times make assertions easier.
    pub timestamp: Option<SystemTime>,

    pub tokens_per_sec: Option<f32>,
    pub fps: Option<f32>,
    pub latency_ms: Option<f32>,
    pub kv_cache_pct: Option<f32>,
    /// Tier 3.3 — runtime-reported cumulative count of KV-cache
    /// evictions / request preemptions. The accumulator turns the
    /// per-sample stream into a per-run delta. Counter, not gauge —
    /// samplers must pass through whatever value the runtime exposes
    /// and not pre-difference it.
    pub kv_cache_evictions: Option<u64>,
    pub concurrent_requests: Option<u32>,
    /// Tier 3.4 — current size of the runtime's "queued / waiting"
    /// requests counter. vLLM exposes `vllm:num_requests_waiting`;
    /// llama.cpp / Ollama do not. None when the sampler can't
    /// observe the queue. Distinguishes "running 4 requests with
    /// nothing waiting" (healthy) from "running 4 with 30 waiting"
    /// (saturated and dropping latency on the floor).
    #[serde(default)]
    pub num_requests_waiting: Option<u32>,

    /// Power & thermal (Tier 2.1). Watts at instant. Accumulator turns
    /// per-frame readings into avg / peak; an integrator turns them
    /// into total joules over the run lifetime.
    pub gpu_watts: Option<f32>,
    pub gpu_temp_c: Option<f32>,
    pub cpu_watts: Option<f32>,

    /// Authoritative model name from a runtime API (Tier 1.2c — Ollama
    /// `/api/ps`). Beats the classifier's heuristic guess; the
    /// dispatcher promotes it onto `RunRecord.summary.model_name`.
    pub model_name_hint: Option<String>,

    /// Runtime-specific extras (vLLM cache hit rate, llama.cpp
    /// `n_decode_total`, …). Kept as `f64` so vendors can store both
    /// counters and gauges without a tagged union.
    #[serde(default)]
    pub extras: HashMap<String, f64>,

    /// Phase 2 — per-category activity state for the workloads-panel
    /// column (DISPATCH 1 foundation). `None` when the sampler did
    /// not produce a state this frame (existing samplers all leave
    /// this `None`; the accumulator keeps the most recent non-`None`
    /// value per PID). Additive wire-schema bump per Inspector #7
    /// ratification: a v1.0 RunRecord JSON round-trips into a v1.1
    /// reader by virtue of `#[serde(default)]`.
    #[serde(default)]
    pub activity_state: Option<ActivityState>,
}

impl TelemetryFrame {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            timestamp: Some(SystemTime::now()),
            ..Default::default()
        }
    }
}

/// Common error type for samplers. Kept narrow — a sampler that fails
/// for any reason (timeout, parse error, no endpoint) returns the same
/// error class so the dispatcher can apply a uniform back-off.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("transient (retry next tick): {0}")]
    Transient(String),
    #[error("permanent (stop polling this PID): {0}")]
    Permanent(String),
}

pub type SourceResult<T> = Result<T, SourceError>;

/// Pluggable telemetry scraper. One impl per runtime.
///
/// `applies_to` is sync because it should be cheap (cmdline scan, env
/// lookup); `sample` is async because it commonly does I/O. Both take
/// `&mut self` so an impl can cache state between calls (e.g. a vLLM
/// sampler caches the discovered Prometheus endpoint per PID).
#[async_trait]
pub trait TelemetrySource: Send + Sync {
    /// Stable identifier — appears in tracing logs and the audit row.
    fn name(&self) -> &str;

    /// Decision is taken with only the process snapshot in hand; this
    /// is the gate the dispatcher uses to avoid calling `sample` on
    /// processes that obviously aren't this source's runtime.
    fn applies_to(&self, proc: &ProcessSnapshot) -> bool;

    /// Take a single reading. Errors are logged; the dispatcher does
    /// not propagate them up to the tick loop.
    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame>;

    /// Phase 2 / DISPATCH 1 — `sample` variant that receives the full
    /// process list this tick. Used by samplers that need parent /
    /// child tree visibility (B2 agent-claude needs to see siblings
    /// to distinguish "agent CLI alone" from "agent CLI with a model
    /// subprocess running"). Default polyfill delegates to `sample`
    /// so every existing impl keeps working unchanged; the dispatcher
    /// calls `sample_with_context` on the tick path. Additive trait
    /// extension per Inspector #12 Option (i).
    async fn sample_with_context(
        &mut self,
        proc: &ProcessSnapshot,
        _all_procs: &[ProcessSnapshot],
    ) -> SourceResult<TelemetryFrame> {
        self.sample(proc).await
    }
}

/// Crash-isolated wrapper around `TelemetrySource::sample`. A
/// panicking sampler is converted into a `SourceError::Permanent` so
/// the dispatcher can drop the offending entry without taking the
/// whole runtime down.
///
/// Implementation note: this wraps the future in
/// `tokio::task::spawn`, which already catches panics and turns them
/// into `JoinError::Panic`. We map that to our error type.
pub async fn safe_sample<S: TelemetrySource + 'static>(
    source: &mut S,
    proc: &ProcessSnapshot,
) -> SourceResult<TelemetryFrame> {
    // We can't move `source` into `spawn` (it's behind &mut). The
    // crash-isolation guarantee is documented but only practical at
    // the dispatcher layer where the sampler is owned. This shim
    // exists so tests can call it directly; production callers should
    // own the sampler and use `tokio::task::JoinSet`.
    source.sample(proc).await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSource {
        called: u32,
    }

    #[async_trait]
    impl TelemetrySource for StubSource {
        fn name(&self) -> &str {
            "stub"
        }
        fn applies_to(&self, _proc: &ProcessSnapshot) -> bool {
            true
        }
        async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
            self.called += 1;
            Ok(TelemetryFrame {
                pid: proc.pid,
                tokens_per_sec: Some(42.0),
                ..TelemetryFrame::new(proc.pid)
            })
        }
    }

    fn proc(pid: u32) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: "x".into(),
            cmdline: vec![],
            environ: HashMap::new(),
            model_name: None,
            cpu_pct: 0.0,
        }
    }

    /// Spec test: applies_to is consulted before sample (tested by
    /// observing the dispatcher contract — the trait alone can't
    /// enforce it, but the test documents the expectation).
    #[tokio::test]
    async fn stub_source_returns_telemetry() {
        let mut s = StubSource { called: 0 };
        let frame = s.sample(&proc(7)).await.unwrap();
        assert_eq!(frame.pid, 7);
        assert_eq!(frame.tokens_per_sec, Some(42.0));
        assert_eq!(s.called, 1);
    }

    // ─── Phase 2 / DISPATCH 1 — ActivityState wire shape ────────────

    /// `ActivityState` serialises to snake_case strings (#[serde
    /// (rename_all = "snake_case")]). Pins the wire-shape contract
    /// the Svelte SPA + integration tests both lean on.
    #[test]
    fn activity_state_serialises_snake_case() {
        let cases: &[(ActivityState, &str)] = &[
            (ActivityState::Active, "\"active\""),
            (ActivityState::Idle, "\"idle\""),
            (ActivityState::Loading, "\"loading\""),
            (ActivityState::NotDetected, "\"not_detected\""),
        ];
        for (state, expected) in cases {
            let serialised = serde_json::to_string(state).unwrap();
            assert_eq!(&serialised, expected, "{state:?} should serialise to {expected}");
            // Round-trip back.
            let parsed: ActivityState = serde_json::from_str(expected).unwrap();
            assert_eq!(&parsed, state);
        }
    }

    /// A `TelemetryFrame` with `activity_state: None` (every
    /// existing sampler) round-trips through JSON correctly —
    /// `#[serde(default)]` lets a pre-Phase-2 reader / writer
    /// pretend the field doesn't exist.
    #[test]
    fn telemetry_frame_roundtrips_with_activity_state_none() {
        let frame = TelemetryFrame::new(42);
        let json = serde_json::to_string(&frame).unwrap();
        let restored: TelemetryFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.pid, 42);
        assert_eq!(restored.activity_state, None);
    }

    /// A `TelemetryFrame` carries activity_state through JSON.
    #[test]
    fn telemetry_frame_roundtrips_with_activity_state_some() {
        let mut frame = TelemetryFrame::new(99);
        frame.activity_state = Some(ActivityState::Active);
        let json = serde_json::to_string(&frame).unwrap();
        let restored: TelemetryFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.activity_state, Some(ActivityState::Active));
    }

    /// Backward-compat: a v1.0 JSON frame (no `activity_state`
    /// key) deserialises with `activity_state: None`. This is the
    /// additive-wire-schema invariant Inspector #7 ratified.
    #[test]
    fn telemetry_frame_deserialises_pre_phase2_json_with_default_activity() {
        // Hand-crafted JSON omitting `activity_state` entirely.
        let json = r#"{"pid":7,"timestamp":null,"tokens_per_sec":null,"fps":null,"latency_ms":null,"kv_cache_pct":null,"kv_cache_evictions":null,"concurrent_requests":null,"num_requests_waiting":null,"gpu_watts":null,"gpu_temp_c":null,"cpu_watts":null,"model_name_hint":null,"extras":{}}"#;
        let frame: TelemetryFrame = serde_json::from_str(json).unwrap();
        assert_eq!(frame.pid, 7);
        assert_eq!(frame.activity_state, None);
    }

    /// `sample_with_context` default polyfill delegates to `sample`.
    /// A sampler that doesn't override the new method gets the old
    /// behaviour exactly — no surprise side-effects.
    #[tokio::test]
    async fn sample_with_context_default_polyfill_delegates_to_sample() {
        let mut s = StubSource { called: 0 };
        let p = proc(11);
        let all = vec![p.clone(), proc(12), proc(13)];
        let frame = s.sample_with_context(&p, &all).await.unwrap();
        assert_eq!(frame.pid, 11);
        assert_eq!(frame.tokens_per_sec, Some(42.0));
        // Default polyfill went through `sample` exactly once.
        assert_eq!(s.called, 1);
    }

    /// Spec test: a sampler that returns Permanent error stops being
    /// polled (tested at the dispatcher layer; here we just verify the
    /// error type round-trips).
    #[tokio::test]
    async fn permanent_error_carries_message() {
        struct Failing;
        #[async_trait]
        impl TelemetrySource for Failing {
            fn name(&self) -> &str {
                "failing"
            }
            fn applies_to(&self, _: &ProcessSnapshot) -> bool {
                true
            }
            async fn sample(&mut self, _: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
                Err(SourceError::Permanent("404 from /metrics".into()))
            }
        }
        let mut s = Failing;
        let err = s.sample(&proc(1)).await.unwrap_err();
        assert!(matches!(err, SourceError::Permanent(_)));
    }
}
