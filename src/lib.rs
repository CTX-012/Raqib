//! edge_monitor — library crate.
//!
//! The binary (`src/main.rs`) is a thin shim that wires CLI parsing,
//! signal handling, and tracing around the modules below. Extracted as a
//! library so integration tests in `tests/` and downstream tooling can
//! reuse the pipeline types (`Runtime`, `Config`, `ClassificationResult`,
//! `LifecycleSummary`, …) without shelling out to the binary.

// Many pub items are only exercised by `#[cfg(test)]` blocks today; the
// binary shim enables only a subset. Drop once the full CLI surface
// covers every public API.
#![allow(dead_code)]

pub mod analysis;
pub mod classifier;
pub mod compare;
pub mod config;
// Sprint 5 — `pub mod dashboard_preflight` removed. The Grafana
// integration that owned the preflight TCP probe was hard-deleted
// from v1.0; the v2 web companion (separate repo) handles the
// dashboard story. The `ux_contract::Action::OpenGrafana` enum
// variant and `ux_contract::status::GRAFANA_UNREACHABLE` template
// remain in the contract as orphans pending Agent A cleanup.
pub mod exec_wrapper;
pub mod exit_classify;
pub mod fingerprint;
pub mod governor;
pub mod history;
pub mod lifecycle;
pub mod model;
pub mod platform;
pub mod runtime;
pub mod storage;
pub mod telemetry;
pub mod ui;
