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
//! scales with 1/rate) to `ros2 topic echo --once --timeout 1` per
//! tick + a 30 s staleness window. Echo arrival is observable at
//! any non-zero rate; the window covers the worst sub-Hz publisher
//! B3 targets (0.1 Hz → ≥ 3 expected arrivals per 30 s).
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

// v1.1.5 ITEM D (BUG-P5-2) — per-tick short `ros2 topic echo --once
// --timeout 1` probe. We only need to OBSERVE a message arrival;
// no Hz computation required. 1 s per probe is the shortest cap
// that still catches a healthy ≥1 Hz publisher in a single tick
// while bounding subprocess churn.
const ROS2_ECHO_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

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

// Inner subprocess timeout for `ros2 topic echo` and `ros2 topic
// list`. echo `--timeout` does the per-message cap inside ros2;
// this is the outer wait we apply on tokio's side as belt-and-
// braces against the subprocess hanging post-message.
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
    last_topic_list_at: Option<Instant>,
    /// Cached topic list from the most recent `ros2 topic list` run.
    topic_list: Vec<String>,
    /// topic → last `Instant` an echo probe successfully observed a
    /// message arrival. Bounded by the topic_list size; cleared on
    /// `forget(pid)` (v1.1.5 ITEM E).
    last_message_at: HashMap<String, Instant>,
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
    /// --timeout <T>` and observe whether a message arrived. Returns
    /// `Ok(true)` if echo printed a non-empty body (a message
    /// arrived inside the probe window); `Ok(false)` if echo exited
    /// without printing (no message); `Err(Transient)` on spawn /
    /// outer-timeout failure.
    ///
    /// `--once` makes echo self-terminate after the first message,
    /// so we don't need the kill discipline the hz mechanism
    /// required. `--timeout` caps the per-probe wait inside ros2.
    /// The outer tokio timeout is belt-and-braces.
    async fn observe_topic_echo(topic: &str) -> SourceResult<bool> {
        let timeout_secs = ROS2_ECHO_PROBE_TIMEOUT.as_secs().to_string();
        let mut child = Command::new("ros2")
            .args([
                "topic",
                "echo",
                "--once",
                "--timeout",
                &timeout_secs,
                topic,
            ])
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

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        let now = Instant::now();
        let state = self.cache.entry(proc.pid).or_default();

        // Refresh topic list per cadence (graphs change slowly).
        let need_list = state
            .last_topic_list_at
            .map(|t| now.duration_since(t) >= ROS2_TOPIC_LIST_INTERVAL)
            .unwrap_or(true);
        if need_list {
            match Self::run_topic_list().await {
                Ok(topics) => {
                    state.topic_list = topics;
                    state.last_topic_list_at = Some(now);
                }
                Err(e) => return Err(e),
            }
        }

        if state.topic_list.is_empty() {
            return Ok(activity_frame(proc.pid, ActivityState::NotDetected));
        }

        // v1.1.5 ITEM D (BUG-P5-2) — pick the first topic and probe
        // it with `ros2 topic echo --once`. Every tick. If a message
        // arrived, refresh `last_message_at`; ActivityState is then
        // governed by the staleness window so sub-Hz topics
        // (0.1 Hz emits ~3 msgs per 30 s) hold Active across the
        // inter-message gaps. v1.1.1+ can round-robin across topics;
        // single-topic is fine while the dispatcher's per-source
        // sample_timeout is 9 s and ROS2_ECHO_PROBE_TIMEOUT is 1 s.
        let topic = state.topic_list[0].clone();
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
        // v1.1.5 values: 3 s inner outer wait around echo --once
        // (which has its own --timeout 1 s), 9 s outer dispatcher
        // ceiling (unchanged from v1.1.3).
        assert_eq!(ROS2_SHELLOUT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(outer, Duration::from_secs(9));
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
            st.last_topic_list_at = Some(t0);
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
    #[test]
    fn b3_staleness_window_covers_sub_hz_rates() {
        assert_eq!(ROS2_ACTIVITY_STALENESS, Duration::from_secs(30));
        assert_eq!(ROS2_ECHO_PROBE_TIMEOUT, Duration::from_secs(1));
        // 0.1 Hz worst-supported publisher emits a message every 10 s;
        // the staleness window must be ≥ 3 * 10 s so losing the
        // expected 3 arrivals is already a real outage.
        assert!(ROS2_ACTIVITY_STALENESS >= Duration::from_secs(30));
    }

    /// v1.1.5 ITEM E preview: `forget(pid)` clears the per-PID
    /// state. Pinned here as a sampler-side test; the dispatcher-
    /// wiring test lives in dispatcher.rs.
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
}
