//! Post-hoc analysis built on top of stored run records.
//!
//! Foundation C of the latest.md plan. Today this is just the baseline /
//! regression detector consumed by `RunStore::baseline` and (later) by
//! the runtime exit hook (Tier 1.3).

pub mod compare;

pub use compare::{
    Baseline, BaselineMetrics, Regression, RegressionEvent, Severity, detect_regressions,
};
