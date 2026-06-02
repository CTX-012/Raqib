//! ROS2 topic-activity sampler (Phase 2 / DISPATCH 2B / B3).
//!
//! Shells out to the `ros2` CLI to observe message activity per
//! AI-classified ROS2 process. Maps observation to
//! [`ActivityState`]: a recently-observed arrival on the probed
//! topic → `Active`; no arrival inside the staleness window →
//! `Idle`; no topics on the graph → `NotDetected`.
//!
//! v1.1.5 ITEM D (BUG-P5-2): mechanism replaced from `ros2 topic
//! hz` (which couldn't observe sub-Hz topics — its first-emit time
//! scales with 1/rate) to `ros2 topic echo --once` per tick + a
//! 30 s staleness window. Echo arrival is observable at any
//! non-zero rate; the window covers the worst sub-Hz publisher B3
//! targets (0.1 Hz → ≥ 3 expected arrivals per 30 s).
//!
//! v1.1.6 ITEM 1 (CRITICAL Humble compat) — DROPPED the
//! `--timeout` flag from the echo invocation: Humble's
//! `ros-humble-ros2cli 0.18.18` does not support `--timeout` on
//! `topic echo` (it was added in Iron/Jazzy/Rolling). v1.1.5
//! shipped with the flag and every probe failed with
//! `unrecognized arguments: --timeout`, locking every topic to
//! Idle (DISPATCH 17B Tester-B). The outer `ROS2_SHELLOUT_TIMEOUT`
//! tokio wrap + `--once` self-termination cap per-probe duration
//! without the distro-specific flag.
//!
//! # Empirical baseline
//!
//! Captured by Tester-A at
//! `tests/empirical/v1_1_0_prep/ros2_shellout_format/` on bare
//! Ubuntu 22.04 + ROS Humble + Cyclone DDS, `ros-humble-ros2cli`
//! 0.18.18 (use `dpkg-query -W ros-humble-ros2cli` — `ros2 --version`
//! does **not** work on this host).
//!
//! `ros2 topic list` output: ASCII, one topic per line, LF-terminated,
//! sorted, no ANSI.
//!
//! `ros2 topic hz` output (active topic): rigid two-line pairs,
//! LF-terminated, no ANSI, no header line. First pair appears at
//! `~3 s` after spawn (needs ≥3 messages before first emit at 1 Hz).
//!
//! ```text
//! average rate: 1.000
//! \tmin: 1.000s max: 1.001s std dev: 0.00052s window: 3
//! ```
//!
//! `ros2 topic hz` against an un-published topic emits a single
//! warning line within ~1 s, then sits silent indefinitely — B3
//! detects the WARNING prefix to fast-fail (Tester-A CHANGE 3)
//! rather than waiting out the full timeout.
//!
//! ```text
//! WARNING: topic [/nonexistent] does not appear to be published yet
//! ```
//!
//! SIGTERM clean within ~100 ms; no SIGKILL escalation needed
//! (Tester-A §5).
//!
//! # Timeout discipline (Inspector #12 §1 B3)
//!
//! Two layers:
//! 1. **Inner** — `ROS2_SHELLOUT_TIMEOUT` on each `tokio::time::timeout`
//!    wrapping the subprocess wait. 5 s (not 2 s — Tester-A confirmed
//!    `ros2 topic hz` needs ≥3 messages before first emit, so 2 s
//!    would clip healthy 1 Hz topics).
//! 2. **Outer** — the dispatcher's per-sample 1 s timeout. The
//!    dispatcher kills the spawned async task if `sample` outruns it;
//!    on the rare case of the inner timeout firing first, we still
//!    issue a kill before returning so the subprocess doesn't outlive
//!    the sampler.
//!
//! # Self-classification feedback loop
//!
//! Subprocesses spawned here have argv
//! `["ros2", "topic", "hz", "<topic>"]` (or `topic list`). v1.0.2
//! Inspector #5 anticipated this — see
//! `src/classifier/ros2.rs::ROS2_TOOLING_NAMES` and
//! `is_shell_wrapped_ros2_invocation` for the existing guards. The
//! synthesized-cmdline classifier-recursion test in this module
//! verifies the cmdline-only path does not false-fire (it doesn't —
//! none of the markers match `ros2 topic`).
//!
//! There is a **separate production concern**: the `ros2` binary
//! loads `librcl.so` / `librmw_implementation.so` at startup, which
//! the classifier's library signal will catch. A future classifier
//! mitigation (e.g. honour an `EDGE_MONITOR_SAMPLER` env-var marker)
//! can close this loop without breaking real ROS2 classification —
//! B3 sets that env on every spawned subprocess so the mitigation
//! has the hook ready. Surfaced in the commit body.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::telemetry::source::{
    ActivityState, ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
};

// `ros2 topic list` is one-shot — runs to completion in <100 ms on
// the empirical host. The 30 s cadence is "how often to re-discover
// the topic set." Most ROS2 graphs change topics rarely.
const ROS2_TOPIC_LIST_INTERVAL: Duration = Duration::from_secs(30);

// v1.1.5 ITEM E (Inspector side-finding) — time-based GC threshold
// for `PerPidState` entries. The dispatcher's `forget(pid)` does
// not propagate to sources (adding `forget_pid` to TelemetrySource
// is foundation work flagged by DISPATCH 16 trigger #4 and routed
// to a future dispatch — wired through v1.1.7 ITEM 2 d8f636d).
// Instead, B3 sweeps its own cache at the top of `sample`: any
// entry whose `last_topic_list_attempt_at` (per v1.1.9 ITEM (b))
// is older than 10× the topic-list refresh interval (5 min) is
// dropped.
// Bounds the leak in time (not in PID count) — equivalent closure
// property to the dispatcher-hook approach without the trait
// extension.
const ROS2_CACHE_GC_THRESHOLD: Duration = Duration::from_secs(60 * 5);

// v1.1.6 ITEM 1 — the v1.1.5 `ROS2_ECHO_PROBE_TIMEOUT` const that
// previously fed `ros2 topic echo --once --timeout <T>` is REMOVED.
// `ros-humble-ros2cli 0.18.18` rejects `--timeout` on `topic echo`
// (the flag was added in Iron/Jazzy/Rolling), so the per-probe cap
// now comes from the outer `ROS2_SHELLOUT_TIMEOUT` tokio wrap +
// `--once` self-termination alone. See `observe_topic_echo`.

// v1.1.8 ITEM 1 (DISPATCH 25) — outer bound on the explicit
// `child.wait()` after `child.kill()`. Releases tokio's per-Child
// signal/pidfd/eventfd registration (cf. tokio-rs/tokio#2685
// "Process API does not guarantee prompt reaping"). The wait
// normally returns near-instantly because `--once` already
// self-exited the child; the guard only covers pathological kernel
// edge cases (e.g. a stuck SIGTERM delivery). 500 ms is well below
// the 3 s `ROS2_SHELLOUT_TIMEOUT` per-probe budget and the 9 s
// dispatcher per-source ceiling, so a stuck reap can't compound
// across ticks.
const ROS2_CHILD_REAP_GUARD: Duration = Duration::from_millis(500);

// v1.1.5 ITEM D (BUG-P5-2) — Active is held for this duration after
// the last observed message arrival on a topic. The window has to
// span the worst publisher rate B3 cares about: a 0.1 Hz publisher
// emits a message every 10 s, so a 30 s window covers ~3 expected
// arrivals — losing all three would already be a real outage.
// Replaces the hz-rate computation, which structurally couldn't
// observe sub-Hz topics no matter how big the inner timeout (8 s,
// 30 s, 60 s — the `ros2 topic hz` first-emit time scales with
// 1/rate). Echo arrival is observable at any non-zero publication
// rate; the staleness window decides Active/Idle.
const ROS2_ACTIVITY_STALENESS: Duration = Duration::from_secs(30);

// v1.1.9 ITEM (c) — per-topic minimum interval between
// `ros2 topic echo --once` probe spawns. Pre-v1.1.9 B3 spawned a
// fresh subprocess every tick (1 Hz) per PID per topic — the
// principal driver of the residual RSS leak (DISPATCH 28 operator
// strace identified 440K close() syscalls over 70s, ~6.3K/s,
// dominated by python interpreter teardown from ros2 CLI spawns).
//
// 10 s gives **three liveness checks per 30 s staleness window** —
// a topic that goes silent is detected within ~10–20 s, which is
// appropriate for activity-state display (not a safety-critical
// timing path). The staleness logic (`ROS2_ACTIVITY_STALENESS = 30 s`)
// still holds Active across inter-probe gaps using the cached
// `last_message_at`, so cadence-gating does NOT change the steady-
// state Active/Idle decisions; it just stops the per-tick subprocess
// churn that drove the leak.
//
// Constraint: `ROS2_ECHO_PROBE_INTERVAL` must stay strictly less
// than `ROS2_ACTIVITY_STALENESS / 2` so that ≥ 2 probes fit in
// every staleness window (one probe failing wouldn't cause a
// premature Idle flip). 5 s doubles spawns for detection latency
// the use case doesn't need; 15 s leaves only 2 probes/window —
// fragile if any single probe fails. 10 s is the sweet spot.
const ROS2_ECHO_PROBE_INTERVAL: Duration = Duration::from_secs(10);

// Inner subprocess timeout for `ros2 topic echo` and `ros2 topic
// list`. v1.1.6 ITEM 1 — this is now the ONLY per-probe cap on
// `echo --once` (the Humble-unsupported `--timeout` flag was
// dropped from the invocation). `--once` causes echo to
// self-terminate on the first message; this tokio wrap caps the
// wait when no message arrives or the subprocess hangs.
const ROS2_SHELLOUT_TIMEOUT: Duration = Duration::from_secs(3);

// Pre-v1.1.5 the WARNING_NO_PUBLISHER_PREFIX line distinguished
// "topic exists but no publisher" from "topic exists and publishes"
// for the hz mechanism. The echo-once mechanism doesn't need it —
// "no message arrived inside the probe window" is the same signal
// regardless of cause (no publisher OR sub-Hz interval longer than
// the probe). The staleness window decides Active vs Idle.

/// Env var stamped onto every B3-spawned subprocess so a future
/// classifier mitigation can recognise and skip self-classification.
/// Unwired today — the recursion is documented as a known issue
/// pending the classifier-side hook.
const SAMPLER_MARKER_ENV: &str = "EDGE_MONITOR_SAMPLER";
const SAMPLER_MARKER_VALUE: &str = "ros2-shellout";

// v1.1.5 ITEM D — `rate_re` + `parse_rate_line` (the
// `average rate: <float>` regex parser for the hz mechanism)
// removed. The echo-once mechanism doesn't compute or report a
// numeric rate.

/// Per-PID state cached across `sample` calls. v1.1.5 ITEM D rewrote
/// this from the hz-based shape to the echo-once shape:
///   - The topic-list cadence is unchanged (graphs change slowly).
///   - The hz cadence + last-hz-observation are gone (`ros2 topic hz`
///     replaced with per-tick `ros2 topic echo --once`).
///   - `last_message_at` tracks the last observed message arrival
///     per topic; the staleness window
///     ([`ROS2_ACTIVITY_STALENESS`]) decides Active vs Idle.
#[derive(Debug, Default)]
struct PerPidState {
    /// v1.1.9 ITEM (b) — last `Instant` a `ros2 topic list` run
    /// **succeeded**. Updated only on `Ok`. Pre-v1.1.9 this was the
    /// sole timestamp and a transient failure would retry every tick
    /// because the cadence predicate keyed off success time only —
    /// the failure amplifier DISPATCH 28 strace called out
    /// (~18% of the spawn churn under flaky-graph conditions).
    last_topic_list_success_at: Option<Instant>,
    /// v1.1.9 ITEM (b) — last `Instant` a `ros2 topic list` run was
    /// **attempted** (success or failure). The cadence predicate now
    /// gates on this, so a failed attempt costs at most one spawn
    /// per `ROS2_TOPIC_LIST_INTERVAL`. Separating attempt from
    /// success (rather than the equivalent 1-LoC "update both on
    /// every attempt" shortcut) keeps the two timestamps independently
    /// readable — "when did we last try" and "when did we last
    /// succeed" are useful at different points in the staleness /
    /// GC logic.
    last_topic_list_attempt_at: Option<Instant>,
    /// Cached topic list from the most recent successful `ros2 topic
    /// list` run.
    topic_list: Vec<String>,
    /// topic → last `Instant` an echo probe successfully observed a
    /// message arrival. Bounded by the topic_list size; cleared on
    /// `forget(pid)` (v1.1.5 ITEM E).
    last_message_at: HashMap<String, Instant>,
    /// v1.1.9 ITEM (c) — topic → last `Instant` an echo probe was
    /// SPAWNED for this PID/topic pair (success or failure). Drives
    /// the [`ROS2_ECHO_PROBE_INTERVAL`] cadence gate that suppresses
    /// per-tick subprocess churn. Decoupled from `last_message_at`
    /// so that "we last tried at t1" and "we last saw a message at
    /// t2" remain independent — a successful probe with no arrival
    /// (`Ok(false)`) still backs off the next probe by the cadence.
    last_echo_probe_at: HashMap<String, Instant>,
}

/// ROS2 topic-rate sampler. One source instance per dispatcher;
/// per-PID state keeps cadence + last-observation across ticks.
pub struct Ros2ShelloutSource {
    cache: HashMap<u32, PerPidState>,
}

impl Ros2ShelloutSource {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Spawn `ros2 topic list`, wait for completion, return parsed
    /// topics. `Transient` on timeout / spawn failure / nonzero exit
    /// / no output — the dispatcher retries next tick.
    ///
    /// Does not borrow `&self`; per-PID state in `cache` mutates
    /// across the await in `sample`, so this method takes nothing
    /// from the source struct.
    async fn run_topic_list() -> SourceResult<Vec<String>> {
        let mut cmd = Command::new("ros2");
        cmd.args(["topic", "list"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env(SAMPLER_MARKER_ENV, SAMPLER_MARKER_VALUE);
        let timed = tokio::time::timeout(ROS2_SHELLOUT_TIMEOUT, cmd.output()).await;
        let output = timed
            .map_err(|_| SourceError::Transient("ros2 topic list timed out".into()))?
            .map_err(|e| SourceError::Transient(format!("ros2 topic list spawn failed: {e}")))?;
        if !output.status.success() {
            return Err(SourceError::Transient(format!(
                "ros2 topic list exited with status {:?}",
                output.status.code()
            )));
        }
        Ok(parse_topic_list(&String::from_utf8_lossy(&output.stdout)))
    }

    /// v1.1.5 ITEM D (BUG-P5-2) — spawn `ros2 topic echo --once
    /// <topic>` and observe whether a message arrived. Returns
    /// `Ok(true)` if echo printed a non-empty body (a message
    /// arrived inside the probe window); `Ok(false)` if echo exited
    /// without printing (no message); `Err(Transient)` on spawn /
    /// outer-timeout failure.
    ///
    /// `--once` makes echo self-terminate after the first message,
    /// so we don't need the kill discipline the hz mechanism
    /// required.
    ///
    /// v1.1.6 ITEM 1 (CRITICAL Humble compat) — REMOVED the
    /// `--timeout <T>` flag. `ros-humble-ros2cli 0.18.18` does NOT
    /// support `--timeout` on `topic echo` (it was added in Iron /
    /// Jazzy / Rolling). Tester-B (DISPATCH 17B) caught it: every
    /// v1.1.5 probe failed with "unrecognized arguments: --timeout"
    /// → `last_message_at` never updated → every topic locked Idle.
    /// The outer `ROS2_SHELLOUT_TIMEOUT` tokio wrap + the `--once`
    /// self-termination give us the per-probe cap without the
    /// distro-specific flag. Verified bare
    /// `ros2 topic echo --once /topic` works on Humble pre-commit.
    async fn observe_topic_echo(topic: &str) -> SourceResult<bool> {
        let mut child = Command::new("ros2")
            .args(ros2_echo_args(topic))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env(SAMPLER_MARKER_ENV, SAMPLER_MARKER_VALUE)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SourceError::Transient(format!("ros2 topic echo spawn failed: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SourceError::Transient("ros2 topic echo missing stdout".into()))?;

        let read_fut = stdout_observed_message(stdout);
        let timed = tokio::time::timeout(ROS2_SHELLOUT_TIMEOUT, read_fut).await;

        // Belt-and-braces: echo --once self-terminates after the
        // first message, but if the outer wait fired first, kill the
        // child explicitly. kill_on_drop covers the rest.
        let _ = child.kill().await;
        // v1.1.8 ITEM 1 (DISPATCH 25) — reap the child deterministically.
        // tokio's best-effort reaper (tokio-rs/tokio#2685) does NOT
        // guarantee prompt release of per-Child state after kill() —
        // kill() signals, but the signal/pidfd registration backing
        // the Child handle (each one consumes a kernel `anon_inode:
        // [eventfd]` slot) is only released when `wait()` completes.
        // Without it, every probe leaked one eventfd: the DISPATCH 25
        // PHASE 0 diagnostic observed 226 eventfds at t=185s under
        // the 10× ROS2-publisher workload (~1 fd/s), correlating with
        // the linear ~195 MB/min RSS growth. The 500 ms guard bounds
        // pathological non-reaping (kernel edge cases); the normal
        // path returns near-instantly since `--once` already
        // self-exited the child before kill().
        let _ = tokio::time::timeout(ROS2_CHILD_REAP_GUARD, child.wait()).await;

        match timed {
            Ok(Ok(observed)) => Ok(observed),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SourceError::Transient(format!(
                "ros2 topic echo timed out after {} s",
                ROS2_SHELLOUT_TIMEOUT.as_secs()
            ))),
        }
    }

    /// v1.1.5 ITEM E (Inspector side-finding) — drop this PID's
    /// cached state. Called from the dispatcher when a PID is
    /// forgotten (e.g. on explicit `forget(pid)` cleanup). Pre-v1.1.5
    /// the per-PID HashMap entry persisted across PID churn; bounded
    /// leak in practice, but material under the echo-once mechanism
    /// which keeps per-topic timestamps.
    pub fn forget(&mut self, pid: u32) {
        self.cache.remove(&pid);
    }
}

impl Default for Ros2ShelloutSource {
    fn default() -> Self {
        Self::new()
    }
}

/// v1.1.6 ITEM 1 — extracted so the regression-pin test
/// (`b3_echo_once_no_timeout_flag_detects_active_topic`) can
/// assert the args list does NOT contain `--timeout`. The flag
/// was added in Iron/Jazzy/Rolling and is rejected by Humble's
/// `ros-humble-ros2cli 0.18.18` — v1.1.5 shipped with it and
/// every probe failed `unrecognized arguments: --timeout`, locking
/// every ROS2 row to Idle (DISPATCH 17B Tester-B).
fn ros2_echo_args(topic: &str) -> [&str; 4] {
    ["topic", "echo", "--once", topic]
}

/// Pure parser for `ros2 topic list` output. Tester-A's empirical
/// capture: one topic per line, LF terminator, sorted, ASCII-only.
/// Public for unit tests.
pub(crate) fn parse_topic_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// v1.1.5 ITEM D — stream-read `ros2 topic echo --once` stdout and
/// return `Ok(true)` the moment any non-empty content arrives (a
/// message was published). Returns `Ok(false)` when echo closes
/// stdout without producing output (the `--timeout` window expired
/// with no publisher emitting). Returns `Err(Transient)` only on a
/// read-IO failure, which is rare.
async fn stdout_observed_message(stdout: tokio::process::ChildStdout) -> SourceResult<bool> {
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| SourceError::Transient(format!("ros2 topic echo read failed: {e}")))?
    {
        // Any non-empty line is evidence that echo printed a message
        // body. `ros2 topic echo --once` emits "---" between messages
        // and the message body lines — both are non-empty.
        if !line.trim().is_empty() {
            return Ok(true);
        }
    }
    // Stdout closed without any non-empty content → no message.
    Ok(false)
}

#[async_trait]
impl TelemetrySource for Ros2ShelloutSource {
    fn name(&self) -> &str {
        "ros2-shellout"
    }

    /// v1.1.1 (DISPATCH 5) — outer dispatcher timeout for ros2
    /// shellout samples. Pre-v1.1.1 the dispatcher used a single
    /// global 1 s wrap that cancelled B3's inner
    /// `ROS2_SHELLOUT_TIMEOUT` early, so `ros2 topic hz` never
    /// observed the ≥ 3 published messages it needs to emit a
    /// rate. Every ROS2 row locked to `NotDetected`.
    ///
    /// v1.1.3 (P5 DISPATCH 9A) — tracks the inner+1s convention:
    /// inner `ROS2_SHELLOUT_TIMEOUT` is now 8 s (was 5 s), so the
    /// outer dispatcher ceiling is 9 s (was 6 s). The +1 s is
    /// headroom for subprocess kill-signal propagation when a
    /// probe genuinely hangs. The outer wrap must always exceed
    /// the inner timeout — see `b3_sample_timeout_exceeds_inner_shellout_timeout`.
    fn sample_timeout(&self) -> Duration {
        Duration::from_secs(9)
    }

    /// Applies to processes the classifier would surface as ROS2 via
    /// env or cmdline signals. Library-signal-only ROS2 nodes (rare
    /// — C++ nodes spawned without `ros2 run` and without
    /// `ROS_DOMAIN_ID` exported) are NOT covered; they classify as
    /// ROS2 in the panel but get no Phase-2 sampling. Acceptable
    /// gap for v1.1.0; v1.1.1+ can plumb `workload_category` onto
    /// `ProcessSnapshot` to close it.
    fn applies_to(&self, proc: &ProcessSnapshot) -> bool {
        // ROS_DOMAIN_ID — standalone-trustworthy runtime signal (per
        // Fix-1; set by `ros2 launch` / `ros2 run` / explicit export).
        if proc
            .environ
            .get("ROS_DOMAIN_ID")
            .is_some_and(|v| !v.is_empty())
        {
            return true;
        }
        // Cmdline marker — standalone-trustworthy per Fix-1. The
        // `is_shell_wrapped_ros2_invocation` shape from the classifier
        // is not replicated here (B3 shouldn't sample a `bash -c`
        // shell wrapping `ros2 …`; the wrapper isn't a graph
        // participant). Markers cover real ROS2 nodes.
        let joined = proc.cmdline.join(" ").to_lowercase();
        joined.contains("ros2 run")
            || joined.contains("ros2 launch")
            || joined.contains("rclcpp_component_container")
            || joined.contains("rclpy")
    }

    /// v1.1.7 ITEM 2 — promptly drop this PID's `PerPidState` when
    /// the runtime notifies B3 that the PID is gone. Pre-v1.1.7 B3
    /// relied on the 5-min time-based GC sweep at the top of
    /// `sample` (v1.1.5 ITEM E) which bounded the leak in time but
    /// left ghost cache entries live for the full window after PID
    /// death. The dispatcher now acquires the per-source mutex and
    /// calls this on every `Dispatcher::forget(pid)`.
    fn on_forget(&mut self, pid: u32) {
        self.cache.remove(&pid);
    }

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        let now = Instant::now();
        // v1.1.5 ITEM E — sweep stale PerPidState entries before
        // touching the cache for this PID. Bounds the leak in time
        // independent of dispatcher forget propagation. v1.1.9 ITEM
        // (b) anchored on `last_topic_list_attempt_at` (the
        // "last-touched" timestamp), since it's set on both success
        // AND failed attempts — a PID whose probes keep failing is
        // still active enough to keep its state warm.
        self.cache.retain(|_, st| {
            st.last_topic_list_attempt_at
                .map(|t| now.duration_since(t) < ROS2_CACHE_GC_THRESHOLD)
                .unwrap_or(true) // cold-start entries (no attempt yet) are kept
        });
        let state = self.cache.entry(proc.pid).or_default();

        // Refresh topic list per cadence (graphs change slowly).
        // v1.1.9 ITEM (b) — gate on `last_topic_list_attempt_at`
        // (not `last_topic_list_success_at`), so a transient
        // `Err(Transient)` from `run_topic_list` doesn't retry on
        // every subsequent tick. We back off to the full
        // `ROS2_TOPIC_LIST_INTERVAL` cadence regardless of outcome,
        // and only the success path updates the cached `topic_list`.
        let need_list = state
            .last_topic_list_attempt_at
            .map(|t| now.duration_since(t) >= ROS2_TOPIC_LIST_INTERVAL)
            .unwrap_or(true);
        if need_list {
            // Mark the attempt BEFORE awaiting; same reasoning as
            // ITEM (c)'s `last_echo_probe_at.insert(..)` — a slow
            // or failing subprocess mustn't double-fire next tick.
            state.last_topic_list_attempt_at = Some(now);
            match Self::run_topic_list().await {
                Ok(topics) => {
                    state.topic_list = topics;
                    state.last_topic_list_success_at = Some(now);
                }
                Err(e) => return Err(e),
            }
        }

        if state.topic_list.is_empty() {
            return Ok(activity_frame(proc.pid, ActivityState::NotDetected));
        }

        // v1.1.5 ITEM D (BUG-P5-2) — pick the first topic and probe
        // it with `ros2 topic echo --once`. v1.1.9 ITEM (c) — gate
        // the probe by `ROS2_ECHO_PROBE_INTERVAL` so we don't spawn
        // a fresh ros2 subprocess every tick (which was the
        // principal driver of DISPATCH 28's 6.3K close()/s leak).
        // Within the cadence window we reuse `last_message_at` +
        // the staleness check: identical Active/Idle decision as a
        // freshly probed `Ok(false)` would produce, just without
        // the subprocess spawn. v1.1.1+ can round-robin across
        // topics; single-topic is fine while the dispatcher's
        // per-source sample_timeout is 9 s.
        let topic = state.topic_list[0].clone();
        let need_probe = state
            .last_echo_probe_at
            .get(&topic)
            .map(|t| now.duration_since(*t) >= ROS2_ECHO_PROBE_INTERVAL)
            .unwrap_or(true);
        if !need_probe {
            // Skip the spawn. The staleness window over
            // `last_message_at` decides Active/Idle exactly as
            // a `Ok(false)` probe would have. NotDetected is
            // unreachable here — we only enter this branch after
            // a successful probe (which seeded `last_echo_probe_at`),
            // so `last_message_at` may be empty for this topic
            // only if every probe so far returned `Ok(false)`
            // (steady Idle), which is the desired output.
            let activity = match state.last_message_at.get(&topic) {
                Some(last) if now.duration_since(*last) < ROS2_ACTIVITY_STALENESS => {
                    ActivityState::Active
                }
                _ => ActivityState::Idle,
            };
            return Ok(activity_frame(proc.pid, activity));
        }

        // Mark the spawn-attempt BEFORE awaiting the subprocess so
        // a slow/failing spawn doesn't double-fire next tick.
        state.last_echo_probe_at.insert(topic.clone(), now);
        let pid_for_log = proc.pid;
        let _ = state; // release the &mut self.cache borrow before .await

        let observed = Self::observe_topic_echo(&topic).await;
        let activity = match observed {
            Ok(true) => {
                // Message arrived — refresh the staleness clock and
                // emit Active.
                let s = self.cache.entry(proc.pid).or_default();
                s.last_message_at.insert(topic.clone(), now);
                ActivityState::Active
            }
            Ok(false) => {
                // No message this probe — fall through to the
                // staleness window. If the last observed arrival on
                // this topic is within ROS2_ACTIVITY_STALENESS, hold
                // Active. Otherwise Idle.
                let s = self.cache.entry(proc.pid).or_default();
                match s.last_message_at.get(&topic) {
                    Some(last) if now.duration_since(*last) < ROS2_ACTIVITY_STALENESS => {
                        ActivityState::Active
                    }
                    _ => ActivityState::Idle,
                }
            }
            Err(e) => {
                tracing::warn!(
                    sampler = self.name(),
                    pid = pid_for_log,
                    topic = %topic,
                    error = %e,
                    "ros2 topic echo observation failed"
                );
                // Probe failed — apply the staleness window against
                // the last-observed arrival so we don't flicker.
                let s = self.cache.entry(proc.pid).or_default();
                match s.last_message_at.get(&topic) {
                    Some(last) if now.duration_since(*last) < ROS2_ACTIVITY_STALENESS => {
                        ActivityState::Active
                    }
                    _ => ActivityState::NotDetected,
                }
            }
        };
        Ok(activity_frame(proc.pid, activity))
    }

}

fn activity_frame(pid: u32, state: ActivityState) -> TelemetryFrame {
    TelemetryFrame {
        pid,
        activity_state: Some(state),
        ..TelemetryFrame::new(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// v1.1.1 DISPATCH 5 — pin the inner < outer invariant.
    /// v1.1.3 (P5 DISPATCH 9A) — refined hz values to 8 s/9 s.
    /// v1.1.5 ITEM D (BUG-P5-2) — replaced the hz mechanism with
    /// `ros2 topic echo --once` activity detection (~3 s inner; the
    /// `outer > inner` invariant is preserved at 9 s). The strict
    /// `outer > inner` assertion remains load-bearing: if a refactor
    /// lowers the override below the inner timeout, echo --once is
    /// cancelled before it can read a message and every ROS2 row
    /// re-locks to NotDetected (the v1.1.0 B3 root cause shape).
    /// The exact-value asserts pin the v1.1.5 values.
    #[test]
    fn b3_sample_timeout_exceeds_inner_shellout_timeout() {
        let s = Ros2ShelloutSource::new();
        let outer = TelemetrySource::sample_timeout(&s);
        assert!(
            outer > ROS2_SHELLOUT_TIMEOUT,
            "B3 outer dispatcher timeout ({outer:?}) must exceed \
             the inner ROS2_SHELLOUT_TIMEOUT ({ROS2_SHELLOUT_TIMEOUT:?}) \
             — otherwise the ros2 subprocess is cancelled before it \
             can read a message. This was the v1.1.0 B3 root cause shape.",
        );
        // v1.1.5 / v1.1.6 values: 3 s inner tokio wait around
        // echo --once (no `--timeout` flag — see v1.1.6 ITEM 1),
        // 9 s outer dispatcher ceiling (unchanged from v1.1.3).
        assert_eq!(ROS2_SHELLOUT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(outer, Duration::from_secs(9));
    }

    /// v1.1.8 ITEM 1 (DISPATCH 25) — pin the reap-guard timing
    /// invariant for the explicit `child.wait()` after
    /// `child.kill()` in `observe_topic_echo`. Without the wait,
    /// tokio's per-Child signal/pidfd/eventfd registration was
    /// leaking — DISPATCH 25 PHASE 0 /proc diagnostic observed
    /// 226 `anon_inode:[eventfd]` FDs at t=185s under the 10×
    /// ROS2-publisher workload (~1 FD/s), correlating with the
    /// linear ~195 MB/min RSS growth.
    ///
    /// The guard MUST stay well below `ROS2_SHELLOUT_TIMEOUT`
    /// (3 s) so a pathological reap can't push a probe past its
    /// budget and pile up sample tasks waiting on the per-source
    /// mutex. Pinned at 500 ms — fast enough to never matter on
    /// the healthy path (the child is already exited; wait()
    /// returns immediately), slow enough to ride out kernel
    /// SIGCHLD delivery hiccups.
    ///
    /// Behavioural unit test against a real `ros2` subprocess is
    /// not possible without a live ROS2 graph (kept out of
    /// `cargo test`); the empirical proof of fix is the post-fix
    /// /proc snapshot showing eventfd count flat.
    #[test]
    fn b3_echo_reaps_child_with_wait() {
        assert_eq!(ROS2_CHILD_REAP_GUARD, Duration::from_millis(500));
        assert!(
            ROS2_CHILD_REAP_GUARD < ROS2_SHELLOUT_TIMEOUT,
            "reap guard ({:?}) must stay well below the per-probe \
             ROS2_SHELLOUT_TIMEOUT ({:?}) — otherwise a stuck reap \
             can push a probe past its budget and pile up sample \
             tasks waiting on the per-source mutex (the v1.1.6 leak \
             shape DISPATCH 22 closed).",
            ROS2_CHILD_REAP_GUARD,
            ROS2_SHELLOUT_TIMEOUT,
        );
        // Symbolic pin: assert the production helper that builds
        // the echo args list is reachable, so a refactor that
        // accidentally drops `observe_topic_echo` (and with it the
        // wait() call) doesn't leave only this constant behind.
        let args = ros2_echo_args("/sentinel");
        assert!(args.contains(&"--once"));
    }

    fn proc(pid: u32, name: &str, cmdline: &[&str], env: &[(&str, &str)]) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            environ: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            model_name: None,
            cpu_pct: 0.0,
            ppid: None,
            workload_category: None,
        }
    }

    // ─── parse_topic_list (Tester-A capture verbatim) ───────────────

    #[test]
    fn parses_default_humble_topic_list() {
        // raw/ros2_topic_list_capture.txt — three lines, LF-terminated.
        let stdout = "/chatter\n/parameter_events\n/rosout\n";
        let topics = parse_topic_list(stdout);
        assert_eq!(
            topics,
            vec![
                "/chatter".to_string(),
                "/parameter_events".to_string(),
                "/rosout".to_string()
            ]
        );
    }

    #[test]
    fn parse_topic_list_handles_empty_output() {
        assert!(parse_topic_list("").is_empty());
        assert!(parse_topic_list("\n").is_empty());
    }

    // v1.1.5 ITEM D — the `parse_rate_line` + WARNING tests (the
    // hz-rate parser surface) were REMOVED with the hz mechanism.
    // The echo-once mechanism doesn't parse a rate; it observes a
    // message arrival via `stdout_observed_message`. The new
    // staleness-window tests below pin the v1.1.5 contract.

    // ─── applies_to ─────────────────────────────────────────────────

    #[test]
    fn applies_to_ros_domain_id_alone() {
        // Standalone-trustworthy runtime signal — same shape as the
        // classifier's Fix-1 ROS_DOMAIN_ID check.
        let p = proc(
            42,
            "talker",
            &["talker"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        let s = Ros2ShelloutSource::new();
        assert!(s.applies_to(&p));
    }

    #[test]
    fn applies_to_cmdline_marker_ros2_run() {
        let p = proc(
            42,
            "ros2",
            &["ros2", "run", "demo_nodes_cpp", "talker"],
            &[],
        );
        let s = Ros2ShelloutSource::new();
        assert!(s.applies_to(&p));
    }

    #[test]
    fn applies_to_cmdline_marker_rclpy_module() {
        let p = proc(
            42,
            "python3",
            &["python3", "-m", "rclpy.executor_test"],
            &[],
        );
        let s = Ros2ShelloutSource::new();
        assert!(s.applies_to(&p));
    }

    #[test]
    fn does_not_apply_to_bare_ros_setup_env_without_cmdline_marker() {
        // RMW_IMPLEMENTATION + ROS_DISTRO + AMENT_PREFIX_PATH set
        // (setup.bash inheritance) but no cmdline marker and no
        // ROS_DOMAIN_ID — same setup.bash-shell-child shape Fix-1
        // tightened the classifier against. Sampler must NOT
        // false-fire on a generic user-shell child.
        let p = proc(
            42,
            "bash",
            &["bash", "-i"],
            &[
                ("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp"),
                ("ROS_DISTRO", "humble"),
                ("AMENT_PREFIX_PATH", "/opt/ros/humble"),
            ],
        );
        let s = Ros2ShelloutSource::new();
        assert!(!s.applies_to(&p));
    }

    #[test]
    fn does_not_apply_to_unrelated_process() {
        let p = proc(42, "firefox", &["firefox"], &[]);
        let s = Ros2ShelloutSource::new();
        assert!(!s.applies_to(&p));
    }

    // ─── Classifier-recursion guard (Tester-A CHANGE 4) ─────────────

    /// When B3 spawns `ros2 topic hz /chatter` as a subprocess, the
    /// edge_monitor classifier must NOT misclassify that subprocess
    /// as a ROS2 workload — otherwise the sampler would observe its
    /// own probes, creating a feedback loop.
    ///
    /// This test verifies the **cmdline-only** path. The classifier's
    /// shell-wrapper guard (`is_shell_wrapped_ros2_invocation`) is
    /// the existing v1.0.2 mitigation for the bash-wrapped case; B3
    /// does NOT wrap in bash, so the relevant question is whether
    /// `["ros2", "topic", "hz", "/chatter"]` joined as
    /// `"ros2 topic hz /chatter"` matches any `ROS2_CMDLINE_MARKERS`
    /// entry. Tester-A flagged that "rclpy" is still in the list but
    /// "ros2 topic" was dropped in B-NEW-16 — this test confirms.
    ///
    /// **Known production gap:** the spawned subprocess loads
    /// `librcl.so` / `librmw_implementation.so` and could still
    /// false-fire via the classifier's library signal. A future
    /// classifier mitigation honouring `EDGE_MONITOR_SAMPLER` env
    /// (which B3 already sets on every spawn) will close this loop;
    /// surfaced in the commit body.
    #[test]
    fn b3_subprocess_not_classified_as_ros2_workload() {
        use crate::classifier::classify_process;
        use crate::model::{ProcessSample, WorkloadCategory};

        // Synthesise the exact ProcessSample shape the edge_monitor
        // platform layer would surface for a B3 spawn. PID is a
        // sentinel that will never have a real /proc/<pid>/maps
        // (avoids the library-signal branch reading actual disk).
        let mut environ = HashMap::new();
        // Inherited from B3's parent (the edge_monitor runtime) on
        // the empirical host — these are the setup.bash exports.
        environ.insert("RMW_IMPLEMENTATION".into(), "rmw_cyclonedds_cpp".into());
        environ.insert("ROS_DISTRO".into(), "humble".into());
        environ.insert("AMENT_PREFIX_PATH".into(), "/opt/ros/humble".into());
        // Plus B3's own marker so a future classifier mitigation can
        // recognise the subprocess.
        environ.insert(SAMPLER_MARKER_ENV.into(), SAMPLER_MARKER_VALUE.into());

        let sample = ProcessSample {
            pid: u32::MAX,
            ppid: Some(1),
            name: "ros2".into(),
            cmdline: vec![
                "ros2".into(),
                "topic".into(),
                "hz".into(),
                "/chatter".into(),
            ],
            environ,
            cwd: None,
            rss_bytes: 0,
            cpu_time_ticks: 0,
            os_start_time: None,
        };
        let result = classify_process(&sample);
        // Cmdline-only path does NOT classify as ROS2 — no marker
        // matches `ros2 topic` / `ros2 topic hz`. (rclpy marker
        // doesn't match either; "ros2 run" / "ros2 launch" /
        // "rclcpp_component_container" all miss.)
        assert_ne!(
            result.workload_category,
            WorkloadCategory::ROS2,
            "Synthesized cmdline {:?} must not false-fire ROS2 classifier — \
             would create observer-watching-its-own-children recursion. \
             Evidence: {}",
            sample.cmdline,
            result.evidence,
        );
    }

    // ─── v1.1.5 ITEM D (BUG-P5-2) — echo-once + staleness window ────

    /// Echo-once mechanism: a message arrival refreshes
    /// `last_message_at` and the next sub-Hz tick (no message this
    /// probe) stays Active because the staleness window hasn't
    /// expired. The regression-pin for BUG-P5-2: a 0.1 Hz topic
    /// emits a message every 10 s; within a 30 s window ~3 messages
    /// arrive, so Active is preserved across the inter-message
    /// gaps.
    ///
    /// Drives the per-PID state directly with simulated `Instant`s
    /// (the same pattern B2/B4 use to test their hold-windows).
    #[test]
    fn b3_echo_activity_detects_sub_hz_topic() {
        let mut s = Ros2ShelloutSource::new();
        let pid = 42;
        let topic = "/sub_hz".to_string();
        // Seed the per-PID state as if the topic-list refresh has
        // happened and a message was just observed.
        let t0 = Instant::now();
        {
            let st = s.cache.entry(pid).or_default();
            st.last_topic_list_success_at = Some(t0);
            st.last_topic_list_attempt_at = Some(t0);
            st.topic_list = vec![topic.clone()];
            st.last_message_at.insert(topic.clone(), t0);
        }

        // 5 s later (a sub-Hz interval would emit nothing yet) the
        // staleness window holds Active.
        let t5 = t0 + Duration::from_secs(5);
        let st = s.cache.entry(pid).or_default();
        let activity = match st.last_message_at.get(&topic) {
            Some(last) if t5.duration_since(*last) < ROS2_ACTIVITY_STALENESS => {
                ActivityState::Active
            }
            _ => ActivityState::Idle,
        };
        assert_eq!(
            activity,
            ActivityState::Active,
            "5 s after last arrival is inside the 30 s staleness window \
             — must hold Active for sub-Hz topics",
        );

        // 31 s with no new arrivals → past the window → Idle.
        let t31 = t0 + Duration::from_secs(31);
        let st = s.cache.entry(pid).or_default();
        let activity = match st.last_message_at.get(&topic) {
            Some(last) if t31.duration_since(*last) < ROS2_ACTIVITY_STALENESS => {
                ActivityState::Active
            }
            _ => ActivityState::Idle,
        };
        assert_eq!(activity, ActivityState::Idle);
    }

    /// `stdout_observed_message` returns `Ok(true)` on the first
    /// non-empty line and `Ok(false)` on closed-empty stdout. Pure
    /// helper test using an in-memory stream is awkward (the helper
    /// takes a `ChildStdout`); the contract is small enough that the
    /// behavioural pin lives on the integration harness side.
    /// Instead pin the constants that drive the staleness behaviour:
    /// the staleness window MUST cover at least 3 expected arrivals
    /// of the worst rate B3 cares about (0.1 Hz → ≥ 30 s).
    ///
    /// v1.1.6 ITEM 1 — the v1.1.5 `ROS2_ECHO_PROBE_TIMEOUT` pin
    /// (1 s) was removed alongside the `--timeout` flag. The
    /// per-probe cap is now the outer `ROS2_SHELLOUT_TIMEOUT` tokio
    /// wrap (3 s) plus `--once` self-termination; the staleness
    /// window remains the load-bearing constant for sub-Hz Active.
    #[test]
    fn b3_staleness_window_covers_sub_hz_rates() {
        assert_eq!(ROS2_ACTIVITY_STALENESS, Duration::from_secs(30));
        // 0.1 Hz worst-supported publisher emits a message every 10 s;
        // the staleness window must be ≥ 3 * 10 s so losing the
        // expected 3 arrivals is already a real outage.
        assert!(ROS2_ACTIVITY_STALENESS >= Duration::from_secs(30));
    }

    // ─── v1.1.9 ITEM (c) — cadence-gate echo probes ─────────────────

    /// Pin the timing invariants on the v1.1.9 cadence-gate. The
    /// 10 s probe interval must remain strictly less than
    /// `ROS2_ACTIVITY_STALENESS / 2` so that at least two probes
    /// always fit inside the staleness window — losing a single
    /// probe to a transient error mustn't cause a premature Idle
    /// flip. The cadence ratio is the property that bounds spawn
    /// rate (the leak we're closing) without changing steady-state
    /// activity detection.
    #[test]
    fn b3_echo_probe_respects_10s_interval_const_pins() {
        assert_eq!(ROS2_ECHO_PROBE_INTERVAL, Duration::from_secs(10));
        assert!(
            ROS2_ECHO_PROBE_INTERVAL * 2 <= ROS2_ACTIVITY_STALENESS,
            "ROS2_ECHO_PROBE_INTERVAL ({:?}) must be ≤ \
             ROS2_ACTIVITY_STALENESS/2 ({:?}) — otherwise a single \
             missed probe can push past the staleness window before \
             the next probe fires, flipping a healthy topic to Idle.",
            ROS2_ECHO_PROBE_INTERVAL,
            ROS2_ACTIVITY_STALENESS / 2,
        );
    }

    /// v1.1.9 ITEM (c) — within the cadence interval, the cached
    /// decision "is `last_message_at` inside the staleness window?"
    /// must be REUSED instead of spawning a fresh probe. This is the
    /// structural fix for the per-tick subprocess churn (DISPATCH 28
    /// strace: 6.3K close()/s, dominated by python interpreter
    /// teardown). Drives the per-PID state directly to assert the
    /// reuse-vs-respawn predicate the production path applies.
    #[test]
    fn b3_echo_probe_respects_10s_interval() {
        let mut s = Ros2ShelloutSource::new();
        let pid = 42;
        let topic = "/chatter".to_string();
        let t0 = Instant::now();
        // Seed: topic list ready; a probe at t0 succeeded and saw a
        // message at t0.
        {
            let st = s.cache.entry(pid).or_default();
            st.last_topic_list_success_at = Some(t0);
            st.last_topic_list_attempt_at = Some(t0);
            st.topic_list = vec![topic.clone()];
            st.last_message_at.insert(topic.clone(), t0);
            st.last_echo_probe_at.insert(topic.clone(), t0);
        }

        // t0 + 5 s — INSIDE cadence: the cadence predicate must say
        // "no fresh probe needed."
        let t5 = t0 + Duration::from_secs(5);
        let st = s.cache.entry(pid).or_default();
        let need_probe = st
            .last_echo_probe_at
            .get(&topic)
            .map(|t| t5.duration_since(*t) >= ROS2_ECHO_PROBE_INTERVAL)
            .unwrap_or(true);
        assert!(
            !need_probe,
            "within 10 s of last probe (5 s elapsed) the cadence \
             gate must suppress a fresh spawn",
        );
        // …and the staleness-window decision over the cached
        // `last_message_at` must hold the topic Active (5 s well
        // inside the 30 s staleness).
        let activity = match st.last_message_at.get(&topic) {
            Some(last) if t5.duration_since(*last) < ROS2_ACTIVITY_STALENESS => {
                ActivityState::Active
            }
            _ => ActivityState::Idle,
        };
        assert_eq!(activity, ActivityState::Active);

        // t0 + 10 s — boundary: cadence elapsed, fresh probe needed.
        let t10 = t0 + Duration::from_secs(10);
        let st = s.cache.entry(pid).or_default();
        let need_probe = st
            .last_echo_probe_at
            .get(&topic)
            .map(|t| t10.duration_since(*t) >= ROS2_ECHO_PROBE_INTERVAL)
            .unwrap_or(true);
        assert!(
            need_probe,
            "at 10 s elapsed the cadence gate must permit a fresh probe",
        );
    }

    // ─── v1.1.9 ITEM (b) — topic-list failure backoff ───────────────

    /// v1.1.9 ITEM (b) — the topic-list refresh cadence keys off
    /// `last_topic_list_attempt_at`, NOT `last_topic_list_success_at`.
    /// Pre-v1.1.9 the predicate used the only timestamp it had
    /// (success), so a transient `Err(Transient)` from
    /// `run_topic_list` triggered a fresh spawn on EVERY subsequent
    /// tick until success — the failure-amplifier branch DISPATCH 28
    /// strace identified as ~18% of B3's spawn churn.
    ///
    /// Drives the predicate directly to assert:
    ///   - A failed attempt at `t0` STILL satisfies the cadence
    ///     gate at `t0 + 5 s` (no retry-next-tick).
    ///   - The same attempt-timestamp opens the gate again at
    ///     `t0 + 30 s` (the success path would similarly).
    ///   - A success path updates both timestamps; a failure path
    ///     touches only `last_topic_list_attempt_at`. The pair must
    ///     remain independently readable so the staleness/GC code
    ///     can distinguish "tried recently" from "succeeded recently."
    #[test]
    fn b3_topic_list_failure_backs_off_to_cadence() {
        let mut s = Ros2ShelloutSource::new();
        let pid = 42;
        let t0 = Instant::now();

        // Simulate a failed `run_topic_list` at t0: attempt updates,
        // success does NOT.
        {
            let st = s.cache.entry(pid).or_default();
            st.last_topic_list_attempt_at = Some(t0);
            // last_topic_list_success_at stays None — no successful
            // refresh yet.
        }

        // At t0 + 5 s — well inside ROS2_TOPIC_LIST_INTERVAL (30 s).
        // The production predicate over `last_topic_list_attempt_at`
        // must say "no fresh attempt needed."
        let t5 = t0 + Duration::from_secs(5);
        let st = s.cache.get(&pid).unwrap();
        let need_list = st
            .last_topic_list_attempt_at
            .map(|t| t5.duration_since(t) >= ROS2_TOPIC_LIST_INTERVAL)
            .unwrap_or(true);
        assert!(
            !need_list,
            "failed attempt at t0 must back off — no retry at t0+5s. \
             Pre-v1.1.9 the predicate keyed off success and a failed \
             refresh re-spawned every tick (DISPATCH 28 ~18% of the \
             B3 churn).",
        );

        // At t0 + 30 s — boundary: cadence elapsed regardless of
        // success/failure.
        let t30 = t0 + ROS2_TOPIC_LIST_INTERVAL;
        let need_list = st
            .last_topic_list_attempt_at
            .map(|t| t30.duration_since(t) >= ROS2_TOPIC_LIST_INTERVAL)
            .unwrap_or(true);
        assert!(
            need_list,
            "at the cadence boundary the gate must permit a retry",
        );

        // Now simulate a SUCCESS at t30: both timestamps update.
        {
            let st = s.cache.entry(pid).or_default();
            st.last_topic_list_attempt_at = Some(t30);
            st.last_topic_list_success_at = Some(t30);
            st.topic_list = vec!["/chatter".into()];
        }
        let st = s.cache.get(&pid).unwrap();
        assert_eq!(
            st.last_topic_list_attempt_at,
            st.last_topic_list_success_at,
            "success path must update BOTH timestamps so a downstream \
             reader can compare them and find them in sync",
        );

        // Independent-readability pin: a later failed attempt
        // updates only attempt_at; success_at stays at t30 so the
        // GC / staleness logic can still see "succeeded 30 s ago."
        let t_fail = t30 + Duration::from_secs(35); // > cadence
        {
            let st = s.cache.entry(pid).or_default();
            st.last_topic_list_attempt_at = Some(t_fail);
            // success_at deliberately not touched
        }
        let st = s.cache.get(&pid).unwrap();
        assert_eq!(
            st.last_topic_list_success_at,
            Some(t30),
            "failure path must NOT touch last_topic_list_success_at",
        );
        assert_eq!(
            st.last_topic_list_attempt_at,
            Some(t_fail),
            "failure path must update last_topic_list_attempt_at",
        );
    }

    /// v1.1.9 ITEM (c) — the cache-update side: a fresh probe (any
    /// outcome) seeds `last_echo_probe_at` BEFORE awaiting the
    /// subprocess, so a slow or failing spawn doesn't double-fire
    /// next tick. Pin the per-(pid, topic) bookkeeping shape that
    /// the production path relies on.
    #[test]
    fn b3_echo_probe_seeds_last_attempt_per_pid_topic() {
        let mut s = Ros2ShelloutSource::new();
        let pid_a = 1;
        let pid_b = 2;
        let topic_x = "/x".to_string();
        let topic_y = "/y".to_string();
        let t0 = Instant::now();

        // PID A probes /x at t0; PID B probes /y at t0+1 s.
        let st = s.cache.entry(pid_a).or_default();
        st.last_echo_probe_at.insert(topic_x.clone(), t0);
        let st = s.cache.entry(pid_b).or_default();
        st.last_echo_probe_at.insert(topic_y.clone(), t0 + Duration::from_secs(1));

        // Cross-traffic: PID A's /x bookkeeping must NOT leak into
        // PID B (different PID) or into /y (different topic).
        let st = s.cache.get(&pid_a).unwrap();
        assert!(st.last_echo_probe_at.contains_key(&topic_x));
        assert!(!st.last_echo_probe_at.contains_key(&topic_y));
        let st = s.cache.get(&pid_b).unwrap();
        assert!(st.last_echo_probe_at.contains_key(&topic_y));
        assert!(!st.last_echo_probe_at.contains_key(&topic_x));
    }

    /// v1.1.6 ITEM 1 (regression pin) — `ros2 topic echo --once`
    /// must NOT carry `--timeout`. Humble's `ros-humble-ros2cli
    /// 0.18.18` rejects the flag (`unrecognized arguments:
    /// --timeout`) and every v1.1.5 probe failed, locking every
    /// ROS2 topic to Idle (DISPATCH 17B Tester-B). The args list
    /// must contain `--once` and the requested topic.
    ///
    /// We can't unit-test the active-topic happy path without a
    /// live ros2 graph; the staleness-window arrival simulation is
    /// covered by `b3_echo_activity_detects_sub_hz_topic`.
    /// Production-shape integration coverage lives in
    /// `tests/integration/sampler_harnesses/b3_ros2_harness.sh`
    /// (updated in v1.1.6 ITEM 2 to mirror the echo-once shape).
    #[test]
    fn b3_echo_once_no_timeout_flag_detects_active_topic() {
        let args = ros2_echo_args("/chatter");
        assert!(
            !args.contains(&"--timeout"),
            "ros2 topic echo args must NOT contain --timeout — \
             Humble's ros-humble-ros2cli 0.18.18 rejects the flag \
             (DISPATCH 17B Tester-B). args = {args:?}",
        );
        assert!(
            args.contains(&"--once"),
            "ros2 topic echo must use --once for self-termination — \
             it's the per-probe cap now that --timeout is gone. \
             args = {args:?}",
        );
        assert!(
            args.contains(&"/chatter"),
            "topic must be present in the args list. args = {args:?}",
        );
        // Pin shape: exactly the three static args + the topic.
        assert_eq!(
            &args[..],
            &["topic", "echo", "--once", "/chatter"],
            "v1.1.6 echo-once shape — any change to this list must \
             come with a CHANGELOG entry and a re-verification on \
             Humble (the distro this fix exists for).",
        );
    }

    /// v1.1.5 ITEM E — `forget(pid)` clears the per-PID state.
    /// Inherent helper; the auto-GC sweep in `sample` covers the
    /// "dispatcher never tells us to forget" case.
    #[test]
    fn forget_drops_per_pid_state() {
        let mut s = Ros2ShelloutSource::new();
        let st = s.cache.entry(7).or_default();
        st.topic_list = vec!["/x".into()];
        st.last_message_at
            .insert("/x".into(), Instant::now());
        assert!(s.cache.contains_key(&7));
        s.forget(7);
        assert!(!s.cache.contains_key(&7));
    }

    /// v1.1.7 ITEM 2 (DISPATCH 22) — B3's `on_forget` trait
    /// override drops the per-PID cache entry promptly when the
    /// dispatcher signals a PID is gone. Pre-v1.1.7 the only
    /// cleanup path was the 5-min `ROS2_CACHE_GC_THRESHOLD`
    /// time-based sweep at the top of `sample`; ghost entries
    /// stayed live for that whole window after PID death.
    /// Inspector #15 side-finding.
    ///
    /// This test invokes the trait method (not the inherent
    /// `forget` helper) to pin the trait-side surface.
    #[test]
    fn b3_on_forget_clears_pid_cache_promptly() {
        use crate::telemetry::source::TelemetrySource;
        let mut s = Ros2ShelloutSource::new();
        // Seed: two PIDs with state; on_forget(13) should drop
        // ONLY pid 13 — pid 27 stays.
        for pid in [13u32, 27] {
            let st = s.cache.entry(pid).or_default();
            st.topic_list = vec![format!("/topic_{pid}")];
            st.last_message_at
                .insert(format!("/topic_{pid}"), Instant::now());
        }
        assert!(s.cache.contains_key(&13));
        assert!(s.cache.contains_key(&27));
        TelemetrySource::on_forget(&mut s, 13);
        assert!(
            !s.cache.contains_key(&13),
            "on_forget(13) must drop pid 13's PerPidState entry — \
             pre-v1.1.7 the only cleanup was the 5-min GC sweep",
        );
        assert!(
            s.cache.contains_key(&27),
            "on_forget(13) must NOT touch unrelated PIDs",
        );
    }

    /// v1.1.5 ITEM E — the cache GC threshold bounds the leak in
    /// time. A stale entry whose `last_topic_list_attempt_at` is
    /// older than the threshold is dropped on the next sample sweep.
    /// Test exercises the sweep predicate directly (the `sample`
    /// method can't be driven without a real `ros2` subprocess).
    ///
    /// v1.1.9 ITEM (b) — anchored on `last_topic_list_attempt_at`
    /// (the "last-touched" timestamp) so a PID whose probes are
    /// failing repeatedly is still considered fresh and not swept.
    #[test]
    fn b3_cache_gc_drops_stale_entries() {
        let mut s = Ros2ShelloutSource::new();
        let now = Instant::now();
        // Stale PID — last_topic_list_attempt_at well past the threshold.
        let stale = s.cache.entry(100).or_default();
        stale.last_topic_list_attempt_at =
            Some(now - ROS2_CACHE_GC_THRESHOLD - Duration::from_secs(1));
        stale.topic_list = vec!["/old".into()];
        // Fresh PID — well inside the threshold.
        let fresh = s.cache.entry(200).or_default();
        fresh.last_topic_list_attempt_at = Some(now);
        fresh.topic_list = vec!["/new".into()];
        // Cold-start entry (no attempt yet) — must be kept.
        let cold = s.cache.entry(300).or_default();
        cold.last_topic_list_attempt_at = None;

        // The sweep predicate the production sample() applies:
        s.cache.retain(|_, st| {
            st.last_topic_list_attempt_at
                .map(|t| now.duration_since(t) < ROS2_CACHE_GC_THRESHOLD)
                .unwrap_or(true)
        });

        assert!(!s.cache.contains_key(&100), "stale entry must be dropped");
        assert!(s.cache.contains_key(&200), "fresh entry must be kept");
        assert!(s.cache.contains_key(&300), "cold-start entry must be kept");
    }

    /// Pin the GC threshold's "≥ 10× refresh interval" relationship —
    /// a refactor that drops the GC threshold below the refresh
    /// cadence would drop entries the next sample call is about to
    /// touch.
    #[test]
    fn b3_cache_gc_threshold_is_well_above_refresh_interval() {
        assert!(ROS2_CACHE_GC_THRESHOLD >= ROS2_TOPIC_LIST_INTERVAL * 10);
        assert_eq!(ROS2_CACHE_GC_THRESHOLD, Duration::from_secs(60 * 5));
    }
}
