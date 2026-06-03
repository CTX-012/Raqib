//! L5 / UX_CONTRACT.md §4 — alert state machine.
//!
//! Pure data layer: decides when an alert fires, when it auto-clears,
//! when it's ack-suppressed, and when it can re-fire. The render
//! layer (alert region above the header) is L6 — this module never
//! touches `ratatui` types.
//!
//! ## States per (scope, alert_id) slot
//!
//! - **Idle**: condition not currently breaching, no active alert.
//!   Slots in this state are not stored (kept implicit) so the map
//!   doesn't grow without bound.
//! - **Pending(since)**: sustain-gated alert in its 0..ALERT_SUSTAIN_SECS
//!   window. Reverts to Idle if the breach stops before sustain
//!   completes.
//! - **Active(entry)**: alert has fired (sustain reached, or instant
//!   for non-sustain-gated). Stays Active while breach holds.
//!   Auto-clears (Idle, emits Cleared event) when breach stops.
//! - **Suppressed**: user ack'd while breach was still holding. Stays
//!   Suppressed until breach stops (returns to Idle), at which point
//!   the slot is eligible to re-fire if breach later recurs. Per
//!   §4, "Re-fires if condition recurs."
//!
//! ## Sustain gating
//!
//! Per §4, only `Vram/Ram/KvPressure` are sustain-gated (5s
//! continuous breach required). `GovernorArmed`, `OomDetected`, and
//! `WorkloadExited` are instant-fire — the moment the caller
//! observes them, the alert fires.
//!
//! ## Ack semantics
//!
//! `ack_all` moves every Active slot to Suppressed. The alert is
//! removed from `visible()` immediately. The underlying condition is
//! NOT cleared — the caller continues to observe the same metric. If
//! the breach stops, Suppressed → Idle (slot removed). If the breach
//! later recurs (Idle → Pending/Active), the alert re-fires per
//! contract §4.
//!
//! ## Priority
//!
//! Contract §4 caps `visible()` at `ALERT_MAX_VISIBLE = 3`. The
//! contract is silent on prioritisation — this module pins:
//! Critical-tier (`GovernorArmed`, `OomDetected`, `WorkloadExited`)
//! before Attention-tier (`VramPressure`, `RamPressure`,
//! `KvPressure`); within a tier, oldest `fired_at` first. If a future
//! row wants this in the contract, file a CAR.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ux_contract::AlertId;
use ux_contract::thresholds::ALERT_SUSTAIN_SECS;

/// Identifies what an alert applies to. RAM pressure is the only
/// system-wide alert in v0.3 §4 (its template names no workload);
/// every other alert binds to a single PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertScope {
    /// One alert slot per workload PID.
    Workload(u32),
    /// One system-wide slot. Used for `AlertId::RamPressure`.
    System,
}

/// Record of a fired alert. Carries the data the §4 template needs
/// to render (`{workload}`, `{pid}`, `{reason}` substitutions);
/// concrete templating is L6's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertEntry {
    pub alert_id: AlertId,
    pub scope: AlertScope,
    /// PID for `Workload` scope; `None` for `System`. Stored
    /// separately from the scope so callers don't have to pattern-
    /// match the enum on every render call.
    pub pid: Option<u32>,
    /// Workload display name. Empty string for `System`-scoped.
    pub workload_name: String,
    pub fired_at: Instant,
    /// L8 — fire-time reason for `WorkloadExited` (e.g. "segfault",
    /// "exit code 139", "killed by governor (rate limited)"). Stored
    /// on the entry because the workload is gone after exit and the
    /// reason can't be reconstructed from `RuntimeState` at render
    /// time. `None` for every other alert (their templates have no
    /// `{reason}` placeholder).
    pub reason: Option<String>,
}

/// Caller-side description of the (scope, name) being observed.
/// Cheap to construct on every tick; the workload name is borrowed
/// and only cloned into an `AlertEntry` when an alert actually fires.
#[derive(Debug, Clone, Copy)]
pub struct WorkloadRef<'a> {
    pub scope: AlertScope,
    pub name: &'a str,
}

impl<'a> WorkloadRef<'a> {
    pub fn workload(pid: u32, name: &'a str) -> Self {
        Self {
            scope: AlertScope::Workload(pid),
            name,
        }
    }
    pub fn system() -> Self {
        Self {
            scope: AlertScope::System,
            name: "",
        }
    }
}

/// Side-effect emitted by `observe()` when a slot crosses a
/// boundary. Caller writes Fired/Cleared events to the Activity
/// panel per §4 ("Each raise + ack writes to Activity panel").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertEvent {
    Fired(AlertId),
    Cleared(AlertId),
}

#[derive(Debug, Clone)]
enum SlotState {
    Pending(Instant),
    Active(AlertEntry),
    Suppressed,
}

/// Per-(scope, alert_id) slot map. `Idle` is the implicit absent
/// state — slots are removed when they become Idle so the map stays
/// bounded by the number of currently-firing or pending alerts.
#[derive(Debug, Default, Clone)]
pub struct AlertState {
    slots: HashMap<(AlertScope, AlertId), SlotState>,
}

impl AlertState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Per-tick update for one (scope, alert) pair. `breaching`
    /// reflects whether the condition holds *right now* — sustain
    /// gating happens inside.
    pub fn observe(
        &mut self,
        now: Instant,
        workload: WorkloadRef,
        alert_id: AlertId,
        breaching: bool,
    ) -> Option<AlertEvent> {
        let key = (workload.scope, alert_id);
        let current = self.slots.remove(&key);
        let (next, event) = transition(current, now, workload, alert_id, breaching);
        if let Some(state) = next {
            self.slots.insert(key, state);
        }
        event
    }

    /// Acknowledge every currently-Active alert. Returns the count
    /// of alerts ack'd. Pending alerts (still in their sustain
    /// window) are NOT ack'd — they haven't fired yet.
    ///
    /// One-shot exit-driven alerts (`OomDetected`, `WorkloadExited`)
    /// are *deleted* from the slot map on ack rather than moved to
    /// `Suppressed`. Their condition is "this PID exited with X
    /// reason", which can never become false (the exit is in the
    /// past), so a Suppressed slot would never transition back to
    /// Idle and the map would leak. Pressure-driven alerts and
    /// `GovernorArmed` move to `Suppressed` so re-fire on
    /// recurrence works per UX_CONTRACT.md §4.
    pub fn ack_all(&mut self) -> usize {
        let active_keys: Vec<(AlertScope, AlertId)> = self
            .slots
            .iter()
            .filter(|(_, s)| matches!(s, SlotState::Active(_)))
            .map(|(k, _)| *k)
            .collect();
        let count = active_keys.len();
        for key in active_keys {
            if is_one_shot_exit(key.1) {
                self.slots.remove(&key);
            } else {
                self.slots.insert(key, SlotState::Suppressed);
            }
        }
        count
    }

    /// Fire an exit-driven alert immediately, capturing the reason
    /// at fire time. Bypasses the sustain gate (these alerts are
    /// instant-fire by definition) and the per-tick observe path
    /// (the firing site is the lifecycle exit hook in `runtime.rs`,
    /// not the metric-pressure scan).
    ///
    /// If a slot for the same `(scope, alert_id)` already exists,
    /// the entry is replaced. Returns `Some(Fired(...))` only on
    /// the first fire — replacement on an already-Active slot
    /// returns `None` so callers don't double-log.
    pub fn observe_exit(
        &mut self,
        now: Instant,
        workload: WorkloadRef<'_>,
        alert_id: AlertId,
        reason: Option<String>,
    ) -> Option<AlertEvent> {
        let key = (workload.scope, alert_id);
        let was_active = matches!(self.slots.get(&key), Some(SlotState::Active(_)));
        let entry = AlertEntry {
            alert_id,
            scope: workload.scope,
            pid: match workload.scope {
                AlertScope::Workload(pid) => Some(pid),
                AlertScope::System => None,
            },
            workload_name: workload.name.to_string(),
            fired_at: now,
            reason,
        };
        self.slots.insert(key, SlotState::Active(entry));
        if was_active {
            None
        } else {
            Some(AlertEvent::Fired(alert_id))
        }
    }

    /// Currently-visible alerts: at most `ALERT_MAX_VISIBLE`, sorted
    /// by tier (Critical first) then `fired_at` (oldest first).
    pub fn visible(&self) -> Vec<&AlertEntry> {
        let mut active: Vec<&AlertEntry> = self
            .slots
            .values()
            .filter_map(|s| match s {
                SlotState::Active(entry) => Some(entry),
                _ => None,
            })
            .collect();
        active.sort_by_key(|e| (priority_tier(e.alert_id), e.fired_at));
        active
            .into_iter()
            .take(ux_contract::ALERT_MAX_VISIBLE)
            .collect()
    }

    /// Total active alerts (uncapped). `visible().len()` ≤ this. The
    /// difference is what the render layer (L6) shows as `+N more`.
    pub fn active_count(&self) -> usize {
        self.slots
            .values()
            .filter(|s| matches!(s, SlotState::Active(_)))
            .count()
    }
}

fn is_sustain_gated(alert: AlertId) -> bool {
    matches!(
        alert,
        AlertId::VramPressure | AlertId::RamPressure | AlertId::KvPressure
    )
}

/// Exit-driven alerts: their condition (workload exited with X
/// reason) is in the past and can never resolve, so ack deletes the
/// slot rather than moving it to `Suppressed` (which would leak).
fn is_one_shot_exit(alert: AlertId) -> bool {
    matches!(alert, AlertId::OomDetected | AlertId::WorkloadExited)
}

fn priority_tier(alert: AlertId) -> u8 {
    // Lower = higher priority (sorts first).
    match alert {
        AlertId::GovernorArmed | AlertId::OomDetected | AlertId::WorkloadExited => 0,
        // v1.2.0 / DISPATCH 45 — ThermalPressure shares the
        // pressure tier with VRAM/RAM/KV per
        // `panels::alerts::alert_tier`. Same priority bucket.
        AlertId::VramPressure
        | AlertId::RamPressure
        | AlertId::KvPressure
        | AlertId::ThermalPressure => 1,
    }
}

fn make_entry(now: Instant, workload: WorkloadRef, alert_id: AlertId) -> AlertEntry {
    AlertEntry {
        alert_id,
        scope: workload.scope,
        pid: match workload.scope {
            AlertScope::Workload(pid) => Some(pid),
            AlertScope::System => None,
        },
        workload_name: workload.name.to_string(),
        fired_at: now,
        reason: None,
    }
}

fn transition(
    current: Option<SlotState>,
    now: Instant,
    workload: WorkloadRef,
    alert_id: AlertId,
    breaching: bool,
) -> (Option<SlotState>, Option<AlertEvent>) {
    match (current, breaching) {
        // Idle (implicit absent): only enter Pending or Active when
        // a breach starts.
        (None, true) => {
            if is_sustain_gated(alert_id) {
                (Some(SlotState::Pending(now)), None)
            } else {
                let entry = make_entry(now, workload, alert_id);
                (
                    Some(SlotState::Active(entry)),
                    Some(AlertEvent::Fired(alert_id)),
                )
            }
        }
        (None, false) => (None, None),

        // Pending: still breaching → check sustain, otherwise reset.
        (Some(SlotState::Pending(since)), true) => {
            if now.duration_since(since) >= Duration::from_secs(ALERT_SUSTAIN_SECS) {
                let entry = AlertEntry {
                    alert_id,
                    scope: workload.scope,
                    pid: match workload.scope {
                        AlertScope::Workload(pid) => Some(pid),
                        AlertScope::System => None,
                    },
                    workload_name: workload.name.to_string(),
                    // fired_at = now so visible-priority ordering
                    // reflects when the alert actually entered the
                    // banner, not when the breach started.
                    fired_at: now,
                    reason: None,
                };
                (
                    Some(SlotState::Active(entry)),
                    Some(AlertEvent::Fired(alert_id)),
                )
            } else {
                (Some(SlotState::Pending(since)), None)
            }
        }
        (Some(SlotState::Pending(_)), false) => (None, None),

        // Active: stay until breach stops.
        (Some(SlotState::Active(entry)), true) => (Some(SlotState::Active(entry)), None),
        (Some(SlotState::Active(_)), false) => (None, Some(AlertEvent::Cleared(alert_id))),

        // Suppressed: stay until breach resolves; then return to
        // Idle so a future breach can re-fire normally.
        (Some(SlotState::Suppressed), true) => (Some(SlotState::Suppressed), None),
        (Some(SlotState::Suppressed), false) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn after(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    fn after_ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn pid(p: u32) -> WorkloadRef<'static> {
        WorkloadRef::workload(p, "phi3")
    }

    // ====================================================================
    // Sustain-gate behavior
    // ====================================================================

    #[test]
    fn vram_pressure_does_not_fire_for_first_4_secs() {
        let mut state = AlertState::new();
        let start = t0();
        // Tick at t=0: enters Pending.
        assert_eq!(state.observe(start, pid(42), AlertId::VramPressure, true), None);
        // Subsequent ticks within the sustain window: still Pending.
        for s in 1..=4 {
            assert_eq!(
                state.observe(after(start, s), pid(42), AlertId::VramPressure, true),
                None,
                "must not fire at t={s}s (sustain is 5s)"
            );
        }
        assert_eq!(state.visible().len(), 0);
        assert_eq!(state.active_count(), 0);
    }

    #[test]
    fn vram_pressure_fires_at_5_secs_continuous() {
        let mut state = AlertState::new();
        let start = t0();
        assert_eq!(state.observe(start, pid(42), AlertId::VramPressure, true), None);
        let event = state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::VramPressure)));
        assert_eq!(state.visible().len(), 1);
        assert_eq!(state.active_count(), 1);
    }

    #[test]
    fn vram_pressure_resets_sustain_when_metric_drops() {
        let mut state = AlertState::new();
        let start = t0();
        // Breach for 3s.
        state.observe(start, pid(42), AlertId::VramPressure, true);
        state.observe(after(start, 3), pid(42), AlertId::VramPressure, true);
        // Drop below threshold → sustain resets.
        state.observe(after(start, 4), pid(42), AlertId::VramPressure, false);
        // Breach resumes — fresh 5s clock starts here. At +1s after
        // the resume, still Pending, no fire.
        state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        let event = state.observe(after(start, 6), pid(42), AlertId::VramPressure, true);
        assert_eq!(event, None, "must not fire only 1s into the new breach");
    }

    #[test]
    fn vram_pressure_fires_with_subsecond_precision() {
        // Defensive: a tick at exactly 4999ms is still Pending; at
        // 5000ms or later, fires. This pins the boundary so a future
        // refactor that uses `as_secs()` (which floors) doesn't fire
        // a tick early.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        assert_eq!(
            state.observe(after_ms(start, 4_999), pid(42), AlertId::VramPressure, true),
            None,
            "4999ms is below the 5000ms gate"
        );
        assert_eq!(
            state.observe(after_ms(start, 5_000), pid(42), AlertId::VramPressure, true),
            Some(AlertEvent::Fired(AlertId::VramPressure))
        );
    }

    // ====================================================================
    // Per-(scope, alert) independence
    // ====================================================================

    #[test]
    fn kv_pressure_independent_from_vram_pressure() {
        // Both alerts on the same PID — each has its own slot, so
        // VRAM firing doesn't preempt KV's sustain timer.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        // KV starts breaching 3s later.
        state.observe(after(start, 3), pid(42), AlertId::KvPressure, true);
        // VRAM fires at t=5; KV should still be Pending (only 2s in).
        state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        assert_eq!(
            state.observe(after(start, 5), pid(42), AlertId::KvPressure, true),
            None,
            "KV must still be Pending"
        );
        // KV fires at t=8 (5s after start at t=3).
        let event = state.observe(after(start, 8), pid(42), AlertId::KvPressure, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::KvPressure)));
    }

    #[test]
    fn vram_pressure_independent_per_workload() {
        // Same alert, different PIDs — independent timers.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        state.observe(after(start, 3), pid(99), AlertId::VramPressure, true);
        // PID 42 fires at t=5s.
        let event_42 = state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        assert_eq!(event_42, Some(AlertEvent::Fired(AlertId::VramPressure)));
        // PID 99 still Pending at t=5s.
        let event_99 = state.observe(after(start, 5), pid(99), AlertId::VramPressure, true);
        assert_eq!(event_99, None);
    }

    // ====================================================================
    // Instant-fire alerts (no sustain)
    // ====================================================================

    #[test]
    fn governor_armed_fires_immediately() {
        let mut state = AlertState::new();
        let event = state.observe(t0(), pid(42), AlertId::GovernorArmed, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::GovernorArmed)));
        assert_eq!(state.visible().len(), 1);
    }

    #[test]
    fn oom_detected_fires_immediately() {
        let mut state = AlertState::new();
        let event = state.observe(t0(), pid(42), AlertId::OomDetected, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::OomDetected)));
    }

    #[test]
    fn workload_exited_fires_immediately() {
        let mut state = AlertState::new();
        let event = state.observe(t0(), pid(42), AlertId::WorkloadExited, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::WorkloadExited)));
    }

    // ====================================================================
    // Auto-clear when condition resolves
    // ====================================================================

    #[test]
    fn auto_clears_when_condition_resolves() {
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        assert_eq!(state.visible().len(), 1);
        // Breach stops.
        let event = state.observe(after(start, 6), pid(42), AlertId::VramPressure, false);
        assert_eq!(event, Some(AlertEvent::Cleared(AlertId::VramPressure)));
        assert_eq!(state.visible().len(), 0);
    }

    // ====================================================================
    // Acknowledge
    // ====================================================================

    #[test]
    fn ack_removes_from_visible() {
        let mut state = AlertState::new();
        state.observe(t0(), pid(42), AlertId::GovernorArmed, true);
        assert_eq!(state.visible().len(), 1);
        let count = state.ack_all();
        assert_eq!(count, 1);
        assert_eq!(state.visible().len(), 0);
    }

    #[test]
    fn ack_does_not_clear_underlying_condition() {
        // Per §4: ack hides the alert from visible, but the condition
        // is still observed. Re-fire requires the breach to resolve
        // and recur.
        let mut state = AlertState::new();
        let start = t0();
        // Fire VRAM via sustain.
        state.observe(start, pid(42), AlertId::VramPressure, true);
        state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        state.ack_all();
        assert_eq!(state.visible().len(), 0);
        // Continue observing breach — must NOT re-fire.
        let event = state.observe(after(start, 6), pid(42), AlertId::VramPressure, true);
        assert_eq!(event, None);
        assert_eq!(state.visible().len(), 0);
    }

    #[test]
    fn acked_then_condition_resolves_then_recurs_refires() {
        // Suppressed → Idle (on resolve) → Pending → Active flow.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        state.ack_all();
        // Condition resolves: Suppressed → Idle.
        state.observe(after(start, 6), pid(42), AlertId::VramPressure, false);
        // Breach recurs: Idle → Pending. Sustain timer restarts.
        state.observe(after(start, 7), pid(42), AlertId::VramPressure, true);
        // 4s into the new breach: still Pending.
        assert_eq!(
            state.observe(after(start, 11), pid(42), AlertId::VramPressure, true),
            None
        );
        // 5s into the new breach: re-fires.
        let event = state.observe(after(start, 12), pid(42), AlertId::VramPressure, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::VramPressure)));
    }

    #[test]
    fn ack_does_not_clear_pending_alerts() {
        // Defensive: only Active alerts are ack-clearable. A breach
        // in its sustain window has not fired yet — ack_all should
        // leave it alone so it can fire normally when sustain
        // completes.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        let count = state.ack_all();
        assert_eq!(count, 0);
        // Sustain still ticks normally.
        let event = state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::VramPressure)));
    }

    // ====================================================================
    // Visibility cap + priority
    // ====================================================================

    #[test]
    fn max_visible_caps_at_3() {
        let mut state = AlertState::new();
        let start = t0();
        // Fire 5 instant alerts on different PIDs.
        for (i, p) in [10u32, 11, 12, 13, 14].iter().enumerate() {
            let when = after_ms(start, i as u64 * 10);
            state.observe(when, pid(*p), AlertId::OomDetected, true);
        }
        assert_eq!(state.active_count(), 5);
        assert_eq!(state.visible().len(), ux_contract::ALERT_MAX_VISIBLE);
        assert_eq!(state.visible().len(), 3);
    }

    #[test]
    fn priority_critical_above_attention() {
        // Older Attention alert + newer Critical alert: Critical wins
        // visibility despite being newer.
        let mut state = AlertState::new();
        let start = t0();
        // Fire VRAM pressure (Attention tier) at t=5s.
        state.observe(start, pid(10), AlertId::VramPressure, true);
        state.observe(after(start, 5), pid(10), AlertId::VramPressure, true);
        // Fire OOM (Critical tier) at t=10s.
        state.observe(after(start, 10), pid(11), AlertId::OomDetected, true);
        let visible = state.visible();
        assert_eq!(visible.len(), 2);
        // Critical (OomDetected) sorts first despite later fired_at.
        assert_eq!(visible[0].alert_id, AlertId::OomDetected);
        assert_eq!(visible[1].alert_id, AlertId::VramPressure);
    }

    #[test]
    fn visible_returns_oldest_first_within_same_tier() {
        let mut state = AlertState::new();
        let start = t0();
        // Three OOMs on different PIDs, fired at different times.
        state.observe(after(start, 1), pid(10), AlertId::OomDetected, true);
        state.observe(after(start, 2), pid(11), AlertId::OomDetected, true);
        state.observe(after(start, 3), pid(12), AlertId::OomDetected, true);
        let visible = state.visible();
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].pid, Some(10));
        assert_eq!(visible[1].pid, Some(11));
        assert_eq!(visible[2].pid, Some(12));
    }

    // ====================================================================
    // Scope (system vs workload)
    // ====================================================================

    #[test]
    fn ram_pressure_uses_system_scope() {
        // ALERT_RAM_PRESSURE has no `{workload}`/`{pid}` placeholder
        // in its template — it's the only system-scope alert in
        // v0.3 §4.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, WorkloadRef::system(), AlertId::RamPressure, true);
        state.observe(
            after(start, 5),
            WorkloadRef::system(),
            AlertId::RamPressure,
            true,
        );
        let visible = state.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].scope, AlertScope::System);
        assert_eq!(visible[0].pid, None);
        assert_eq!(visible[0].workload_name, "");
    }

    #[test]
    fn observe_with_no_breach_idle_returns_none() {
        let mut state = AlertState::new();
        let event = state.observe(t0(), pid(42), AlertId::VramPressure, false);
        assert_eq!(event, None);
        assert_eq!(state.visible().len(), 0);
    }

    // ====================================================================
    // L8 — observe_exit + one-shot ack semantics
    // ====================================================================

    #[test]
    fn observe_exit_fires_immediately_with_reason_captured() {
        let mut state = AlertState::new();
        let event = state.observe_exit(
            t0(),
            pid(42),
            AlertId::WorkloadExited,
            Some("exit code 139".into()),
        );
        assert_eq!(event, Some(AlertEvent::Fired(AlertId::WorkloadExited)));
        let visible = state.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].reason.as_deref(), Some("exit code 139"));
    }

    #[test]
    fn observe_exit_replacement_returns_none_no_double_log() {
        // A second exit event for the same (PID, alert_id) replaces
        // the entry but doesn't re-fire — caller doesn't need to
        // log the alert twice.
        let mut state = AlertState::new();
        state.observe_exit(t0(), pid(42), AlertId::OomDetected, None);
        let event = state.observe_exit(t0(), pid(42), AlertId::OomDetected, None);
        assert_eq!(event, None);
        assert_eq!(state.visible().len(), 1);
    }

    #[test]
    fn one_shot_exit_alert_deleted_on_ack_not_suppressed() {
        // L8 — exit alerts are deleted from the slot map on ack
        // (their condition is in the past and can't recur, so
        // Suppressed would leak). After ack, the slot is gone — a
        // future observe(false) on the same key would see None
        // (Idle), not a Suppressed slot.
        let mut state = AlertState::new();
        state.observe_exit(
            t0(),
            pid(42),
            AlertId::WorkloadExited,
            Some("segfault".into()),
        );
        assert_eq!(state.visible().len(), 1);
        let count = state.ack_all();
        assert_eq!(count, 1);
        assert_eq!(state.visible().len(), 0);
        // The slot is gone (Idle). A subsequent fire creates a new
        // entry rather than re-using the Suppressed one.
        let event = state.observe_exit(
            t0(),
            pid(42),
            AlertId::WorkloadExited,
            Some("exit code 42".into()),
        );
        assert_eq!(
            event,
            Some(AlertEvent::Fired(AlertId::WorkloadExited)),
            "slot should be Idle after ack of one-shot, so a new fire emits Fired"
        );
    }

    #[test]
    fn pressure_alert_ack_still_moves_to_suppressed() {
        // Defensive: the L8 one-shot deletion only touches
        // OomDetected and WorkloadExited. Pressure alerts continue
        // to use Suppressed so the recurrence path
        // (Suppressed → Idle → Pending → Active) works per L5.
        let mut state = AlertState::new();
        let start = t0();
        state.observe(start, pid(42), AlertId::VramPressure, true);
        state.observe(after(start, 5), pid(42), AlertId::VramPressure, true);
        state.ack_all();
        // Continue observing breach — Suppressed must NOT re-fire.
        let event = state.observe(after(start, 6), pid(42), AlertId::VramPressure, true);
        assert_eq!(
            event, None,
            "pressure-driven ack still moves to Suppressed, not deleted"
        );
    }

    #[test]
    fn exit_alert_persists_after_workload_removed_from_registry() {
        // L8 lifetime decision (option (b)): the alert sticks until
        // ack regardless of whether the underlying workload still
        // appears in any registry. Once observe_exit fires, no
        // subsequent observe(false) can clear it (the per-tick path
        // doesn't iterate one-shot alert ids), and only ack_all
        // removes the slot.
        let mut state = AlertState::new();
        state.observe_exit(
            t0(),
            pid(42),
            AlertId::OomDetected,
            None,
        );
        // Simulate many ticks passing with the PID gone — the
        // per-tick observe path never touches OomDetected slots, so
        // the alert is unaffected by elapsed time.
        for _ in 0..100 {
            // No call: the per-tick observation path skips one-shot
            // alert ids entirely (App::observe_alerts iterates
            // VramPressure / RamPressure / KvPressure / GovernorArmed
            // only).
        }
        assert_eq!(state.visible().len(), 1);
    }
}
