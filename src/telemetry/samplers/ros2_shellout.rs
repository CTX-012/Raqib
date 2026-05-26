//! ROS2 topic-rate sampler (Phase 2 / DISPATCH 2B / B3, Hybrid 1).
//!
//! Shells out to the `ros2` CLI to observe topic publication rate
//! per AI-classified ROS2 process. Maps rate to
//! [`ActivityState`]: a topic publishing at >0 Hz → `Active`; an
//! existing-but-unpublished topic (the WARNING fast-fail line) →
//! `Idle`; cold start / no topics → `NotDetected`.
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
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::telemetry::source::{
    ActivityState, ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
};

// PROVISIONAL: refined post-v1.1.0 sampler validation (v1.1.1).
// CAR-candidate: lift to ux_contract once stable.
//
// `ros2 topic list` is one-shot — runs to completion in <100 ms on
// the empirical host. The 30 s cadence is "how often to re-discover
// the topic set." Most ROS2 graphs change topics rarely; a 30-second
// staleness window is plenty.
const ROS2_TOPIC_LIST_INTERVAL: Duration = Duration::from_secs(30);

// `ros2 topic hz <topic>` needs to stay alive long enough to observe
// a rate sample. 60 s between hz probes per topic balances "fresh
// activity reading" against "subprocess churn."
const ROS2_TOPIC_HZ_INTERVAL: Duration = Duration::from_secs(60);

// EMPIRICAL: Tester-A confirmed `ros2 topic hz` needs ≥3 messages
// before first emit (~3 s at 1 Hz). 2 s would clip healthy 1 Hz
// topics. 5 s is the conservative floor; scale per expected min
// rate if a future deployment cares.
const ROS2_SHELLOUT_TIMEOUT: Duration = Duration::from_secs(5);

/// WARNING-line prefix emitted by `ros2 topic hz` when the named
/// topic has no publisher (Tester-A CHANGE 3). Stable across
/// `ros-humble-ros2cli 0.18.18` and any topic name we'd hand it.
const WARNING_NO_PUBLISHER_PREFIX: &str = "WARNING: topic [";

/// Env var stamped onto every B3-spawned subprocess so a future
/// classifier mitigation can recognise and skip self-classification.
/// Unwired today — the recursion is documented as a known issue
/// pending the classifier-side hook.
const SAMPLER_MARKER_ENV: &str = "EDGE_MONITOR_SAMPLER";
const SAMPLER_MARKER_VALUE: &str = "ros2-shellout";

/// Compiled regex matching the `average rate: <float>` line in
/// `ros2 topic hz` output. Tester-A's hex-dump verification: no ANSI
/// codes, no leading whitespace, bare float (Hz implied by program
/// name), LF terminator.
fn rate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // ok: expect — compile-time-constant regex; a malformed pattern
    // would surface at the first call in any unit test long before
    // hitting production.
    RE.get_or_init(|| {
        Regex::new(r"^average rate: ([0-9]+\.[0-9]+)\s*$").expect("rate regex compiles")
    })
}

/// Per-PID state cached across `sample` calls. Tracks last
/// topic-list and hz refresh timestamps so we don't re-shell every
/// tick; the dispatcher calls `sample` once per tick (1 Hz default)
/// but the underlying ROS2 graph doesn't change topic shape that
/// fast.
#[derive(Debug, Default)]
struct PerPidState {
    last_topic_list_at: Option<Instant>,
    last_topic_hz_at: Option<Instant>,
    /// Cached topic list from the most recent `ros2 topic list` run.
    /// Tester-A's capture: default Humble install has 3 topics
    /// (`/chatter`, `/parameter_events`, `/rosout`); production
    /// graphs typically have low tens. Unbounded by design — a
    /// future overgrown graph would still fit in memory cheaply.
    topic_list: Vec<String>,
    /// Most recently observed activity state. Returned on subsequent
    /// ticks within the hz interval so the renderer doesn't flicker
    /// between `Active` and `NotDetected` every tick.
    last_activity: Option<ActivityState>,
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

    /// Spawn `ros2 topic hz <topic>` and observe the first emitted
    /// rate sample. Returns `Ok(Some(rate))` on success,
    /// `Ok(None)` on WARNING fast-fail (topic exists but no
    /// publisher), and `Err(Transient)` on spawn / timeout failure.
    ///
    /// Always kills the child before returning — `ros2 topic hz`
    /// never exits on its own, and Tester-A verified SIGTERM/SIGKILL
    /// both reap within ~100 ms with no SIGKILL escalation needed.
    ///
    /// Does not borrow `&self` (same rationale as `run_topic_list`).
    async fn observe_topic_hz(topic: &str) -> SourceResult<Option<f32>> {
        let mut child = Command::new("ros2")
            .args(["topic", "hz", topic])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env(SAMPLER_MARKER_ENV, SAMPLER_MARKER_VALUE)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SourceError::Transient(format!("ros2 topic hz spawn failed: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SourceError::Transient("ros2 topic hz missing stdout".into()))?;

        let read_fut = read_first_rate_or_warning(stdout);
        let timed = tokio::time::timeout(ROS2_SHELLOUT_TIMEOUT, read_fut).await;

        // Kill before returning so the subprocess never outlives this
        // call (kill_on_drop is the belt; this is the braces — keeps
        // the contract explicit and the wait-for-reap synchronous).
        let _ = child.kill().await;

        match timed {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SourceError::Transient(format!(
                "ros2 topic hz timed out after {} s",
                ROS2_SHELLOUT_TIMEOUT.as_secs()
            ))),
        }
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

/// Stream-read `ros2 topic hz` stdout until we get either the first
/// rate sample (success) or the WARNING fast-fail line (Idle).
/// Returns `Err(Transient)` if the subprocess closes stdout without
/// producing either, which we treat as a "retry next tick" condition.
async fn read_first_rate_or_warning(
    stdout: tokio::process::ChildStdout,
) -> SourceResult<Option<f32>> {
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| SourceError::Transient(format!("ros2 topic hz read failed: {e}")))?
    {
        // WARNING fast-fail (Tester-A CHANGE 3): topic exists but
        // no publisher. ros2 emits this line within ~1 s and then
        // sits silent; without this branch we'd wait out the full
        // 5 s timeout for every unpublished topic.
        if line.starts_with(WARNING_NO_PUBLISHER_PREFIX)
            && line.contains("does not appear to be published yet")
        {
            return Ok(None);
        }
        if let Some(rate) = parse_rate_line(&line) {
            return Ok(Some(rate));
        }
        // Detail / std-dev lines fall through silently; we only act
        // on the rate header and the warning.
    }
    Err(SourceError::Transient(
        "ros2 topic hz closed stdout without rate or WARNING".into(),
    ))
}

/// Pure parser for a single `average rate: <float>` line. Public for
/// unit tests; returns `None` on non-rate-line input.
pub(crate) fn parse_rate_line(line: &str) -> Option<f32> {
    rate_re()
        .captures(line)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f32>().ok())
}

#[async_trait]
impl TelemetrySource for Ros2ShelloutSource {
    fn name(&self) -> &str {
        "ros2-shellout"
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

        // Refresh topic list per cadence. On cold start (no prior
        // observation) we MUST run it; otherwise we'd have no topic
        // to hz-probe.
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
                Err(e) => {
                    // List failed — keep prior state (if any) and
                    // surface the transient error so the dispatcher
                    // can log it.
                    return Err(e);
                }
            }
        }

        if state.topic_list.is_empty() {
            // No topics → process is alive but not participating in
            // a publishing graph. Idle is the honest read; the
            // operator-facing UI hides the column entirely for rows
            // with no other Phase-2 reporters.
            let frame = activity_frame(proc.pid, ActivityState::Idle);
            state.last_activity = Some(ActivityState::Idle);
            return Ok(frame);
        }

        // Hz cadence: only re-probe every ROS2_TOPIC_HZ_INTERVAL.
        // Between probes, return the most recently observed state
        // (NotDetected on cold start; previous value otherwise) so
        // the column doesn't flicker.
        let need_hz = state
            .last_topic_hz_at
            .map(|t| now.duration_since(t) >= ROS2_TOPIC_HZ_INTERVAL)
            .unwrap_or(true);
        if !need_hz {
            let cached = state.last_activity.unwrap_or(ActivityState::NotDetected);
            return Ok(activity_frame(proc.pid, cached));
        }

        // Pick the first topic to probe this tick. v1.1.1+ can
        // round-robin across `state.topic_list` so every topic gets
        // sampled within a longer window; v1.1.0 keeps it simple to
        // stay inside the dispatcher's 1 s outer budget per tick.
        let topic = state.topic_list[0].clone();
        state.last_topic_hz_at = Some(now);

        // Snapshot the prior activity (the only field we need from
        // `state` after the await) so the &mut borrow doesn't bridge
        // the await point — required because the await uses
        // `Self::observe_topic_hz`, which doesn't borrow self.cache,
        // but holding `state: &mut PerPidState` across it would still
        // hold a mutable borrow of `self.cache`.
        let prior = state.last_activity;
        let pid_for_log = proc.pid;
        let _ = state; // release the &mut self.cache borrow before .await

        let outcome = Self::observe_topic_hz(&topic).await;
        let activity = match outcome {
            Ok(Some(rate)) if rate > 0.0 => ActivityState::Active,
            Ok(Some(_)) => ActivityState::Idle, // rate observed as 0.0 — degenerate but not active
            Ok(None) => ActivityState::Idle,    // WARNING fast-fail
            Err(e) => {
                // hz subprocess failed — keep prior state if any, but
                // surface the transient so the dispatcher logs it.
                tracing::warn!(
                    sampler = self.name(),
                    pid = pid_for_log,
                    topic = %topic,
                    error = %e,
                    "ros2 topic hz observation failed"
                );
                prior.unwrap_or(ActivityState::NotDetected)
            }
        };
        // Re-borrow to persist the observation.
        self.cache.entry(proc.pid).or_default().last_activity = Some(activity);
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

    // ─── parse_rate_line (Tester-A two-line pair shape) ─────────────

    #[test]
    fn parses_average_rate_line() {
        // raw/ros2_topic_hz_active.txt — first header line.
        assert_eq!(parse_rate_line("average rate: 1.000"), Some(1.0));
    }

    #[test]
    fn parses_average_rate_line_multi_publisher() {
        // raw/ros2_topic_hz_multi_publisher.txt — ~2 Hz.
        assert_eq!(parse_rate_line("average rate: 2.000"), Some(2.0));
        assert_eq!(parse_rate_line("average rate: 1.928"), Some(1.928));
    }

    #[test]
    fn skips_tab_indented_detail_line() {
        // The detail line starts with a TAB (0x09) and has multiple
        // `key: value` pairs. Must NOT match the rate regex.
        let detail = "\tmin: 1.000s max: 1.001s std dev: 0.00052s window: 3";
        assert_eq!(parse_rate_line(detail), None);
    }

    #[test]
    fn skips_warning_line() {
        let warn = "WARNING: topic [/nonexistent] does not appear to be published yet";
        assert_eq!(parse_rate_line(warn), None);
    }

    #[test]
    fn rejects_rate_without_decimal_point() {
        // Tester-A's capture always showed `<int>.<frac>`; refuse to
        // match a bare integer in case some future ros2cli release
        // emits one — better to retry next tick than silently parse a
        // shape we haven't verified.
        assert_eq!(parse_rate_line("average rate: 1"), None);
    }

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
}
