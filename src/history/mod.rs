//! v1.3.2 / DISPATCH 89 / PHASE 5 — history subsystem.
//!
//! Two distinct surfaces share this module's namespace per the
//! [`docs/PHASE5_HISTORY_DESIGN.md`] Q1 decision:
//!
//! 1. **CLI history subcommand** (pre-D89, lives in [`cli`]).
//!    `edge_monitor history [MODEL] [--limit N] [--json]` reads
//!    persisted [`crate::storage::run_store::RunRecord`]s and renders
//!    a per-model peaks-only browser. **Untouched by D89**; its
//!    public API is re-exported here so [`crate::main`] and
//!    [`crate::ui::panels::history_overlay`] keep working unchanged.
//!
//! 2. **In-memory session History** (NEW in D89). Per-PID rolling
//!    sample trajectories + a cross-PID event archive, lost on
//!    restart, sized for a live debugging window (defaults: ~30 min
//!    of trajectory samples per PID, ~500 events archive-wide).
//!    Future PHASE 5 steps wire CAPTURE (step 3) and a web view
//!    (step 6+); D89 ships TYPES + tests only — pure structure,
//!    nothing in [`crate::runtime`] constructs or reads it yet.
//!
//! ## The Q1 honest shape (one module, two structures)
//!
//! [`History`] is the single owner: a [`HashMap`] of per-PID
//! [`TrajectoryRing`]s + a [`VecDeque`] of [`HistoryEvent`]s. The
//! collections are structurally distinct because their access
//! patterns are orthogonal (per-PID write-then-query vs. cross-PID
//! write-then-time-query). Forcing them into one VecDeque or one
//! HashMap would either bloat the per-event memory (a PID indirection
//! on events that don't have one) or re-implement the HashMap on top
//! of a time-ordered Vec. See [`docs/PHASE5_HISTORY_DESIGN.md`] Q1
//! for the alternatives weighed.
//!
//! ## What lives where, when
//!
//! - **While a PID is alive:** its samples accumulate in
//!   [`History::trajectories`] under [`History`]. Capacity-bounded
//!   FIFO: at cap, the OLDEST sample is dropped on push (the
//!   "rolling 30-minute window" semantic).
//! - **On PID exit (PHASE 5 step 4, future dispatch):** the ring is
//!   drained into a [`Trajectory`] (the FROZEN form) and attached to
//!   the corresponding [`crate::lifecycle::LifecycleSummary`].
//!   `History::trajectories` no longer holds an entry for that PID.
//!   The eviction of dead processes from history rides the existing
//!   `completed_history = 50` cap on `state.completed` for free.
//! - **Cross-PID events:** every exit / kill / regression that
//!   today pushes into `state.completed` / `state.audit` /
//!   `state.regressions` ALSO pushes a derived [`HistoryEvent`] into
//!   [`History::event_archive`] (PHASE 5 step 5, future dispatch).
//!   ADDITIVE — the live wire activity feed cap is unchanged.

mod ring;
pub mod cli;

// Preserve the CLI's pre-D89 public API so `main.rs` (subcommand
// dispatch) and `ui::panels::history_overlay` keep their existing
// imports. The directory-module shape is internal to the D89 split;
// the call sites still see `crate::history::run_history`, etc.
pub use cli::{
    ModelSummary, build_model_summaries, format_exit_short, format_exit_short_for_record,
    run_history, run_history_to,
};

pub use ring::{
    HistoryEvent, HistoryEventKind, Sample, Trajectory, TrajectoryRing,
};

use std::collections::{HashMap, VecDeque};

/// PHASE 5 in-memory History container — the single owner of the
/// per-PID trajectories and the cross-PID event archive.
///
/// **No production code constructs or reads `History` yet.** D89
/// ships the types + tests; D90 wires CAPTURE into the runtime tick.
/// Until then the module is dormant — the
/// `history_is_not_constructed_in_runtime_yet` integration guard
/// pins that invariant.
#[derive(Debug, Clone)]
pub struct History {
    /// Per-live-PID rolling sample ring. Bounded individually by
    /// `runtime.history_trajectory_samples_per_pid` (default 1800).
    /// Entries are removed when a PID exits — its samples then live
    /// on `LifecycleSummary.trajectory` per the Q3-C design.
    pub trajectories: HashMap<u32, TrajectoryRing>,
    /// Cross-PID event archive. Bounded by
    /// `runtime.history_event_archive_cap` (default 500). Newest at
    /// the back; on push, evict from the front when full. The
    /// archive is ADDITIVE w.r.t. the existing
    /// `state.completed`/`audit`/`regressions` rings; the live
    /// activity wire continues to read from those (capped at 50 per
    /// `ACTIVITY_FEED_WIRE_MAX`). Only the future history view
    /// queries this longer archive.
    pub event_archive: VecDeque<HistoryEvent>,

    /// Bounds snapshotted at construction so the push paths don't
    /// need to re-read `Config` on every tick — the same idiom the
    /// existing `runtime.audit_history` reads use at the audit-ring
    /// push sites.
    trajectory_cap_per_pid: usize,
    event_archive_cap: usize,
}

impl History {
    /// Construct a `History` with the operator-configured bounds.
    /// Both caps come from [`crate::config::RuntimeConfig`] (see
    /// `history_trajectory_samples_per_pid` and
    /// `history_event_archive_cap`).
    pub fn new(trajectory_cap_per_pid: usize, event_archive_cap: usize) -> Self {
        Self {
            trajectories: HashMap::new(),
            event_archive: VecDeque::new(),
            trajectory_cap_per_pid,
            event_archive_cap,
        }
    }

    /// Push a sample for `pid`. Creates the per-PID ring on first
    /// sample; subsequent samples ride the same ring. At capacity
    /// the OLDEST sample is dropped (rolling-window semantics —
    /// recent matters for trajectory replay).
    pub fn record_sample(&mut self, pid: u32, sample: Sample) {
        let cap = self.trajectory_cap_per_pid;
        self.trajectories
            .entry(pid)
            .or_insert_with(|| TrajectoryRing::with_capacity(cap))
            .push(sample);
    }

    /// Remove and FREEZE the PID's trajectory ring into an owned
    /// [`Trajectory`]. Called by the runtime on PID exit (PHASE 5
    /// step 4) — the frozen trajectory then rides on the
    /// corresponding `LifecycleSummary`. Returns `None` when the
    /// PID never recorded a sample (very short-lived process that
    /// appeared and vanished inside one tick — same shape the
    /// existing run summary handles).
    pub fn drain_trajectory(&mut self, pid: u32) -> Option<Trajectory> {
        self.trajectories.remove(&pid).and_then(|ring| ring.freeze())
    }

    /// Push a history event. At cap, evict the oldest from the
    /// front (FIFO archive — newest at the back, time-descending
    /// when iterated `.iter().rev()`).
    pub fn record_event(&mut self, event: HistoryEvent) {
        self.event_archive.push_back(event);
        while self.event_archive.len() > self.event_archive_cap {
            self.event_archive.pop_front();
        }
    }

    /// The trajectory ring cap as configured. Exposed for tests
    /// and the future view's bounds display.
    pub fn trajectory_cap_per_pid(&self) -> usize {
        self.trajectory_cap_per_pid
    }

    /// The event-archive cap as configured.
    pub fn event_archive_cap(&self) -> usize {
        self.event_archive_cap
    }

    /// Number of live PIDs with at least one recorded sample. Useful
    /// for the future view's "tracking N workloads" line.
    pub fn live_pid_count(&self) -> usize {
        self.trajectories.len()
    }

    /// Total event archive length (≤ event_archive_cap).
    pub fn event_count(&self) -> usize {
        self.event_archive.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn s(secs: i64, cpu: f32, rss: u32, vram: Option<u32>) -> Sample {
        Sample {
            timestamp: DateTime::from_timestamp(secs, 0).unwrap(),
            cpu_pct: cpu,
            rss_mb: rss,
            vram_mb: vram,
        }
    }

    #[test]
    fn new_history_is_empty() {
        let h = History::new(1800, 500);
        assert_eq!(h.live_pid_count(), 0);
        assert_eq!(h.event_count(), 0);
        assert_eq!(h.trajectory_cap_per_pid(), 1800);
        assert_eq!(h.event_archive_cap(), 500);
    }

    #[test]
    fn record_sample_creates_per_pid_ring_lazily() {
        let mut h = History::new(10, 100);
        h.record_sample(42, s(0, 1.0, 10, None));
        assert_eq!(h.live_pid_count(), 1);
        h.record_sample(42, s(1, 2.0, 11, None));
        assert_eq!(h.live_pid_count(), 1); // same PID, still one entry
        h.record_sample(99, s(2, 3.0, 5, Some(100)));
        assert_eq!(h.live_pid_count(), 2);
    }

    #[test]
    fn record_event_archive_bounded_at_cap() {
        let mut h = History::new(10, 3);
        for i in 0..10 {
            h.record_event(HistoryEvent {
                timestamp: DateTime::from_timestamp(i, 0).unwrap(),
                pid: 100 + i as u32,
                name: format!("p{i}"),
                kind: HistoryEventKind::Exit,
                summary: format!("ev #{i}"),
            });
        }
        assert_eq!(h.event_count(), 3, "archive must cap at 3");
        // FIFO: oldest evicted, so we keep the LAST 3 (pids 107..=109).
        let pids: Vec<u32> = h.event_archive.iter().map(|e| e.pid).collect();
        assert_eq!(pids, vec![107, 108, 109]);
    }

    #[test]
    fn drain_trajectory_returns_none_when_pid_unknown() {
        let mut h = History::new(10, 100);
        assert!(h.drain_trajectory(404).is_none());
    }

    #[test]
    fn drain_trajectory_returns_owned_and_removes_entry() {
        let mut h = History::new(10, 100);
        h.record_sample(7, s(0, 1.0, 10, None));
        h.record_sample(7, s(1, 2.0, 11, None));
        assert_eq!(h.live_pid_count(), 1);
        let traj = h.drain_trajectory(7).expect("trajectory must exist");
        assert_eq!(traj.samples.len(), 2);
        assert_eq!(traj.samples[0].cpu_pct, 1.0);
        assert_eq!(h.live_pid_count(), 0, "drain must remove the entry");
        // Re-drain returns None.
        assert!(h.drain_trajectory(7).is_none());
    }

    /// Sample size pin against the doc's memory math (~32 B/sample).
    /// If a future refactor adds a field that bloats Sample past 40 B,
    /// the memory budget in PHASE5_HISTORY_DESIGN.md needs revisiting.
    #[test]
    fn sample_size_matches_design_doc_memory_math() {
        use std::mem::size_of;
        let sz = size_of::<Sample>();
        assert!(
            sz <= 40,
            "Sample size {sz} B exceeds the doc-locked ~32 B budget \
             (allowed 40 B for alignment slack). The memory math in \
             docs/PHASE5_HISTORY_DESIGN.md (32 B × 32 PIDs × 1800 \
             samples ≈ 1.84 MB) MUST be revisited if Sample grows.",
        );
    }

    /// Nothing-wired-yet sanity: D89 is pure structure. The runtime
    /// MUST NOT construct or read `History` until PHASE 5 step 3+. A
    /// future contributor accidentally wiring CAPTURE into
    /// `Runtime::tick()` ahead of the step-3 dispatch trips this
    /// check.
    ///
    /// We grep the runtime source string for the `History` constructor
    /// name. The match is loose on purpose (allow doc-comments and
    /// future-tense planning text) — the trigger is a CALL site
    /// `History::new(`.
    #[test]
    fn history_is_not_constructed_in_runtime_yet() {
        let src = include_str!("../runtime.rs");
        // Allow any prose mention; reject only the constructor call.
        assert!(
            !src.contains("History::new("),
            "PHASE 5 step 1 invariant breached: runtime.rs constructs \
             `History::new(...)` but D89 ships TYPES ONLY. CAPTURE \
             wiring is PHASE 5 step 3 — a separate dispatch. If a \
             follow-up step landed correctly, update this guard to \
             allow the expected call site.",
        );
        // Also reject calls into the record/drain entry points.
        // The pre-existing `self.tracker\n    .record_sample(...)`
        // method-chain at runtime.rs:983 is the ONLY allowed call
        // site; it lives on `LifecycleTracker`, NOT on `History`.
        // We walk every `.record_sample(` occurrence and verify each
        // is preceded (within the immediate prefix, stripping
        // whitespace) by a `tracker` token. A `History::record_sample`
        // call would fail that check.
        for (offset, _) in src.match_indices(".record_sample(") {
            let head = src[..offset].trim_end();
            assert!(
                head.ends_with("self.tracker") || head.ends_with("tracker"),
                "PHASE 5 step 1 invariant breached: runtime.rs calls \
                 `.record_sample(...)` on something other than the \
                 existing `LifecycleTracker`. D89 is types-only. \
                 First non-tracker call at byte offset {offset}.",
            );
        }
    }

}
