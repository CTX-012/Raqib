//! On-disk persistence for long-lived edge_monitor state.
//!
//! Currently scoped to the run-summary log — a durable JSONL record of
//! every `LifecycleSummary` the tracker emits. Audit-log persistence lives
//! in `governor::audit` because its primary stakeholder is the governor
//! decision trail, not the lifecycle subsystem.

pub mod log_store;

pub use log_store::LogStore;
