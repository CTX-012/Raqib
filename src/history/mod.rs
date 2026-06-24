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

mod from_state;
mod ring;
pub mod cli;

pub use from_state::{exit_event, kill_event, regression_event};

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

    /// THE D89/D90/D91 INVARIANT (CONVERTED at each step — same
    /// guard role, evolved shape). The History write paths are
    /// wired in known, bounded places in `runtime.rs`, with:
    ///
    ///   * exactly ONE `History::new(` construction (in `Runtime::new`),
    ///   * exactly ONE `.record_sample(` call routed through
    ///     `self.history` (the per-tick trajectory capture site in
    ///     `Runtime::tick`),
    ///   * exactly ONE `.drain_trajectory(` call (the exit hand-off
    ///     in the `for summary in &lifecycle.recent_exits` loop),
    ///   * **DISPATCH 91 / step 5:** exactly SIX `.record_event(`
    ///     calls on `self.history` (1 exit-drain + 4 audit-push sites
    ///     for kills + 1 regression-iter), and NOTHING ELSE.
    ///
    /// Plus the WRITE-ONLY invariant: nothing in `runtime.rs` reads
    /// `self.history.trajectories` or `self.history.event_archive`.
    /// The first consumer is PHASE 5 step 6 (a separate dispatch
    /// with a contract bump).
    ///
    /// Conversion history: D89 forbade ANY History construction or
    /// method call (dormant); D90 lit up trajectory capture + exit
    /// hand-off; D91 lights up event-archive writes. Same pattern as
    /// the D80 `send_sigterm_actuation_site_is_auto_actuate_gated`
    /// staged tripwire.
    #[test]
    fn history_capture_is_wired_exactly_once_in_runtime() {
        let src = include_str!("../runtime.rs");
        // Strip the `#[cfg(test)]` test module so per-test fixture
        // constructions don't count against the production guard.
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };

        // (1) Exactly ONE `History::new(` call site in production.
        let new_count = prod.matches("History::new(").count();
        assert_eq!(
            new_count, 1,
            "PHASE 5 step 3 invariant: `History::new(...)` must \
             appear EXACTLY ONCE in runtime.rs production (the \
             `Runtime::new` construction). Found {new_count}. A \
             second constructor would mean two trajectory stores \
             that never see each other's samples — split-brain.",
        );

        // (2) Exactly ONE `self.history.record_sample(...)` capture
        // site. Allow the pre-existing `self.tracker.record_sample`
        // call (it lives on `LifecycleTracker`, a different type).
        let mut history_capture_sites = 0usize;
        for (offset, _) in prod.match_indices(".record_sample(") {
            let head = prod[..offset].trim_end();
            if head.ends_with("self.history") || head.ends_with("history") {
                history_capture_sites += 1;
            } else if head.ends_with("self.tracker") || head.ends_with("tracker") {
                // pre-existing LifecycleTracker site — allow.
            } else {
                panic!(
                    "PHASE 5 step 3 invariant: `.record_sample(...)` \
                     called on an UNRECOGNISED receiver in runtime.rs \
                     at byte offset {offset}. Allowed receivers are \
                     `self.tracker` (pre-existing) and `self.history` \
                     (D90 capture site). Prefix: …{:?}",
                    &head[head.len().saturating_sub(40)..],
                );
            }
        }
        assert_eq!(
            history_capture_sites, 1,
            "PHASE 5 step 3 invariant: `self.history.record_sample(...)` \
             must appear EXACTLY ONCE in runtime.rs production (the \
             tick-loop capture site). Found {history_capture_sites}.",
        );

        // (3) Exactly ONE `.drain_trajectory(` call (the exit
        // hand-off site, PHASE 5 step 4).
        let drain_count = prod.matches(".drain_trajectory(").count();
        assert_eq!(
            drain_count, 1,
            "PHASE 5 step 4 invariant: `.drain_trajectory(...)` must \
             appear EXACTLY ONCE in runtime.rs production (the \
             `for summary in &lifecycle.recent_exits` exit-drain \
             hand-off). Found {drain_count}. Multiple drains would \
             mean trajectories handed off in more than one place — \
             the dead-PID retention story would split.",
        );

        // (4) DISPATCH 91 / step 5 — event-archive write sites.
        // Six expected calls:
        //   * 1 in the `for summary in &lifecycle.recent_exits` loop
        //     (the EXIT site, AI-only filtered).
        //   * 4 at the audit-push sites for kills:
        //       - `manual_kill` (SIGTERM, operator-initiated)
        //       - `manual_force_kill` (SIGKILL, operator-consent)
        //       - `record_governor_audit` SIGTERM auto path
        //       - `record_governor_audit` SIGKILL escalation
        //   * 1 in the `state.regressions.iter().skip(regs_before)`
        //     loop (the REGRESSION mirror, alongside the existing
        //     Prom counter increment).
        // A 7th would mean a stray event source landed without
        // review; fewer than 6 means a refactor moved a kill push
        // off the audit path.
        let mut history_event_sites = 0usize;
        for (offset, _) in prod.match_indices(".record_event(") {
            let head = prod[..offset].trim_end();
            if head.ends_with("self.history") || head.ends_with("history") {
                history_event_sites += 1;
            } else {
                panic!(
                    "PHASE 5 step 5 invariant: `.record_event(...)` \
                     called on an UNRECOGNISED receiver in runtime.rs \
                     at byte offset {offset}. Only `self.history` is \
                     allowed. Prefix: …{:?}",
                    &head[head.len().saturating_sub(40)..],
                );
            }
        }
        assert_eq!(
            history_event_sites, 6,
            "PHASE 5 step 5 invariant: `self.history.record_event(...)` \
             must appear EXACTLY 6 times in runtime.rs production \
             (1 exit + 4 kill + 1 regression). Found {history_event_sites}. \
             A 7th would mean a stray event class slipped past review; \
             fewer means a kill audit push lost its mirror.",
        );

        // (5) WRITE-ONLY invariant — extended from D90. The History
        // container is constructed and written to, but nothing
        // READS the trajectories or event archive. A future
        // contributor adding a read path before PHASE 5 step 6
        // (the wire/view consumer) trips this check.
        for needle in [
            "self.history.trajectories",
            "self.history.event_archive",
            "history.trajectories.iter",
            "history.event_archive.iter",
        ] {
            assert!(
                !prod.contains(needle),
                "PHASE 5 step 3+4+5 invariant: runtime.rs reads from \
                 `{needle}` but D91 ships WRITE-ONLY capture + archive. \
                 The first consumer is PHASE 5 step 6 (web /api/history) \
                 — a separate dispatch (with a `ux_contract` bump).",
            );
        }
    }

}
