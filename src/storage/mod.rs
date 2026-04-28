//! On-disk persistence for long-lived edge_monitor state.
//!
//! Two stores live here:
//!
//! * [`LogStore`] — Phase-1 single-file JSONL of `LifecycleSummary`s.
//!   Kept around for backwards compatibility with existing logs;
//!   superseded by `RunStore` for new writes.
//! * [`RunStore`] — Foundation-A typed store of `RunRecord`s with a
//!   per-model index (latest.md spec). Backs the history viewer (Tier
//!   1.1) and the regression detector (Tier 1.3).
//!
//! Audit-log persistence lives in `governor::audit` because its primary
//! stakeholder is the governor decision trail, not the lifecycle
//! subsystem.

pub mod log_store;
pub mod run_store;

pub use log_store::LogStore;
pub use run_store::{
    ColdStartStats, ExitReason, RunId, RunMetrics, RunRecord, RunStore, RunStoreError, RuntimeKind,
};
