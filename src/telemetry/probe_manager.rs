//! DISPATCH connectivity — async HTTP health-probe manager.
//!
//! Runs on the dispatcher's tokio runtime (NOT the sync tick loop),
//! probes each PID's derived HTTP endpoint every 5 s with a 500 ms
//! per-probe timeout, and writes results into a shared `Arc<RwLock<…>>`
//! that the tick loop drains once per tick. The tick loop reconciles
//! the PID → endpoint mapping (adding new PIDs as `Checking`, dropping
//! exited PIDs).
//!
//! ## Safety property — THE probe MUST NOT block the tick loop
//!
//! The dispatcher's tokio runtime is `rt-multi-thread` (2 workers per
//! `src/telemetry/dispatcher.rs:78-82`). The probe manager owns a
//! long-lived `spawn(async move { … })` task that does:
//!
//!   1. `tokio::time::interval(5s).tick().await` — suspends, no busy loop.
//!   2. Snapshots the PID list via `shared.read()` (holds lock briefly).
//!   3. Fires N concurrent `reqwest::get(endpoint)` with per-request
//!      500 ms timeout.
//!   4. Updates `shared.write()` with results.
//!
//! The sync tick loop only ever `shared.read()`s or `shared.write()`s
//! very briefly (nanoseconds — a HashMap clone / retain / insert-if-
//! absent). No cross-task blocking; no `.block_on()`.
//!
//! ## Honesty rules (PENDING.md ratified spec)
//!
//! * **First-load state is `Checking`**, NEVER `Unreachable`. A PID
//!   that just appeared has never been probed; saying "down" would lie.
//! * **Debounce-after-2**: two CONSECUTIVE failures before flipping to
//!   `Unreachable`. A single dropped packet keeps the prior status
//!   (typically `Ok`), preventing the chip from flapping. Mirrors the
//!   sampler-side idle-debounce shape at `samplers/ollama_api.rs:117`.
//! * A single success resets the streak to 0 and sets status to `Ok`.
//! * Excluded workloads (embeddings, agents, ROS2 — those with no
//!   HTTP endpoint per `probe_endpoint::derive_probe_endpoint`)
//!   NEVER appear in the shared state, so the wire mapper emits no
//!   `probe_status` for them (no chip renders).

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Interval between probe rounds. 5 s per the ratified spec — long
/// enough that N × 500 ms of concurrent probes can't melt the
/// dispatcher's tokio pool; short enough that a wall-monitor observer
/// sees a stalled backend within one screen-refresh cycle.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Per-probe HTTP timeout. 500 ms matches the sampler timeouts
/// (`SCRAPE_TIMEOUT` in `samplers/vllm_prometheus.rs:33` and
/// `samplers/llama_cpp_server.rs:26`). If a probe hangs longer than
/// this, the tokio join treats it as failure — the probe task never
/// blocks longer than one interval.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Consecutive failures needed before flipping to `Unreachable`.
/// Mirrors the sampler-side idle debounce at
/// `samplers/ollama_api.rs::OLLAMA_IDLE_DEBOUNCE_SAMPLES = 2`.
pub const FAILURE_DEBOUNCE: u8 = 2;

/// Current probe result for a single PID. Wire-serializes as
/// snake_case via [`ProbeStatus::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Last probe succeeded (any 2xx). Streak reset to 0.
    Ok,
    /// PID has an endpoint but no probe has completed yet (first-load
    /// state), OR one probe failed but debounce hasn't been reached.
    /// NEVER shown as "down."
    Checking,
    /// At least `FAILURE_DEBOUNCE` consecutive failures.
    Unreachable,
}

impl ProbeStatus {
    /// Wire-stable string projection. The frontend pattern-matches on
    /// these three literals — do NOT change without a coordinated
    /// wire schema update.
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStatus::Ok => "ok",
            ProbeStatus::Checking => "checking",
            ProbeStatus::Unreachable => "unreachable",
        }
    }
}

/// One entry in the shared state — the endpoint we're probing, its
/// current status, and the consecutive-failure counter for the
/// debounce.
#[derive(Debug, Clone)]
struct ProbeEntry {
    endpoint: String,
    status: ProbeStatus,
    consecutive_failures: u8,
}

/// The shared state written by the async probe task and read by the
/// sync tick loop. Keyed by PID; only contains entries for PIDs whose
/// workload has a derived HTTP endpoint (the tick loop's reconcile
/// step guarantees this).
#[derive(Debug, Default)]
pub struct ProbeState {
    per_pid: HashMap<u32, ProbeEntry>,
}

impl ProbeState {
    /// Update the tracked PID→endpoint set to match `endpoints`.
    /// Called by the tick loop after each tick's annotation pass.
    ///
    /// * PIDs that are NEW (in `endpoints`, not in `per_pid`) get
    ///   inserted with status `Checking` — the honest first-load
    ///   state until the probe task returns their first result.
    /// * PIDs whose endpoint CHANGED (rare — cmdline is stable per
    ///   process instance) reset the streak + status to `Checking`,
    ///   since the prior status was about a different URL.
    /// * PIDs that DISAPPEARED (in `per_pid`, not in `endpoints`)
    ///   are dropped — the process is gone and its status is stale.
    pub fn reconcile(&mut self, endpoints: &[(u32, String)]) {
        let current: HashSet<u32> = endpoints.iter().map(|(pid, _)| *pid).collect();
        self.per_pid.retain(|pid, _| current.contains(pid));
        for (pid, endpoint) in endpoints {
            match self.per_pid.get_mut(pid) {
                Some(entry) if entry.endpoint == *endpoint => {
                    // Same PID + same endpoint — leave status alone.
                }
                Some(entry) => {
                    // Endpoint changed under the same PID; reset.
                    entry.endpoint = endpoint.clone();
                    entry.status = ProbeStatus::Checking;
                    entry.consecutive_failures = 0;
                }
                None => {
                    self.per_pid.insert(
                        *pid,
                        ProbeEntry {
                            endpoint: endpoint.clone(),
                            status: ProbeStatus::Checking,
                            consecutive_failures: 0,
                        },
                    );
                }
            }
        }
    }

    /// Snapshot `pid → status` for the wire mapper. Cheap clone
    /// (HashMap of copies of `u32 → ProbeStatus` — no strings).
    pub fn snapshot_statuses(&self) -> HashMap<u32, ProbeStatus> {
        self.per_pid
            .iter()
            .map(|(pid, e)| (*pid, e.status))
            .collect()
    }

    /// The PID → endpoint list the probe task should probe THIS
    /// round. Returned as owned strings so the async task can drop
    /// the read lock immediately.
    fn probe_targets(&self) -> Vec<(u32, String)> {
        self.per_pid
            .iter()
            .map(|(pid, e)| (*pid, e.endpoint.clone()))
            .collect()
    }

    /// Apply a probe result. Pure transition function — no I/O.
    /// Splits from the async loop so the state machine can be
    /// unit-tested without spinning up tokio.
    fn record_result(&mut self, pid: u32, ok: bool) {
        if let Some(entry) = self.per_pid.get_mut(&pid) {
            if ok {
                entry.status = ProbeStatus::Ok;
                entry.consecutive_failures = 0;
            } else {
                entry.consecutive_failures =
                    entry.consecutive_failures.saturating_add(1);
                if entry.consecutive_failures >= FAILURE_DEBOUNCE {
                    entry.status = ProbeStatus::Unreachable;
                }
                // < FAILURE_DEBOUNCE: leave prior status alone so a
                // single blip doesn't flap the chip.
            }
        }
    }
}

/// Shared handle wrapping [`ProbeState`] behind an `RwLock`. Cloned
/// into the async task (which writes) and the tick loop (which
/// reads + reconciles).
pub type SharedProbeState = Arc<RwLock<ProbeState>>;

/// Build a fresh empty shared state. Called once at Runtime::new;
/// cloned into the dispatcher's async task via
/// `Dispatcher::enable_probe_manager`.
pub fn shared_state() -> SharedProbeState {
    Arc::new(RwLock::new(ProbeState::default()))
}

/// Spawn the probe loop on the given tokio runtime. Long-lived task;
/// aborted when the runtime drops (`Dispatcher::drop` → tokio Runtime
/// drop → all spawned tasks aborted). NEVER blocks its caller.
///
/// Test hook: pass `Some(std::time::Duration::from_millis(N))` as
/// `interval_override` to run the loop faster than 5 s.
pub fn spawn(
    runtime: &tokio::runtime::Runtime,
    shared: SharedProbeState,
    interval_override: Option<Duration>,
) -> tokio::task::JoinHandle<()> {
    // ok: expect — reqwest::Client::builder is infallible when only
    // built-in options are set (no custom TLS config, no proxy); the
    // rustls-tls feature is on so the TLS stack is bundled.
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .expect("reqwest client build (rustls-tls feature is on) never fails");
    let interval = interval_override.unwrap_or(PROBE_INTERVAL);
    runtime.spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the initial immediate tick — otherwise a fresh Runtime
        // fires a probe before the tick loop has reconciled any PIDs
        // (harmless but noisy). MissedTickBehavior::Delay: after a
        // long tick delay (e.g. the process was suspended), fire once
        // and reset — don't burst-fire to "catch up."
        ticker.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Delay,
        );
        ticker.tick().await; // consume the immediate first tick
        loop {
            ticker.tick().await;
            let targets = {
                let Ok(guard) = shared.read() else {
                    // Poisoned lock — the tick loop panicked mid-write.
                    // Nothing we can do; abandon the probe task rather
                    // than loop-panic.
                    tracing::error!("probe_manager: shared state poisoned; exiting task");
                    return;
                };
                guard.probe_targets()
            };
            if targets.is_empty() {
                continue;
            }
            let probes = targets.into_iter().map(|(pid, endpoint)| {
                let client = client.clone();
                async move {
                    let ok = client
                        .get(&endpoint)
                        .send()
                        .await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false);
                    (pid, ok)
                }
            });
            let results: Vec<(u32, bool)> =
                futures_util::future::join_all(probes).await;
            if let Ok(mut guard) = shared.write() {
                for (pid, ok) in results {
                    guard.record_result(pid, ok);
                }
            } else {
                tracing::error!("probe_manager: shared state poisoned during write; exiting");
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints(pairs: &[(u32, &str)]) -> Vec<(u32, String)> {
        pairs.iter().map(|(p, u)| (*p, (*u).to_string())).collect()
    }

    /// The wire-string projection is load-bearing — the frontend
    /// literally pattern-matches on these three strings. Pin them so
    /// a rename never silently breaks the chip render.
    #[test]
    fn probe_status_wire_strings_are_locked() {
        assert_eq!(ProbeStatus::Ok.as_str(), "ok");
        assert_eq!(ProbeStatus::Checking.as_str(), "checking");
        assert_eq!(ProbeStatus::Unreachable.as_str(), "unreachable");
    }

    /// First-load: a new PID appears with `Checking`, NEVER
    /// `Unreachable`. This is THE honesty rule — showing "down"
    /// before any probe has completed would lie.
    #[test]
    fn reconcile_new_pid_enters_as_checking() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(42, "http://127.0.0.1:11434/api/ps")]));
        let snap = state.snapshot_statuses();
        assert_eq!(snap.get(&42), Some(&ProbeStatus::Checking));
    }

    /// PIDs that disappeared (process exited) are dropped from the
    /// state — otherwise the map grows unbounded across a long
    /// session with rolling processes.
    #[test]
    fn reconcile_removes_disappeared_pids() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a"), (2, "http://b")]));
        state.reconcile(&endpoints(&[(1, "http://a")]));
        let snap = state.snapshot_statuses();
        assert!(snap.contains_key(&1));
        assert!(!snap.contains_key(&2), "pid 2 disappeared → must be dropped");
    }

    /// Same PID + same endpoint across ticks preserves the last
    /// probe status (`Ok` stays `Ok`) — reconcile must NOT reset.
    #[test]
    fn reconcile_preserves_status_when_endpoint_unchanged() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a")]));
        state.record_result(1, true);
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Ok));
        // Same reconcile call — status must stay Ok, not flip back to
        // Checking.
        state.reconcile(&endpoints(&[(1, "http://a")]));
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Ok));
    }

    /// Same PID but the endpoint changed (rare — cmdline changed?
    /// PID reuse across a fast fork?) resets to `Checking` — the
    /// old status was about a different URL and would mislead.
    #[test]
    fn reconcile_resets_status_when_endpoint_changed() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a")]));
        state.record_result(1, true);
        state.reconcile(&endpoints(&[(1, "http://b")]));
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Checking));
    }

    /// A single failure does NOT flip to `Unreachable` — the
    /// debounce holds until the SECOND consecutive failure.
    #[test]
    fn one_failure_does_not_flip_to_unreachable() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a")]));
        // Seed a prior success so we can prove one failure keeps Ok.
        state.record_result(1, true);
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Ok));
        state.record_result(1, false);
        // Status must stay Ok — a single blip doesn't flap the chip.
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Ok));
    }

    /// Two consecutive failures DO flip to `Unreachable`.
    #[test]
    fn two_consecutive_failures_flip_to_unreachable() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a")]));
        state.record_result(1, false);
        state.record_result(1, false);
        assert_eq!(
            state.snapshot_statuses().get(&1),
            Some(&ProbeStatus::Unreachable),
        );
    }

    /// A single success in the middle of a failure streak resets
    /// the counter — the NEXT failure is treated as failure #1, not
    /// failure #3.
    #[test]
    fn success_resets_the_failure_streak() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a")]));
        state.record_result(1, false);
        state.record_result(1, true);
        // Now one failure — must not flip (streak reset to 0).
        state.record_result(1, false);
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Ok));
    }

    /// Recovery: `Unreachable` → success → `Ok` immediately (no
    /// debounce on the recovery direction; a live service coming
    /// back should surface right away).
    #[test]
    fn unreachable_recovers_to_ok_on_first_success() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a")]));
        state.record_result(1, false);
        state.record_result(1, false);
        assert_eq!(
            state.snapshot_statuses().get(&1),
            Some(&ProbeStatus::Unreachable),
        );
        state.record_result(1, true);
        assert_eq!(state.snapshot_statuses().get(&1), Some(&ProbeStatus::Ok));
    }

    /// probe_targets() and snapshot_statuses() match each other —
    /// every PID in one is in the other. Guards against a future
    /// refactor that adds a state field but forgets to update both
    /// projections.
    #[test]
    fn projections_are_pid_consistent() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[
            (1, "http://a"),
            (2, "http://b"),
            (3, "http://c"),
        ]));
        let targets: HashSet<u32> = state.probe_targets().into_iter().map(|(p, _)| p).collect();
        let statuses: HashSet<u32> = state.snapshot_statuses().into_keys().collect();
        assert_eq!(targets, statuses);
    }

    /// Reconciling an empty endpoint list clears the whole map —
    /// e.g. when every HTTP workload exits, the shared state
    /// drains to empty.
    #[test]
    fn reconcile_empty_clears_map() {
        let mut state = ProbeState::default();
        state.reconcile(&endpoints(&[(1, "http://a"), (2, "http://b")]));
        state.reconcile(&endpoints(&[]));
        assert!(state.snapshot_statuses().is_empty());
    }
}
