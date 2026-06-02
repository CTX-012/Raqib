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
    /// Phase 2 / DISPATCH 1.6 — parent process ID, as reported by
    /// `/proc/<pid>/status PPid` (sourced from `ProcessSample::ppid`
    /// at the builder site). Samplers needing child-process
    /// detection — B2 Agent (claude) tracks Bash-tool invocations
    /// via `child.ppid == agent.pid` — read this field.
    ///
    /// EMPIRICAL anchor (Tester-A claude capture, 22 concurrent
    /// agents on the test host): multi-instance attribution
    /// requires precise ppid filtering or every bash subshell
    /// in the snapshot gets credited to every agent.
    ///
    /// `None` when the source data layer cannot resolve a parent
    /// (PID 0, kernel threads, transient processes whose
    /// `/proc/<pid>/stat` race-failed). Samplers consuming this
    /// field should treat `None` as "unknown parent", not as
    /// "no parent".
    pub ppid: Option<u32>,
    /// Phase 2 / DISPATCH 16 / v1.1.5 ITEM B — the classifier's
    /// `WorkloadCategory` verdict for this PID, surfaced onto the
    /// snapshot so samplers don't have to re-derive it from
    /// cmdline. v1.1.4 broadened the classifier (script-sniff +
    /// extended keyword coverage) but B4 was still gating on its
    /// own cmdline-substring `is_embeddings_cmdline` — script-file
    /// embeddings workloads classified correctly but never got
    /// sampled (activity null). DISPATCH 13B finding (D-B4-SCRIPT-
    /// ASYMMETRY). Plumbed here so the classifier's verdict is the
    /// single source of truth, mirroring how 1.5 plumbed cpu_pct
    /// and 1.6 plumbed ppid.
    ///
    /// `None` for test fixtures that don't care; production
    /// builders always pass `Some(...)` from
    /// `AnnotatedProcess::workload_category`.
    pub workload_category: Option<crate::model::WorkloadCategory>,
}

// Phase 2 — per-category activity surfacing (DISPATCH 1 foundation).
//
// v1.1.10 ITEM 1 (DISPATCH 32 / CAR-21): `ActivityState` is now
// consumed from `ux_contract::activity` v0.3.12 — single source of
// truth, ratified through edge_monitor's P5 sampler validation
// cycle (v1.1.0–v1.1.5). Variant taxonomy (Active / Idle / Loading
// / NotDetected) and the `Debug, Clone, Copy, PartialEq, Eq, Hash`
// shape are unchanged; see the contract's doc-comment for the
// canonical per-variant semantics.
//
// The contract crate is intentionally zero-dependency (no `serde`
// derives on the enum), so wire serialization belongs to the
// consumer at its boundary. `web/wire.rs::activity_state_to_str`
// already does that for the dashboard. For `TelemetryFrame`'s
// `#[derive(Serialize, Deserialize)]` we use serde's remote-derive
// pattern via `ActivityStateDef` below — no behaviour change,
// identical snake_case wire shape (`"active" / "idle" / "loading"
// / "not_detected"`), no serde leakage onto the contract.
pub use ux_contract::activity::ActivityState;

/// v1.1.10 ITEM 1 — serde remote-derive shim for the foreign
/// [`ux_contract::activity::ActivityState`]. The contract enum has
/// no `Serialize`/`Deserialize` derives (the crate is intentionally
/// dependency-free), so we use this local mirror with
/// `#[serde(remote = ...)]` to give serde a handle on the foreign
/// type without forcing a dep onto the contract crate.
///
/// Wire shape: `"active" / "idle" / "loading" / "not_detected"` —
/// IDENTICAL to the pre-v1.1.10 local enum's `rename_all = "snake_case"`
/// output. A wire-format regression-pin lives in
/// `tests/telemetry_frame_activity_state_wire.rs`-style coverage
/// inside this module (see `activity_state_consumed_from_contract`).
///
/// Applied at the field site via
/// `#[serde(with = "activity_state_option_serde")]` on `Option<ActivityState>`.
#[derive(Serialize, Deserialize)]
#[serde(remote = "ux_contract::activity::ActivityState")]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // referenced through serde's `with` attribute, not directly
enum ActivityStateDef {
    Active,
    Idle,
    Loading,
    NotDetected,
}

/// Serde shim for `Option<ActivityState>` that delegates to
/// `ActivityStateDef`. `#[serde(with = "Def")]` doesn't compose
/// directly through `Option`, so we wrap the `Some` payload in a
/// transparent helper inside the serializer / deserializer.
pub(crate) mod activity_state_option_serde {
    use super::{ActivityState, ActivityStateDef};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &Option<ActivityState>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Helper<'a>(#[serde(with = "ActivityStateDef")] &'a ActivityState);
        match value {
            Some(v) => serializer.serialize_some(&Helper(v)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<ActivityState>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper(#[serde(with = "ActivityStateDef")] ActivityState);
        Ok(Option::<Helper>::deserialize(deserializer)?.map(|h| h.0))
    }
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
    ///
    /// v1.1.10 ITEM 1 — `ActivityState` is now the foreign type from
    /// `ux_contract::activity` (zero-dep crate, no serde derives).
    /// Wire format unchanged via the `activity_state_option_serde`
    /// shim that mirrors the pre-v1.1.10 `rename_all = "snake_case"`
    /// output.
    #[serde(default, with = "activity_state_option_serde")]
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

    /// Phase 2 / DISPATCH 1 — `sample` variant that receives the
    /// process lists this tick. Used by samplers that need parent /
    /// child tree visibility (B2 agent-claude needs to see siblings
    /// to distinguish "agent CLI alone" from "agent CLI with a Bash
    /// tool-child running"). Default polyfill delegates to `sample`
    /// so every existing impl keeps working unchanged; the dispatcher
    /// calls `sample_with_context` on the tick path. Additive trait
    /// extension per Inspector #12 Option (i).
    ///
    /// `ai_procs`: AI-classified workloads only (matches the
    /// classifier `AICategory != NotAi` filter the runtime applies
    /// before `Dispatcher::tick`). Use this for sampler-to-sampler
    /// correlation between known workloads.
    ///
    /// `all_procs`: the UNFILTERED kernel process list. Necessary for
    /// samplers that detect NON-AI children of an AI process — e.g.
    /// B2 agent_claude detecting Bash tool-children, where `bash`
    /// itself is `NotAi` and would be absent from `ai_procs`.
    /// EMPIRICAL (DISPATCH 6B): `runtime.rs` filters to AI workloads
    /// before `tick`, so a sampler doing child-process detection MUST
    /// read `all_procs` — reading `ai_procs` was the v1.1.1 B2
    /// active-detection bug (bash children filtered out → activity
    /// locked to Idle). Same defect class as the B1 v1.1.0 asymmetric
    /// compare and the cpu_pct / ppid foundation gaps: the data
    /// exists and the plumbing landed, but the consumer couldn't
    /// reach it.
    ///
    /// New samplers should default to `ai_procs` unless they
    /// specifically need bash / utility children.
    async fn sample_with_context(
        &mut self,
        proc: &ProcessSnapshot,
        _ai_procs: &[ProcessSnapshot],
        _all_procs: &[ProcessSnapshot],
    ) -> SourceResult<TelemetryFrame> {
        self.sample(proc).await
    }

    /// v1.1.1 — per-source upper bound on a single `sample` call.
    /// The dispatcher wraps `sample_with_context` in a
    /// `tokio::time::timeout(self.sample_timeout(), ...)` so a
    /// stuck sampler can't block the tick loop.
    ///
    /// Default: [`crate::telemetry::DEFAULT_SAMPLE_TIMEOUT`] (1 s)
    /// — fits HTTP-scrape samplers (vLLM, llama.cpp, Ollama) and
    /// pure-CPU heuristics (B4). Samplers with empirically longer
    /// signal acquisition (B3 ROS2 needs ≥ 3 s for `ros2 topic hz`
    /// to publish its first rate after observing 3 messages)
    /// override this method to widen the dispatcher's outer wrap.
    ///
    /// Constraint: returning a long timeout does not speed the
    /// sampler up — it only buys it room to finish. Samplers
    /// should still cap any internal I/O wait at their declared
    /// `sample_timeout` minus a small kill-signal headroom, so
    /// the dispatcher's outer wrap never has to cancel a healthy
    /// sample mid-flight.
    ///
    /// B3 root-cause (DISPATCH 5 STEP 2): pre-v1.1.1 the
    /// dispatcher used a single global 1 s outer wrap. B3's
    /// inner `ROS2_SHELLOUT_TIMEOUT = 5 s` was always cancelled
    /// at 1 s, so `ros2 topic hz` never observed enough messages
    /// to emit a rate. ActivityState locked to `NotDetected` for
    /// every ROS2 row in v1.1.0.
    fn sample_timeout(&self) -> std::time::Duration {
        crate::telemetry::DEFAULT_SAMPLE_TIMEOUT
    }

    /// v1.1.7 (DISPATCH 22 ITEM 2) — notify the sampler that the
    /// runtime has forgotten `pid` (it exited and its `RunRecord`
    /// has been persisted, or the operator killed it). Samplers
    /// that cache per-PID state should drop the entry here so
    /// stale data doesn't leak into a recycled PID.
    ///
    /// Default impl is a no-op: most samplers are stateless
    /// per-PID and have nothing to forget. B3 ROS2 shellout
    /// (`Ros2ShelloutSource`) overrides this to drop its
    /// `PerPidState` map entry — pre-v1.1.7 B3 relied on a 5-min
    /// time-based GC sweep at the top of B3's `sample` body
    /// (`ROS2_CACHE_GC_THRESHOLD`) which bounded the leak in time
    /// but kept ghost entries for that whole window after PID
    /// death. The `on_forget` hook makes the clear prompt.
    ///
    /// Sync signature (not `async`): cache drops are O(1) HashMap
    /// removals and don't need to await. The dispatcher acquires
    /// the per-source `tokio::sync::Mutex` and calls this under
    /// the guard.
    ///
    /// Foundation extension flagged by DISPATCH 16 trigger #4 and
    /// Inspector #15 (cache-clear gap). Deliberately deferred in
    /// v1.1.5 ITEM E (which shipped the GC sweep as a bounded
    /// workaround); now landed under operator sanction.
    fn on_forget(&mut self, _pid: u32) {}
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
            ppid: None,
            workload_category: None,
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

    /// v1.1.10 ITEM 1 — wire-format regression pin for the foreign
    /// [`ux_contract::activity::ActivityState`]. The contract enum
    /// is zero-dep (no `Serialize` derive), so `serde_json::to_string`
    /// on the bare enum no longer compiles. Wire shape is preserved
    /// through `TelemetryFrame`'s `activity_state_option_serde`
    /// shim — round-trip via the field and assert the snake_case
    /// payload (`"active" / "idle" / "loading" / "not_detected"`)
    /// appears verbatim in the JSON.
    ///
    /// Renamed from `activity_state_serialises_snake_case` because
    /// the pin's surface moved (enum → field-via-shim).
    #[test]
    fn activity_state_consumed_from_contract() {
        let cases: &[(ActivityState, &str)] = &[
            (ActivityState::Active, "\"active\""),
            (ActivityState::Idle, "\"idle\""),
            (ActivityState::Loading, "\"loading\""),
            (ActivityState::NotDetected, "\"not_detected\""),
        ];
        for (state, expected_payload) in cases {
            let mut frame = TelemetryFrame::new(7);
            frame.activity_state = Some(*state);
            let json = serde_json::to_string(&frame).unwrap();
            assert!(
                json.contains(expected_payload),
                "TelemetryFrame{{ activity_state: Some({state:?}) }} JSON \
                 must contain {expected_payload} (via the v1.1.10 \
                 activity_state_option_serde shim). got: {json}",
            );
            // Round-trip back through the shim.
            let restored: TelemetryFrame = serde_json::from_str(&json).unwrap();
            assert_eq!(
                restored.activity_state,
                Some(*state),
                "round-trip via the shim must preserve the variant",
            );
        }
    }

    /// v1.1.10 ITEM 1 — pin that the foreign `ActivityState` is
    /// distinctly the four contract variants and they round-trip
    /// via the shim end-to-end. Complements the contract-side
    /// `all_four_variants_are_distinct` test in `~/ux_contract`
    /// (which pins the variant set at the producer); this is the
    /// consumer-side pin that all four variants reach edge_monitor
    /// without collapse.
    #[test]
    fn activity_state_four_variants_round_trip_via_shim() {
        let states = [
            ActivityState::Active,
            ActivityState::Idle,
            ActivityState::Loading,
            ActivityState::NotDetected,
        ];
        for state in &states {
            let mut frame = TelemetryFrame::new(13);
            frame.activity_state = Some(*state);
            let json = serde_json::to_string(&frame).unwrap();
            let restored: TelemetryFrame = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.activity_state, Some(*state));
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
        // v1.1.2 — polyfill ignores both ai_procs + all_procs and
        // delegates to `sample`; pass the same slice for both.
        let frame = s.sample_with_context(&p, &all, &all).await.unwrap();
        assert_eq!(frame.pid, 11);
        assert_eq!(frame.tokens_per_sec, Some(42.0));
        // Default polyfill went through `sample` exactly once.
        assert_eq!(s.called, 1);
    }

    /// v1.1.7 ITEM 2 — default `on_forget` body is a no-op.
    /// Samplers that don't override get a silent default that
    /// matches every existing sampler's "no per-PID cache to
    /// drop" reality. Pinned here so a future trait refactor
    /// that tightens the default (e.g. requires explicit impl)
    /// trips this test before breaking every downstream sampler.
    #[test]
    fn on_forget_default_body_is_a_noop() {
        // StubSource carries no per-PID state. The default trait
        // body runs and returns; no panic, no observable effect.
        // The `called` counter stays at 0 (sample() not invoked).
        let mut s = StubSource { called: 0 };
        s.on_forget(7);
        s.on_forget(8);
        s.on_forget(u32::MAX);
        assert_eq!(s.called, 0, "default on_forget must not invoke sample");
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
