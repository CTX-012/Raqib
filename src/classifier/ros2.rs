//! L9 / UX_CONTRACT.md §1 region 4 — ROS2 detection.
//!
//! Process-level only for v1.0. Three independent signals trigger
//! `WorkloadCategory::ROS2`:
//!
//! 1. **Environment variables** — set by `ros2 launch`, the rclpy
//!    init path, and the rclcpp/rmw initialisation. Most reliable
//!    of the three; checked first because it requires no I/O
//!    beyond what `ProcessSample::environ` already collected.
//! 2. **Command line** — `ros2 run`, `ros2 launch`, the
//!    `rclcpp_component_container` host process, and bare `rclpy`
//!    invocations.
//! 3. **Linked libraries** — read `/proc/<pid>/maps` and look for
//!    `librclcpp.so`, `librclpy.so`, `libfastdds.so`,
//!    `libfastrtps.so`. Catches C++ nodes that were spawned
//!    outside of `ros2 launch` and so don't carry the env vars.
//!
//! Per UX_CONTRACT.md §1 region 4, ROS2 detection runs **before**
//! the LLM/Vision/Embeddings classifiers — a perception node that
//! also imports `torch` for inference is still a ROS2 process to
//! the operator, and grouping it as LLM would hide it from the
//! ROS2 section. ROS1 patterns (`rosrun`, `roslaunch`,
//! `ROS_MASTER_URI`) are intentionally NOT detected (out of scope
//! per contract §0).
//!
//! Hz / per-topic rate sampling is **v1.1+**. v1.0 ROS2 rows show
//! process-level CPU/RAM only; `panels::workloads::primary_metric`
//! returns `"running actively"` for the category until topic-rate
//! telemetry lands.

use std::fs;

use crate::model::{AICategory, ClassificationResult, ProcessSample, WorkloadCategory};

/// Fix-1 — env vars that are reliably set **at runtime** by the
/// rclpy/rclcpp init path (`rclpy.init()`, `rclcpp::init()`, the
/// `ros2 launch` runner). Presence implies the process actually
/// joined a ROS2 graph, so this signal is standalone trustworthy.
///
/// Today this list is just `ROS_DOMAIN_ID`; other runtime-set vars
/// can be added here if a future RMW or distro emits something that
/// only fires at init time and never at shell-source time.
const ROS2_RUNTIME_ENV_VARS: &[&str] = &["ROS_DOMAIN_ID"];

/// Fix-1 — env vars set by `/opt/ros/<distro>/setup.bash` (the
/// "source ROS" shell hook). Every process spawned from a shell that
/// sourced that file inherits these — including Firefox, the
/// VS Code Remote-SSH server, Claude Code agents, and coreutils. So
/// these alone do **not** prove a process is a ROS2 node; they only
/// prove its environment came from a ROS-sourced shell.
///
/// The classifier requires a cmdline OR `/proc/<pid>/maps` corroboration
/// alongside any of these — see [`classify`]. The list is kept around
/// (rather than deleted) so [`ros2_shell_env_signal`] can still
/// surface "this process inherited a ROS shell env" for future
/// weighted-scoring or operator-debug surfaces.
const ROS2_SHELL_ENV_VARS: &[&str] = &[
    "RMW_IMPLEMENTATION",
    "ROS_DISTRO",
    "AMENT_PREFIX_PATH",
    "ROS_VERSION",
];

/// Substrings (lowercased, word-boundary not required because
/// these strings are unambiguous) that match in a cmdline join.
const ROS2_CMDLINE_MARKERS: &[&str] = &[
    "ros2 run",
    "ros2 launch",
    "ros2 service",
    "ros2 topic",
    "ros2 node",
    "rclcpp_component_container",
    "rclpy",
];

/// Library basenames that signal ROS2 linkage. Looked up as
/// substrings of `/proc/<pid>/maps` lines so the absolute install
/// path doesn't matter. `libfastdds`/`libfastrtps` cover the
/// default DDS implementations on Humble; cyclonedds-only nodes are
/// caught via the rcl libraries.
const ROS2_LIBRARY_MARKERS: &[&str] = &[
    "librclcpp.so",
    "librclpy.so",
    "libfastdds.so",
    "libfastrtps.so",
];

/// Top-level entry — runs the signal checks in priority order and
/// returns a `WorkloadCategory::ROS2` classification on the first
/// standalone-trustworthy match. Returns `None` when no signal
/// fires (the dispatch falls through to the LLM/Vision/Embeddings
/// classifiers).
///
/// Fix-1 / classifier audit: only [`ros2_runtime_env_signal`]
/// (currently `ROS_DOMAIN_ID`), [`ros2_cmdline_signal`], and
/// [`ros2_library_signal`] are standalone-trustworthy. The
/// shell-set env vars (`RMW_IMPLEMENTATION`, `ROS_DISTRO`,
/// `AMENT_PREFIX_PATH`, `ROS_VERSION`) are inherited by every
/// process spawned from a ROS-sourced shell and therefore can't
/// classify on their own — see [`ros2_shell_env_signal`] for the
/// surfaced-but-not-load-bearing helper.
pub(crate) fn classify(sample: &ProcessSample) -> Option<ClassificationResult> {
    if let Some(var) = ros2_runtime_env_signal(sample) {
        let evidence = format!("ROS2 runtime env var present: {var}");
        return Some(make_ros2_result(evidence));
    }
    if let Some(marker) = ros2_cmdline_signal(&sample.cmdline) {
        let evidence = format!("cmdline matches ROS2 marker {marker:?}");
        return Some(make_ros2_result(evidence));
    }
    if let Some(lib) = ros2_library_signal(sample.pid) {
        let evidence = format!("/proc/{}/maps links {}", sample.pid, lib);
        return Some(make_ros2_result(evidence));
    }
    // Fix-1 — a shell-set env signal (RMW_IMPLEMENTATION et al.)
    // alone is not enough; if we got here, no cmdline or library
    // signal backed it up, so the process is most likely a regular
    // user-shell process that just inherited the env. Fall through
    // to the AI classifiers (most of which will return None for a
    // non-AI process, which is exactly what we want).
    None
}

fn make_ros2_result(evidence: String) -> ClassificationResult {
    ClassificationResult::ai(AICategory::Inference, WorkloadCategory::ROS2, evidence)
}

/// Fix-1 — returns the matched env var name when a *runtime-set*
/// ROS2 env var is present with a non-empty value. Currently just
/// `ROS_DOMAIN_ID` — set by `rclpy.init()` / `rclcpp::init()` and
/// the `ros2 launch` runner, so its presence is strong evidence
/// the process actually joined a ROS2 graph.
pub(crate) fn ros2_runtime_env_signal(sample: &ProcessSample) -> Option<&'static str> {
    env_signal_for(sample, ROS2_RUNTIME_ENV_VARS)
}

/// Fix-1 — returns the matched env var name when a *shell-set* ROS2
/// env var is present with a non-empty value (`RMW_IMPLEMENTATION`,
/// `ROS_DISTRO`, `AMENT_PREFIX_PATH`, `ROS_VERSION`). Not used by
/// [`classify`] on its own — every shell that sources
/// `/opt/ros/<distro>/setup.bash` propagates these to every child
/// process, so they'd false-positive on user shells, browsers, IDEs,
/// and coreutils. Exposed so future weighted-scoring code (or
/// operator-debug surfaces) can read the signal without re-deriving
/// the list.
pub(crate) fn ros2_shell_env_signal(sample: &ProcessSample) -> Option<&'static str> {
    env_signal_for(sample, ROS2_SHELL_ENV_VARS)
}

fn env_signal_for(sample: &ProcessSample, vars: &[&'static str]) -> Option<&'static str> {
    vars.iter().copied().find(|var| {
        sample
            .environ
            .get(*var)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    })
}

/// Returns the matched marker substring when the cmdline (joined
/// with spaces, lowercased) contains any ROS2 cmdline pattern.
pub(crate) fn ros2_cmdline_signal(cmdline: &[String]) -> Option<&'static str> {
    if cmdline.is_empty() {
        return None;
    }
    let joined = cmdline.join(" ").to_lowercase();
    ROS2_CMDLINE_MARKERS
        .iter()
        .copied()
        .find(|marker| joined.contains(marker))
}

/// Reads `/proc/<pid>/maps` and returns the matched library
/// basename when any ROS2 library is linked. Errors (permission
/// denied, file gone, kernel-thread PID with no maps) silently
/// fall through to `None` — the env / cmdline signals already
/// cover most real ROS2 processes.
pub(crate) fn ros2_library_signal(pid: u32) -> Option<&'static str> {
    let path = format!("/proc/{pid}/maps");
    match fs::read_to_string(&path) {
        Ok(contents) => ros2_library_in_maps_text(&contents),
        Err(_) => None,
    }
}

/// Pure substring search over a `/proc/<pid>/maps` snapshot. Tests
/// drive this directly with synthetic content; the I/O wrapper
/// above stays a thin shim.
pub(crate) fn ros2_library_in_maps_text(maps: &str) -> Option<&'static str> {
    ROS2_LIBRARY_MARKERS
        .iter()
        .copied()
        .find(|lib| maps.contains(lib))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample(name: &str, argv: &[&str]) -> ProcessSample {
        ProcessSample {
            pid: 4242,
            ppid: Some(1),
            name: name.into(),
            cmdline: argv.iter().map(|s| s.to_string()).collect(),
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        }
    }

    fn sample_with_env(name: &str, argv: &[&str], env: &[(&str, &str)]) -> ProcessSample {
        ProcessSample {
            environ: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..sample(name, argv)
        }
    }

    // ────────────────────────────────────────────────────────────
    // Env-var signal — runtime tier (load-bearing for classify)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn ros_domain_id_runtime_env_signals_ros2() {
        let s = sample_with_env(
            "python3",
            &["python3", "perception_node.py"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        assert_eq!(ros2_runtime_env_signal(&s), Some("ROS_DOMAIN_ID"));
    }

    #[test]
    fn empty_ros_domain_id_does_not_signal_ros2() {
        // Defensive: an env var with an empty value (sometimes set
        // by shell quoting accidents) must not fire. Real ROS2
        // processes always have a non-empty ROS_DOMAIN_ID.
        let s = sample_with_env("x", &["x"], &[("ROS_DOMAIN_ID", "")]);
        assert_eq!(ros2_runtime_env_signal(&s), None);
    }

    #[test]
    fn no_ros_env_returns_none_for_runtime() {
        let s = sample("python3", &["python3", "train.py"]);
        assert_eq!(ros2_runtime_env_signal(&s), None);
    }

    // Fix-1 — shell-set env vars must NOT register as runtime
    // signals. The classifier audit found these were the false-
    // positive source: every process spawned from a ROS-sourced
    // shell inherits them, so trusting them standalone misclassified
    // user shells, browsers, and CLI tools as ROS2.
    #[test]
    fn rmw_implementation_does_not_register_as_runtime_signal() {
        let s = sample_with_env(
            "rclcpp_component_container",
            &["rclcpp_component_container"],
            &[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")],
        );
        assert_eq!(ros2_runtime_env_signal(&s), None);
    }

    #[test]
    fn ros_distro_does_not_register_as_runtime_signal() {
        let s = sample_with_env(
            "node_exec",
            &["node_exec"],
            &[("ROS_DISTRO", "humble")],
        );
        assert_eq!(ros2_runtime_env_signal(&s), None);
    }

    #[test]
    fn ament_prefix_path_does_not_register_as_runtime_signal() {
        let s = sample_with_env(
            "x",
            &["x"],
            &[("AMENT_PREFIX_PATH", "/opt/ros/humble")],
        );
        assert_eq!(ros2_runtime_env_signal(&s), None);
    }

    // ────────────────────────────────────────────────────────────
    // Env-var signal — shell tier (informational only, not load-bearing)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn rmw_implementation_registers_as_shell_signal() {
        let s = sample_with_env(
            "x",
            &["x"],
            &[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")],
        );
        assert_eq!(ros2_shell_env_signal(&s), Some("RMW_IMPLEMENTATION"));
    }

    #[test]
    fn ros_distro_registers_as_shell_signal() {
        let s = sample_with_env("x", &["x"], &[("ROS_DISTRO", "humble")]);
        assert_eq!(ros2_shell_env_signal(&s), Some("ROS_DISTRO"));
    }

    #[test]
    fn ament_prefix_path_registers_as_shell_signal() {
        let s = sample_with_env(
            "x",
            &["x"],
            &[("AMENT_PREFIX_PATH", "/opt/ros/humble")],
        );
        assert_eq!(ros2_shell_env_signal(&s), Some("AMENT_PREFIX_PATH"));
    }

    #[test]
    fn ros_version_registers_as_shell_signal() {
        let s = sample_with_env("x", &["x"], &[("ROS_VERSION", "2")]);
        assert_eq!(ros2_shell_env_signal(&s), Some("ROS_VERSION"));
    }

    #[test]
    fn ros_domain_id_does_not_register_as_shell_signal() {
        // ROS_DOMAIN_ID is the *runtime* signal; the shell helper
        // must not also fire on it (otherwise a debug surface that
        // says "this process inherited shell ROS env" would mislead
        // for a genuine `rclpy.init()`'d process).
        let s = sample_with_env("x", &["x"], &[("ROS_DOMAIN_ID", "0")]);
        assert_eq!(ros2_shell_env_signal(&s), None);
    }

    // ────────────────────────────────────────────────────────────
    // Cmdline signal
    // ────────────────────────────────────────────────────────────

    #[test]
    fn ros2_run_cmdline_signals_ros2() {
        let s = sample("ros2", &["ros2", "run", "demo_nodes_cpp", "talker"]);
        assert_eq!(ros2_cmdline_signal(&s.cmdline), Some("ros2 run"));
    }

    #[test]
    fn ros2_launch_cmdline_signals_ros2() {
        let s = sample(
            "ros2",
            &["ros2", "launch", "my_pkg", "system.launch.py"],
        );
        assert_eq!(ros2_cmdline_signal(&s.cmdline), Some("ros2 launch"));
    }

    #[test]
    fn rclcpp_component_container_signals_ros2() {
        let s = sample(
            "rclcpp_component_container",
            &["rclcpp_component_container", "--ros-args"],
        );
        assert_eq!(
            ros2_cmdline_signal(&s.cmdline),
            Some("rclcpp_component_container")
        );
    }

    #[test]
    fn rclpy_module_signals_ros2() {
        let s = sample(
            "python3",
            &["python3", "-m", "rclpy.executor_test"],
        );
        assert_eq!(ros2_cmdline_signal(&s.cmdline), Some("rclpy"));
    }

    #[test]
    fn ros1_rosrun_does_not_signal_ros2() {
        // Per UX_CONTRACT.md §0, ROS1 patterns are intentionally
        // NOT detected. `rosrun` (ROS1) must not match any ROS2
        // marker, even though "ros" is a substring of "ros2".
        let s = sample("rosrun", &["rosrun", "my_pkg", "node"]);
        assert_eq!(ros2_cmdline_signal(&s.cmdline), None);
    }

    #[test]
    fn empty_cmdline_returns_none() {
        let cmdline: Vec<String> = vec![];
        assert_eq!(ros2_cmdline_signal(&cmdline), None);
    }

    // ────────────────────────────────────────────────────────────
    // Library-link signal (pure helper)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn rclpy_in_maps_signals_ros2() {
        let maps = "\
7f1234567000-7f1234600000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librclpy.so.5.3.0
7f1234600000-7f1234700000 rw-p 00000000 00:00 0 [heap]
";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            Some("librclpy.so")
        );
    }

    #[test]
    fn rclcpp_in_maps_signals_ros2() {
        let maps = "00400000-00500000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librclcpp.so.18.1\n";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            Some("librclcpp.so")
        );
    }

    #[test]
    fn fastdds_in_maps_signals_ros2() {
        let maps = "00400000-00500000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/libfastdds.so.2.10\n";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            Some("libfastdds.so")
        );
    }

    #[test]
    fn fastrtps_in_maps_signals_ros2() {
        let maps = "00400000-00500000 r-xp 00000000 00:00 0 /usr/lib/x86_64-linux-gnu/libfastrtps.so.2.6\n";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            Some("libfastrtps.so")
        );
    }

    #[test]
    fn no_ros2_libraries_in_maps_returns_none() {
        let maps = "00400000-00500000 r-xp 00000000 00:00 0 /usr/lib/libtorch.so\n";
        assert_eq!(ros2_library_in_maps_text(maps), None);
    }

    #[test]
    fn empty_maps_returns_none() {
        assert_eq!(ros2_library_in_maps_text(""), None);
    }

    // ────────────────────────────────────────────────────────────
    // Top-level classify
    // ────────────────────────────────────────────────────────────

    #[test]
    fn classify_fires_ros2_on_env_signal() {
        let s = sample_with_env(
            "python3",
            &["python3", "perception_node.py"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        let result = classify(&s).expect("should classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
        assert_eq!(result.category, AICategory::Inference);
        assert!(
            result.evidence.contains("ROS_DOMAIN_ID"),
            "evidence: {}",
            result.evidence
        );
    }

    #[test]
    fn classify_fires_ros2_on_cmdline_signal() {
        let s = sample("ros2", &["ros2", "run", "demo_nodes_cpp", "talker"]);
        let result = classify(&s).expect("should classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
    }

    #[test]
    fn classify_returns_none_for_non_ros2_python() {
        let s = sample("python3", &["python3", "train.py", "--lr", "0.001"]);
        assert!(classify(&s).is_none());
    }

    // ────────────────────────────────────────────────────────────
    // Fix-1 — shell-set env vars alone must NOT classify as ROS2.
    // Smoke-tested against the user's actual /proc/PID/environ
    // contents: every user-shell process spawned after sourcing
    // /opt/ros/humble/setup.bash inherits these, so trusting them
    // standalone misclassifies bash, node, claude, browsers, etc.
    // ────────────────────────────────────────────────────────────

    #[test]
    fn classify_returns_none_for_shell_env_alone_rmw() {
        let s = sample_with_env(
            "node",
            &["node", "/home/u/.vscode-server/extensions/anthropic.claude-code/cli.js"],
            &[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")],
        );
        assert!(
            classify(&s).is_none(),
            "RMW_IMPLEMENTATION alone must not classify as ROS2 \
             (it's set by setup.bash; every shell child inherits it)",
        );
    }

    #[test]
    fn classify_returns_none_for_shell_env_alone_distro() {
        let s = sample_with_env("bash", &["bash"], &[("ROS_DISTRO", "humble")]);
        assert!(classify(&s).is_none());
    }

    #[test]
    fn classify_returns_none_for_shell_env_alone_ament() {
        let s = sample_with_env(
            "head",
            &["head", "-n", "10"],
            &[("AMENT_PREFIX_PATH", "/opt/ros/humble")],
        );
        assert!(classify(&s).is_none());
    }

    #[test]
    fn classify_returns_none_for_full_setup_bash_env_with_no_corroboration() {
        // The full kit the user's `source /opt/ros/humble/setup.bash`
        // exports — pinned together because that's the realistic
        // case-in-the-wild. Without a cmdline or library signal,
        // none of these prove the process is a ROS2 node.
        let s = sample_with_env(
            "claude",
            &["claude"],
            &[
                ("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp"),
                ("ROS_DISTRO", "humble"),
                ("AMENT_PREFIX_PATH", "/opt/ros/humble"),
                ("ROS_VERSION", "2"),
            ],
        );
        assert!(classify(&s).is_none());
    }

    #[test]
    fn classify_fires_ros2_on_shell_env_plus_cmdline_marker() {
        // Cmdline marker is standalone-trustworthy, so this test
        // pins the "shell env doesn't break legitimate ROS2
        // processes" invariant: a real `ros2 run` invocation with
        // the typical setup.bash env still classifies correctly.
        let s = sample_with_env(
            "ros2",
            &["ros2", "run", "demo_nodes_cpp", "talker"],
            &[
                ("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp"),
                ("ROS_DISTRO", "humble"),
            ],
        );
        let result = classify(&s).expect("should classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
        // Evidence should cite the cmdline marker, not the env var,
        // because the cmdline check fires first under the Fix-1
        // dispatch ordering.
        assert!(
            result.evidence.contains("cmdline"),
            "evidence should cite cmdline, got: {}",
            result.evidence,
        );
    }

    #[test]
    fn classify_fires_ros2_on_runtime_env_alone() {
        // Mirror of the env-only positive case: ROS_DOMAIN_ID is the
        // runtime signal and remains standalone-trustworthy.
        let s = sample_with_env(
            "perception_node",
            &["perception_node"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        let result = classify(&s).expect("should classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
        assert!(result.evidence.contains("ROS_DOMAIN_ID"));
    }
}
