//! Cold-load disk I/O detector (latest.md Tier 2.2).
//!
//! Watches `/proc/<pid>/io` `read_bytes` for each AI process and
//! decides when the model has finished loading from disk. The model
//! load shows up as a sustained burst of reads (often hundreds of
//! MB/s on NVMe), followed by a plateau when the process settles
//! into inference. The transition from burst → plateau is the
//! cold-start boundary.
//!
//! Output is a [`crate::storage::run_store::ColdStartStats`] that
//! the runtime stamps on `RunRecord.cold_start`. Tier 3.2 then uses
//! the cold-start watermark to split steady-state metrics from
//! warm-up noise.
//!
//! Heuristic
//! ---------
//! We sample read-bytes per tick. We keep a rolling delta. When:
//!
//! * total bytes read so far exceeds a sanity floor (16 MiB; smaller
//!   than that is unlikely to be a model load), AND
//! * the most recent `PLATEAU_TICKS` samples each had a delta below
//!   `PLATEAU_RATE_BPS` bytes/s (1 MiB/s default) — i.e. effectively
//!   no I/O,
//!
//! then we declare cold-load complete and freeze the stats.
//!
//! After completion the tracker stops updating that PID's stats; the
//! result remains queryable via [`ColdLoadTracker::stats`] until the
//! runtime calls `forget(pid)` at exit.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::storage::run_store::ColdStartStats;

/// Minimum bytes to consider a "real" model load. Below this we
/// won't declare cold-load complete; tiny models loaded into FS
/// cache wouldn't have a meaningful cold-start anyway.
const MIN_BYTES_FOR_LOAD: u64 = 16 * 1024 * 1024;
/// Per-tick read rate at or below which we consider the process
/// "settled" (no longer streaming weights).
const PLATEAU_RATE_BPS: f32 = 1.0 * 1024.0 * 1024.0;
/// Number of consecutive low-rate samples before we declare plateau.
const PLATEAU_TICKS: usize = 2;
/// Hard cap on how long we wait for cold-load to complete. After
/// this elapses with no plateau, we record what we have anyway —
/// this is the path taken by streaming inference workloads that
/// never stop reading.
const HARD_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct State {
    started_at: Instant,
    last_sample_at: Option<Instant>,
    last_read_bytes: u64,
    /// Largest single-tick throughput seen so far, in bytes/sec.
    peak_throughput_bps: f32,
    /// Running sum of per-tick deltas (matches `read_bytes` but is
    /// monotonically increasing and resilient to PID reuse).
    bytes_read: u64,
    /// Trailing window of recent throughput readings (bytes/sec).
    recent_rates: Vec<f32>,
    /// Set once cold-load is declared complete.
    finalised: Option<ColdStartStats>,
}

impl State {
    fn new(initial_read_bytes: u64) -> Self {
        Self {
            started_at: Instant::now(),
            last_sample_at: None,
            last_read_bytes: initial_read_bytes,
            peak_throughput_bps: 0.0,
            bytes_read: 0,
            recent_rates: Vec::with_capacity(PLATEAU_TICKS + 1),
            finalised: None,
        }
    }
}

/// Per-PID cold-load tracker. Single-writer (the runtime tick loop).
#[derive(Debug, Default)]
pub struct ColdLoadTracker {
    by_pid: HashMap<u32, State>,
}

impl ColdLoadTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one `read_bytes` reading for `pid`. Returns the freshly-
    /// finalised `ColdStartStats` exactly once on the tick the
    /// transition fires; subsequent calls return `None` until
    /// `forget(pid)` resets the slot.
    pub fn record(&mut self, pid: u32, read_bytes: u64) -> Option<ColdStartStats> {
        let now = Instant::now();
        let state = self
            .by_pid
            .entry(pid)
            .or_insert_with(|| State::new(read_bytes));
        if state.finalised.is_some() {
            // Already done; ignore further samples until forget(pid).
            return None;
        }

        // Compute Δ since the prior tick. If this is the first
        // observation we just store the baseline.
        let delta_bytes = read_bytes.saturating_sub(state.last_read_bytes);
        let prev_at = state.last_sample_at.replace(now);
        state.last_read_bytes = read_bytes;
        state.bytes_read = state.bytes_read.saturating_add(delta_bytes);

        let dt_secs = match prev_at {
            Some(t) => now.saturating_duration_since(t).as_secs_f32(),
            None => 0.0,
        };
        let rate_bps = if dt_secs > 0.0 {
            delta_bytes as f32 / dt_secs
        } else {
            0.0
        };
        if rate_bps > state.peak_throughput_bps {
            state.peak_throughput_bps = rate_bps;
        }
        if dt_secs > 0.0 {
            // Roll the recent-rates window.
            state.recent_rates.push(rate_bps);
            if state.recent_rates.len() > PLATEAU_TICKS {
                state.recent_rates.remove(0);
            }
        }

        // Detect plateau or hard-timeout.
        let plateau_reached = state.recent_rates.len() >= PLATEAU_TICKS
            && state.recent_rates.iter().all(|r| *r <= PLATEAU_RATE_BPS)
            && state.bytes_read >= MIN_BYTES_FOR_LOAD;
        let timed_out = now.saturating_duration_since(state.started_at) >= HARD_TIMEOUT;

        if plateau_reached || timed_out {
            let elapsed = now
                .saturating_duration_since(state.started_at)
                .as_secs_f32();
            let stats = build_stats(state, elapsed);
            state.finalised = Some(stats.clone());
            return Some(stats);
        }
        None
    }

    /// Latest finalised stats for `pid`, or `None` if cold-load is
    /// still in progress / never started.
    pub fn stats(&self, pid: u32) -> Option<ColdStartStats> {
        self.by_pid.get(&pid).and_then(|s| s.finalised.clone())
    }

    pub fn forget(&mut self, pid: u32) {
        self.by_pid.remove(&pid);
    }
}

fn build_stats(state: &State, elapsed_seconds: f32) -> ColdStartStats {
    let avg_throughput_mbps = if elapsed_seconds > 0.0 {
        (state.bytes_read as f32 / 1.0e6) / elapsed_seconds
    } else {
        0.0
    };
    let peak_throughput_mbps = state.peak_throughput_bps / 1.0e6;
    ColdStartStats {
        duration_seconds: elapsed_seconds,
        bytes_read: state.bytes_read,
        avg_throughput_mbps,
        peak_throughput_mbps,
    }
}

/// Linux-only: read `/proc/<pid>/io` and return the `read_bytes`
/// field. `None` on permission denied / nonexistent PID — both are
/// expected and not error-worthy.
pub fn read_bytes_for(pid: u32) -> Option<u64> {
    let path = format!("/proc/{}/io", pid);
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("read_bytes:")
            && let Ok(n) = rest.trim().parse::<u64>()
        {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    /// One enormous burst → small reads → plateau detection fires.
    /// We use `record` directly with synthetic byte counts; no real
    /// /proc reads.
    #[test]
    fn detects_load_then_plateau() {
        let mut t = ColdLoadTracker::new();
        // Tick 0: baseline (no Δ window yet).
        assert!(t.record(1, 0).is_none());
        // Tick 1: 32 MiB read in ~10ms — burst phase. Not plateaued.
        sleep(Duration::from_millis(10));
        assert!(t.record(1, 32 * 1024 * 1024).is_none());
        // Tick 2: identical read_bytes → 0 throughput → plateau tick 1.
        sleep(Duration::from_millis(10));
        assert!(t.record(1, 32 * 1024 * 1024).is_none());
        // Tick 3: still flat → 2 plateau ticks → fire.
        sleep(Duration::from_millis(10));
        let stats = t
            .record(1, 32 * 1024 * 1024)
            .expect("plateau should be detected");
        assert!(stats.bytes_read >= 32 * 1024 * 1024);
        assert!(stats.peak_throughput_mbps > 100.0); // 32 MiB / 10ms ≈ 3200 MB/s
        // Subsequent calls return None — the result is sticky.
        assert!(t.record(1, 32 * 1024 * 1024).is_none());
    }

    /// Below the MIN_BYTES_FOR_LOAD floor we never declare complete.
    #[test]
    fn small_reads_never_finalise() {
        let mut t = ColdLoadTracker::new();
        assert!(t.record(1, 0).is_none());
        sleep(Duration::from_millis(10));
        assert!(t.record(1, 1024).is_none()); // 1 KB read
        sleep(Duration::from_millis(10));
        assert!(t.record(1, 1024).is_none()); // flat, but tiny
        sleep(Duration::from_millis(10));
        // Even after PLATEAU_TICKS of flat behaviour, total bytes
        // never reached the floor → no finalisation.
        assert!(t.record(1, 1024).is_none());
        assert!(t.stats(1).is_none());
    }

    /// `forget` resets the slot so a recycled PID starts fresh.
    #[test]
    fn forget_resets_state() {
        let mut t = ColdLoadTracker::new();
        t.record(1, 0);
        t.record(1, 100);
        t.forget(1);
        assert!(t.stats(1).is_none());
    }

    /// Cross-PID isolation.
    #[test]
    fn separate_pids_do_not_share_state() {
        let mut t = ColdLoadTracker::new();
        t.record(1, 0);
        sleep(Duration::from_millis(10));
        t.record(1, 64 * 1024 * 1024);
        // PID 2 starts late.
        t.record(2, 0);
        sleep(Duration::from_millis(10));
        t.record(2, 100);
        // PID 1's growing total shouldn't leak into PID 2's window.
        assert_eq!(t.by_pid.get(&2).unwrap().bytes_read, 100);
    }
}
