//! v1.3.2 / DISPATCH 89 / PHASE 5 step 1 — types for the in-memory
//! History subsystem.
//!
//! Pure data + bounded-ring primitives. The runtime tick path does
//! NOT call these in D89; CAPTURE wiring is PHASE 5 step 3, a
//! separate dispatch. See [`super`] for the architecture overview
//! and [`docs/PHASE5_HISTORY_DESIGN.md`] for the field-by-field
//! rationale.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One per-tick sample for one PID. The compact shape (~32 B in
/// practice — verified by `super::tests::sample_size_matches_design_doc_memory_math`)
/// is the cornerstone of the trajectory memory budget: 32 B × 32
/// worst-case PIDs × 1800 samples ≈ 1.84 MB worst case for the
/// live trajectory store.
///
/// Field choices:
/// * `timestamp: DateTime<Utc>` — 12 B on 64-bit Linux (i64 secs +
///   i32 nanos). Same shape `LifecycleSummary` already serializes
///   (the existing wire format consumers know this layout).
/// * `cpu_pct: f32` — mirrors the existing
///   [`crate::lifecycle::ResourceStats::cpu_peak_pct`] precision.
/// * `rss_mb: u32` — 4 GB RSS ceiling per sample is well above any
///   AI workload; the runtime tick passes
///   `p.rss_mb` ([`crate::runtime::AnnotatedProcess::rss_mb`]) which
///   is already a `u64` we narrow to fit. 64-bit RSS samples would
///   double the ring's memory footprint for negligible gain.
/// * `vram_mb: Option<u32>` — `None` ≠ 0 honesty (same discipline
///   the D74/D78 VRAM_UNMEASURED path enforces). 32-bit covers
///   4 GB per device; multi-device aggregates are summed before the
///   sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub timestamp: DateTime<Utc>,
    pub cpu_pct: f32,
    pub rss_mb: u32,
    /// `None` when VRAM is unmeasured (no GPU, NVML failed, the PID
    /// never appeared in `per_process_vram`). NEVER zero-filled —
    /// the renderer prints `VRAM_UNMEASURED` rather than `0`.
    pub vram_mb: Option<u32>,
}

/// Live (mutating) per-PID sample ring. Capacity-bounded FIFO: when
/// full, [`Self::push`] evicts the OLDEST sample before appending
/// the new one. The 1800-sample default (~30 min @ 1 Hz) is the
/// "how far back is interesting for a live trajectory" window
/// chosen in the design doc.
///
/// Owned by [`super::History::trajectories`]; on PID exit, the
/// runtime calls [`Self::freeze`] to convert into the
/// [`Trajectory`] owned form that rides on `LifecycleSummary`.
#[derive(Debug, Clone)]
pub struct TrajectoryRing {
    /// Newest samples at the back of the deque. `VecDeque` matches
    /// the existing audit/completed ring idiom in [`crate::runtime::RuntimeState`].
    samples: VecDeque<Sample>,
    /// Hard cap; runtime side sources from
    /// `config.runtime.history_trajectory_samples_per_pid`. Stored
    /// per-ring so the same struct can be constructed in tests with
    /// a smaller cap (avoiding 1800-iteration test setups).
    cap: usize,
}

impl TrajectoryRing {
    /// Construct an empty ring with the given hard cap. `cap = 0`
    /// is rejected at config-load time
    /// ([`crate::config::Config::validate`]); reaching here with 0
    /// would mean "drop every sample on push," which is the same as
    /// a no-op tracker and we don't bother specializing.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(cap.min(8192)),
            cap,
        }
    }

    /// Push a sample. At capacity, the OLDEST sample is dropped FIRST
    /// then the new one is appended at the back (rolling-window
    /// semantics — the LAST N samples win, regardless of when they
    /// arrived).
    pub fn push(&mut self, sample: Sample) {
        if self.cap == 0 {
            return;
        }
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Current sample count (≤ `cap`).
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// `true` when no samples have been pushed yet.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Hard cap. Exposed for the view's "N/M samples" display.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Read-only access for the future view's iteration (no runtime
    /// caller in D89; consumed by step 4 onward).
    pub fn iter(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    /// Drain the ring into a [`Trajectory`] (the FROZEN owned form).
    /// Returns `None` when the ring is empty — the runtime exit-drain
    /// (step 4) uses this signal to skip attaching an empty
    /// trajectory to `LifecycleSummary` (matches the existing
    /// `samples == 0` post-mortem precedent).
    pub fn freeze(self) -> Option<Trajectory> {
        if self.samples.is_empty() {
            return None;
        }
        // Q3-C: the trajectory now rides on a LifecycleSummary. We
        // snapshot `first_sample_at` / `last_sample_at` here so the
        // post-mortem doesn't have to walk the Vec to find them.
        let first_sample_at = self.samples.front().map(|s| s.timestamp);
        let last_sample_at = self.samples.back().map(|s| s.timestamp);
        let samples: Vec<Sample> = self.samples.into_iter().collect();
        // ok: expect — both options were just populated from the
        // VecDeque front/back; the only way for them to be None is
        // an empty VecDeque, which the early-return above already
        // rejected.
        let first_sample_at = first_sample_at.expect("non-empty deque has a front");
        let last_sample_at = last_sample_at.expect("non-empty deque has a back");
        Some(Trajectory {
            samples,
            first_sample_at,
            last_sample_at,
        })
    }
}

/// Frozen, owned form of a per-PID trajectory. Built by
/// [`TrajectoryRing::freeze`] on PID exit (PHASE 5 step 4) and
/// attached to the corresponding `LifecycleSummary` for the
/// remaining lifetime of the in-memory `state.completed` ring
/// (default 50 dead processes — see `runtime.completed_history`).
///
/// `samples` is a `Vec` rather than a `VecDeque` because once
/// frozen the trajectory does not mutate; the renderer iterates and
/// reads. `first_sample_at` / `last_sample_at` are snapshotted at
/// freeze time so the view's axis labels don't have to walk the
/// Vec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// Oldest-first; index 0 is the first sample recorded for this
    /// PID, last entry is the final sample before exit. Capped at
    /// `runtime.history_trajectory_samples_per_pid` (default 1800).
    pub samples: Vec<Sample>,
    pub first_sample_at: DateTime<Utc>,
    pub last_sample_at: DateTime<Utc>,
}

impl Trajectory {
    /// Total samples carried — bounded by the configured cap.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// `true` when no samples were captured before exit (very
    /// short-lived process). Note: [`TrajectoryRing::freeze`]
    /// returns `None` in that case, so a `Trajectory` value
    /// almost always has `is_empty() == false`. Kept for symmetry
    /// with `len()`.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Discriminator for [`HistoryEvent`]. Mirrors the three sources
/// the existing live activity feed merges
/// ([`crate::ui::panels::activity::build_events`]): `Exit` from
/// `state.completed`, `Kill` from `state.audit`,
/// `Regression` from `state.regressions`.
///
/// `serde(rename_all = "snake_case")` so future wire types serialize
/// to the same lowercase tokens the activity feed already uses
/// (`"exit"` / `"kill"` / `"regression"` per the
/// [`crate::web::wire::WireActivityEntry::kind`] field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEventKind {
    Exit,
    Kill,
    Regression,
}

/// One archived event in the cross-PID event timeline. Pre-rendered
/// summary text (`summary`) so the future view renders identical
/// wording to the live activity feed — same single-source-of-truth
/// pattern the wire's `WireActivityEntry::summary` already uses.
///
/// The shape is intentionally minimal for D89; richer per-kind
/// detail (D74 shape-A fields, regression delta numbers) can be
/// added in PHASE 5 step 5+ when the EVENT ARCHIVE write path lights
/// up. The trajectory ride-along for `Exit` events happens via the
/// `LifecycleSummary.trajectory` field per Q3-C; this struct
/// doesn't carry a trajectory pointer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub timestamp: DateTime<Utc>,
    pub pid: u32,
    pub name: String,
    pub kind: HistoryEventKind,
    /// One-line summary, pre-rendered server-side. Mirrors
    /// `WireActivityEntry::summary` shape.
    pub summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: i64, cpu: f32, rss: u32) -> Sample {
        Sample {
            timestamp: DateTime::from_timestamp(secs, 0).unwrap(),
            cpu_pct: cpu,
            rss_mb: rss,
            vram_mb: None,
        }
    }

    #[test]
    fn ring_below_cap_retains_all_samples() {
        let mut r = TrajectoryRing::with_capacity(10);
        for i in 0..5 {
            r.push(s(i, i as f32, i as u32));
        }
        assert_eq!(r.len(), 5);
        let cpus: Vec<f32> = r.iter().map(|s| s.cpu_pct).collect();
        assert_eq!(cpus, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn ring_at_cap_evicts_oldest_fifo() {
        let mut r = TrajectoryRing::with_capacity(3);
        for i in 0..6 {
            r.push(s(i, i as f32, i as u32));
        }
        assert_eq!(r.len(), 3, "cap is 3; len must clamp");
        // FIFO: the LAST 3 samples win (3, 4, 5).
        let cpus: Vec<f32> = r.iter().map(|s| s.cpu_pct).collect();
        assert_eq!(
            cpus,
            vec![3.0, 4.0, 5.0],
            "rolling window must retain the newest cap samples",
        );
    }

    #[test]
    fn ring_cap_zero_is_no_op() {
        // Defensive — Config::validate rejects 0 at load time
        // (DISPATCH 89 step 0), but the inner type stays robust if
        // a test constructs one directly.
        let mut r = TrajectoryRing::with_capacity(0);
        r.push(s(0, 1.0, 1));
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn freeze_empty_returns_none() {
        let r = TrajectoryRing::with_capacity(10);
        assert!(r.freeze().is_none());
    }

    #[test]
    fn freeze_populated_returns_trajectory_with_endpoints() {
        let mut r = TrajectoryRing::with_capacity(10);
        r.push(s(100, 1.0, 10));
        r.push(s(200, 2.0, 20));
        r.push(s(300, 3.0, 30));
        let t = r.freeze().expect("non-empty ring freezes");
        assert_eq!(t.samples.len(), 3);
        assert_eq!(t.first_sample_at.timestamp(), 100);
        assert_eq!(t.last_sample_at.timestamp(), 300);
    }

    #[test]
    fn freeze_preserves_eviction_window() {
        // After eviction, the FROZEN trajectory carries the rolling
        // window's last N samples — not the original sequence.
        let mut r = TrajectoryRing::with_capacity(3);
        for i in 0..6 {
            r.push(s(i * 10, i as f32, i as u32));
        }
        let t = r.freeze().expect("frozen");
        let cpus: Vec<f32> = t.samples.iter().map(|s| s.cpu_pct).collect();
        assert_eq!(cpus, vec![3.0, 4.0, 5.0]);
        assert_eq!(t.first_sample_at.timestamp(), 30);
        assert_eq!(t.last_sample_at.timestamp(), 50);
    }

    #[test]
    fn trajectory_len_matches_samples_vec() {
        let t = Trajectory {
            samples: vec![s(0, 1.0, 10), s(1, 2.0, 20)],
            first_sample_at: DateTime::from_timestamp(0, 0).unwrap(),
            last_sample_at: DateTime::from_timestamp(1, 0).unwrap(),
        };
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
    }

    #[test]
    fn history_event_kind_serializes_snake_case() {
        let ev = HistoryEvent {
            timestamp: DateTime::from_timestamp(0, 0).unwrap(),
            pid: 1234,
            name: "ollama".into(),
            kind: HistoryEventKind::Kill,
            summary: "Sent SIGTERM to ollama (PID 1234)".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"kind\":\"kill\""),
            "HistoryEventKind must serialize as snake_case to match \
             WireActivityEntry::kind tokens; got: {json}",
        );
    }
}
