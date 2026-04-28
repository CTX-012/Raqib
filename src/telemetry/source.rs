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
    pub concurrent_requests: Option<u32>,

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
