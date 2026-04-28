//! Telemetry sampler infrastructure — Foundation B of latest.md.
//!
//! Different runtimes expose throughput, latency, and KV-cache metrics
//! through wildly different surfaces (Prometheus endpoints, JSON APIs,
//! stdout regexes). This module gives them a common shape — the
//! [`TelemetrySource`] trait — plus a per-PID accumulator that folds
//! repeated samples into the metric fields the lifecycle / RunStore
//! eventually persist.
//!
//! Concrete sources (vLLM, llama.cpp server, Ollama, stdout parser)
//! land in Tier 1.2 as separate modules under `samplers/`. This file
//! ships only the abstractions and test scaffolding.
//!
//! Concurrency model:
//!  * `TelemetrySource` is `async` so HTTP scrapes can yield the
//!    runtime to other tasks while waiting for the network.
//!  * A panicking sampler must not bring down the runtime. The
//!    intended dispatcher (Tier 1.2) wraps each source in
//!    `tokio::spawn` and ignores join errors — Foundation B encodes
//!    that contract via the `safe_sample` helper used in tests.
//!  * Accumulator updates are `&mut self`; callers serialise per-PID
//!    via channel or mutex. No interior mutability inside the trait.

pub mod accumulator;
pub mod source;

pub use accumulator::TelemetryAccumulator;
pub use source::{ProcessSnapshot, TelemetryFrame, TelemetrySource, safe_sample};
