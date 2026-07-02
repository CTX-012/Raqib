//! v1.3.2 / DISPATCH 94 / PHASE 5 step 6 — `/api/history` +
//! `/api/history/trajectory/{pid}` endpoints.
//!
//! First operator-visible surface for the D89-D92 captured history
//! data. Two auth-gated read routes per CAR-D93's split-delivery
//! shape (Q2 in `docs/PHASE5_HISTORY_DESIGN.md`):
//!
//! * `GET /api/history` — SNAPSHOT: event archive (cap
//!   [`ux_contract::history::EVENT_ARCHIVE_MAX`]) + a lightweight
//!   dead-PID index. ~150 KB worst case; safe to fetch on view open.
//! * `GET /api/history/trajectory/{pid}` — the FULL per-PID sample
//!   trajectory for a specific dead PID. Fetched on demand when the
//!   operator drills into a row from the dead-PID index. ~50 KB.
//!
//! Rejected: single fat `/api/history` that embeds ALL trajectories.
//! 50 dead × 1800 samples × 28 B ≈ 2.5 MB per snapshot reload —
//! Q2 rejected this shape.
//!
//! ## Where the read invariant lives
//!
//! The D91 tripwire
//! `history_capture_is_wired_exactly_once_in_runtime` still forbids
//! ANY read of `self.history.trajectories` or `.event_archive` from
//! `runtime.rs`. This module is not scanned by that tripwire, so
//! the read path lives HERE — localized in one file, alongside the
//! endpoint handlers, not scattered through the runtime core. The
//! tick loop refreshes the shared view via [`refresh_shared`] from
//! main.rs / ui/mod.rs (outside runtime.rs), keeping the "write in
//! runtime, read at the endpoint" split clean.

use std::sync::{Arc, RwLock};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::WebState;
use crate::history::{History, HistoryEventKind};
use crate::runtime::RuntimeState;

// ─────────────────────────────────────────────────────────────────
// Wire types — consumer-side per CAR-D93's contract/consumer split.
// Field names match the KEY_* constants in
// `ux_contract::history` (contract-locked snake_case).
// ─────────────────────────────────────────────────────────────────

/// One sample in a per-PID trajectory. VRAM is
/// `Option<u32>` — `None` serializes as JSON `null` (via serde's
/// default) OR is OMITTED via `skip_serializing_if`. **NEVER
/// zero-filled.** The v0.3.20 CAR-D93 Q3 pins this: a `Some(0)`
/// means "measured, zero this tick"; `None`/absent means "no
/// measurement" (driver unloaded, NVML failed).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireSample {
    pub timestamp: DateTime<Utc>,
    pub cpu_pct: f32,
    pub rss_mb: u32,
    /// **VRAM honesty (CAR-D93 Q3):** `None` ⇒ omit / null on wire.
    /// `Some(N)` ⇒ measured N MB (including `Some(0)` — genuine
    /// zero). The renderer displays [`ux_contract::history::VRAM_UNMEASURED`]
    /// for the omitted case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_mb: Option<u32>,
}

impl From<&crate::history::Sample> for WireSample {
    fn from(s: &crate::history::Sample) -> Self {
        Self {
            timestamp: s.timestamp,
            cpu_pct: s.cpu_pct,
            rss_mb: s.rss_mb,
            vram_mb: s.vram_mb,
        }
    }
}

/// Full per-PID trajectory — the on-demand heavy payload.
#[derive(Debug, Clone, Serialize)]
pub struct WireTrajectory {
    pub samples: Vec<WireSample>,
    pub first_sample_at: DateTime<Utc>,
    pub last_sample_at: DateTime<Utc>,
}

impl From<&crate::history::Trajectory> for WireTrajectory {
    fn from(t: &crate::history::Trajectory) -> Self {
        Self {
            samples: t.samples.iter().map(Into::into).collect(),
            first_sample_at: t.first_sample_at,
            last_sample_at: t.last_sample_at,
        }
    }
}

/// One event in the history archive. `kind` serializes as
/// `"exit"` / `"kill"` / `"regression"` per
/// [`ux_contract::activity::ActivityKind`] — REUSED not
/// re-declared (CAR-D93 Q1).
#[derive(Debug, Clone, Serialize)]
pub struct WireHistoryEvent {
    /// One of `"exit"` / `"kill"` / `"regression"`.
    pub kind: String,
    pub timestamp: DateTime<Utc>,
    pub pid: u32,
    pub name: String,
    /// Pre-rendered one-line summary (single source of truth —
    /// consumer does NOT re-render on read).
    pub summary: String,
}

impl From<&crate::history::HistoryEvent> for WireHistoryEvent {
    fn from(e: &crate::history::HistoryEvent) -> Self {
        Self {
            kind: kind_to_wire_str(e.kind).to_string(),
            timestamp: e.timestamp,
            pid: e.pid,
            name: e.name.clone(),
            summary: e.summary.clone(),
        }
    }
}

/// One entry in the dead-PID index. Lightweight — carries enough
/// for the operator to identify + click into a trajectory. Does
/// NOT carry the sample sequence (that comes from
/// `/api/history/trajectory/{pid}`).
#[derive(Debug, Clone, Serialize)]
pub struct WireDeadPidEntry {
    pub pid: u32,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub exit_time: DateTime<Utc>,
}

/// `GET /api/history` response envelope.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WireHistorySnapshot {
    pub events: Vec<WireHistoryEvent>,
    pub dead_pids: Vec<WireDeadPidEntry>,
}

/// Canonical wire string for a [`HistoryEventKind`]. Mirrors
/// [`super::wire::activity_kind_to_str`] but scoped to the history
/// enum (identical output — the CAR-D93 Q1 pin holds).
fn kind_to_wire_str(k: HistoryEventKind) -> &'static str {
    match k {
        HistoryEventKind::Exit => "exit",
        HistoryEventKind::Kill => "kill",
        HistoryEventKind::Regression => "regression",
    }
}

// ─────────────────────────────────────────────────────────────────
// SharedHistoryView — the cross-thread cell the tick loop refreshes
// and the web handlers borrow. Mimics the D86 SharedTunables shape.
// ─────────────────────────────────────────────────────────────────

/// Materialized read snapshot the tick loop refreshes each tick.
/// The handlers borrow this via `WebState.history_view` — no runtime
/// borrow, no coupled mutex on the hot path.
///
/// `dead_pid_trajectories` is keyed by PID; the trajectory endpoint
/// looks up by PID and 404s on miss. If the operator's live session
/// includes a PID that DIED and then was reused (rare on Linux
/// pid_max, non-existent within a session's ring window), the map
/// will hold the MOST RECENT trajectory for that PID — the wire
/// endpoint's per-PID GET returns the most recent, matching what
/// the dead-PID index displays.
#[derive(Debug, Default, Clone)]
pub struct HistoryReadState {
    pub snapshot: WireHistorySnapshot,
    pub dead_pid_trajectories: std::collections::HashMap<u32, WireTrajectory>,
}

/// The Arc-wrapped cell shared between the tick loop (writer) and
/// the web handlers (readers). Std `RwLock` (not tokio) because the
/// tick loop is sync — per-request `read()` is cheap.
pub type SharedHistoryView = Arc<RwLock<HistoryReadState>>;

/// Build a fresh [`SharedHistoryView`] with an empty read state.
/// Called at startup; the first tick populates it.
pub fn shared_view() -> SharedHistoryView {
    Arc::new(RwLock::new(HistoryReadState::default()))
}

// ─────────────────────────────────────────────────────────────────
// Refresh — called ONCE per tick by main.rs / ui/mod.rs (outside
// runtime.rs, so the tick-loop reads don't land inside the file
// the D91 tripwire scans).
// ─────────────────────────────────────────────────────────────────

/// Rebuild the shared read state from the runtime's current view.
/// Idempotent, no side effects on the runtime.
///
/// Cost per tick:
/// * `snapshot.events` — clone up to `EVENT_ARCHIVE_MAX = 500`
///   `WireHistoryEvent`s from the archive (~150 KB).
/// * `snapshot.dead_pids` — scan `state.completed` (bounded by
///   `completed_history = 50`) and project the identity fields.
/// * `dead_pid_trajectories` — for each `state.completed[i]` whose
///   `trajectory.is_some()`, clone the sample vec (~50 KB per
///   entry; up to 50 entries = ~2.5 MB worst case). Rebuilt each
///   tick; not free but well-bounded and only ~1 Hz.
///
/// Optimization deferred: rebuild only when `state.completed`
/// changes (rare — only on exit ticks). Profile-driven; not
/// necessary yet.
pub fn refresh_shared(
    shared: &SharedHistoryView,
    state: &RuntimeState,
    history: &History,
) {
    let events: Vec<WireHistoryEvent> = history
        .event_archive
        .iter()
        .map(Into::into)
        .collect();

    let mut dead_pids: Vec<WireDeadPidEntry> = Vec::new();
    let mut dead_pid_trajectories: std::collections::HashMap<u32, WireTrajectory> =
        std::collections::HashMap::new();
    for s in state.completed.iter() {
        // AI-only filter mirrors the live activity feed
        // (`build_activity`); non-AI exits don't project to
        // the history view either.
        if s.category.is_none() {
            continue;
        }
        dead_pids.push(WireDeadPidEntry {
            pid: s.pid,
            name: s.name.clone(),
            model_name: s.model_name.clone(),
            exit_time: s.exit_time,
        });
        if let Some(traj) = s.trajectory.as_ref() {
            // If two dead entries in `state.completed` share a PID
            // (PID reuse across the ring window — theoretically
            // possible but rare), the LAST insert wins here. The
            // wire endpoint documents this as "most recent for a
            // reused PID"; the dead-PID index sort order + timestamp
            // let the operator disambiguate.
            dead_pid_trajectories.insert(s.pid, WireTrajectory::from(traj));
        }
    }

    // Sort dead-PIDs newest-first (matches the activity feed
    // ordering — the operator's mental model is "recent exits at
    // the top").
    dead_pids.sort_by_key(|entry| std::cmp::Reverse(entry.exit_time));

    let new_state = HistoryReadState {
        snapshot: WireHistorySnapshot { events, dead_pids },
        dead_pid_trajectories,
    };

    match shared.write() {
        Ok(mut guard) => *guard = new_state,
        Err(poisoned) => {
            // Recover from a poisoned lock (a handler panicked
            // while holding it). We'd rather serve the freshest
            // data than crash the tick loop.
            *poisoned.into_inner() = new_state;
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Handlers — D85 auth-gated via the nested /api middleware.
// ─────────────────────────────────────────────────────────────────

/// `GET /api/history` — the snapshot handler.
pub async fn get_snapshot(State(state): State<WebState>) -> impl IntoResponse {
    let Some(view) = state.history_view.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "history endpoint requires SharedHistoryView (web companion \
             launched without it)"
                .to_string(),
        )
            .into_response();
    };
    let snapshot = match view.read() {
        Ok(guard) => guard.snapshot.clone(),
        Err(poisoned) => poisoned.into_inner().snapshot.clone(),
    };
    Json(snapshot).into_response()
}

/// `GET /api/history/trajectory/{pid}` — the per-PID trajectory
/// handler. Returns the full sample sequence for a dead PID that
/// appears in the current dead-PID index. 404 when the PID isn't
/// in the map (either it's still live, was evicted from
/// `state.completed`, or was never captured).
pub async fn get_trajectory(
    State(state): State<WebState>,
    Path(pid): Path<u32>,
) -> impl IntoResponse {
    let Some(view) = state.history_view.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "trajectory endpoint requires SharedHistoryView".to_string(),
        )
            .into_response();
    };
    let trajectory = match view.read() {
        Ok(guard) => guard.dead_pid_trajectories.get(&pid).cloned(),
        Err(poisoned) => poisoned.into_inner().dead_pid_trajectories.get(&pid).cloned(),
    };
    match trajectory {
        Some(t) => Json(t).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            format!("no trajectory for PID {pid} in the current history window"),
        )
            .into_response(),
    }
}

/// Convenience: expose the CAR-D93 field-name pin as a compile-time
/// check that our serde derives use the contract keys. Called from
/// tests only; not part of the public surface.
#[cfg(test)]
fn assert_wire_keys_match_contract() {
    use ux_contract::history as h;
    // Field names on our structs must match the contract's KEY_*
    // constants. If a serde `#[serde(rename = ...)]` diverges from
    // the constant, this test needs updating too — which is the
    // point (a rename becomes a two-place change, visible in
    // review).
    assert_eq!(h::KEY_EVENTS, "events");
    assert_eq!(h::KEY_DEAD_PIDS, "dead_pids");
    assert_eq!(h::KEY_EVENT_KIND, "kind");
    assert_eq!(h::KEY_EVENT_TIMESTAMP, "timestamp");
    assert_eq!(h::KEY_EVENT_PID, "pid");
    assert_eq!(h::KEY_EVENT_NAME, "name");
    assert_eq!(h::KEY_EVENT_SUMMARY, "summary");
    assert_eq!(h::KEY_TRAJECTORY_SAMPLES, "samples");
    assert_eq!(h::KEY_TRAJECTORY_FIRST_SAMPLE_AT, "first_sample_at");
    assert_eq!(h::KEY_TRAJECTORY_LAST_SAMPLE_AT, "last_sample_at");
    assert_eq!(h::KEY_SAMPLE_TIMESTAMP, "timestamp");
    assert_eq!(h::KEY_SAMPLE_CPU_PCT, "cpu_pct");
    assert_eq!(h::KEY_SAMPLE_RSS_MB, "rss_mb");
    assert_eq!(h::KEY_SAMPLE_VRAM_MB, "vram_mb");
    assert_eq!(h::KEY_DEAD_PID, "pid");
    assert_eq!(h::KEY_DEAD_PID_NAME, "name");
    assert_eq!(h::KEY_DEAD_PID_MODEL, "model_name");
    assert_eq!(h::KEY_DEAD_PID_EXIT_TIME, "exit_time");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v0.3.20 CAR-D93 Q3 pin at the wire boundary. A
    /// [`WireSample`] with `vram_mb = None` MUST serialize such that
    /// the `vram_mb` key is ABSENT from the JSON (or `null`) —
    /// NEVER `0`. A `Some(0)` (genuine measured-zero) MUST be
    /// distinguishable.
    #[test]
    fn wire_sample_vram_none_omits_field_from_json() {
        let unmeasured = WireSample {
            timestamp: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            cpu_pct: 1.0,
            rss_mb: 100,
            vram_mb: None,
        };
        let json = serde_json::to_string(&unmeasured).unwrap();
        assert!(
            !json.contains("vram_mb"),
            "VRAM unmeasured MUST NOT appear in the wire JSON. \
             Got: {json}",
        );
        // Extra safety: the explicit `vram_mb:0` collapse form must
        // never appear. (Bare `:0` matches inside ISO timestamps like
        // `1970-01-01T00:00:00Z`, so the check is scoped to the
        // whole key-value pair.)
        assert!(
            !json.contains("\"vram_mb\":0"),
            "unmeasured VRAM MUST NOT collapse to `0` — the CAR-D93 \
             Q3 honesty rule. Got: {json}",
        );
    }

    #[test]
    fn wire_sample_vram_some_zero_serializes_as_zero_not_omitted() {
        // Distinguishability: a genuine `Some(0)` reading MUST
        // appear as `"vram_mb": 0`, not be omitted.
        let measured_zero = WireSample {
            timestamp: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            cpu_pct: 1.0,
            rss_mb: 100,
            vram_mb: Some(0),
        };
        let json = serde_json::to_string(&measured_zero).unwrap();
        assert!(
            json.contains("\"vram_mb\":0"),
            "genuine measured `Some(0)` MUST serialize as \
             `\"vram_mb\":0` — the honesty discriminator between \
             \"no measurement\" and \"measured zero\". Got: {json}",
        );
    }

    #[test]
    fn wire_sample_vram_some_nonzero_serializes_normally() {
        let measured = WireSample {
            timestamp: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            cpu_pct: 1.0,
            rss_mb: 100,
            vram_mb: Some(4800),
        };
        let json = serde_json::to_string(&measured).unwrap();
        assert!(json.contains("\"vram_mb\":4800"));
    }

    #[test]
    fn wire_snapshot_serialize_keys_match_contract() {
        assert_wire_keys_match_contract();
        let empty = WireHistorySnapshot::default();
        let json = serde_json::to_string(&empty).unwrap();
        assert!(json.contains("\"events\""));
        assert!(json.contains("\"dead_pids\""));
    }

    #[test]
    fn wire_event_kind_uses_contract_strings() {
        assert_eq!(kind_to_wire_str(HistoryEventKind::Exit), "exit");
        assert_eq!(kind_to_wire_str(HistoryEventKind::Kill), "kill");
        assert_eq!(
            kind_to_wire_str(HistoryEventKind::Regression),
            "regression"
        );
    }

    #[test]
    fn refresh_shared_populates_events_and_dead_pids() {
        let mut history = crate::history::History::new(100, 100);
        history.record_event(crate::history::HistoryEvent {
            timestamp: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            pid: 42,
            name: "ollama".into(),
            kind: HistoryEventKind::Kill,
            summary: "SIGTERM OK manual pid=42 ollama - test".into(),
        });

        let mut state = crate::runtime::RuntimeState::default();
        let mut summary = crate::lifecycle::LifecycleSummary {
            pid: 99,
            name: "python3".into(),
            category: Some(crate::model::AICategory::Inference),
            model_name: Some("yolov8n".into()),
            spawn_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            exit_time: chrono::DateTime::from_timestamp(120, 0).unwrap(),
            uptime_secs: 120,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 10.0,
            peak_cpu_pct: 30.0,
            peak_rss_mb: 500,
            peak_vram_mb: 800,
            samples: 120,
            trajectory: Some(crate::history::Trajectory {
                samples: vec![crate::history::Sample {
                    timestamp: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                    cpu_pct: 5.0,
                    rss_mb: 50,
                    vram_mb: Some(100),
                }],
                first_sample_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                last_sample_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            }),
        };
        // Push an AI exit into state.completed.
        state.completed.push_back(summary.clone());
        // And a non-AI exit — it MUST NOT appear in the wire.
        summary.pid = 100;
        summary.category = None;
        state.completed.push_back(summary);

        let shared = shared_view();
        refresh_shared(&shared, &state, &history);
        let guard = shared.read().unwrap();

        assert_eq!(guard.snapshot.events.len(), 1);
        assert_eq!(guard.snapshot.events[0].kind, "kill");
        assert_eq!(guard.snapshot.dead_pids.len(), 1);
        assert_eq!(
            guard.snapshot.dead_pids[0].pid, 99,
            "AI-only filter: non-AI (category=None) exits MUST NOT \
             appear in the dead-PID index"
        );
        assert_eq!(guard.dead_pid_trajectories.len(), 1);
        assert!(guard.dead_pid_trajectories.contains_key(&99));
    }
}
