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
//! returns `"(no metrics)"` for the category until topic-rate
//! telemetry lands.

use std::fs;

use crate::model::{AICategory, ClassificationResult, ProcessSample, WorkloadCategory};

/// Env vars whose presence (with any non-empty value) signals a
/// ROS2 process. `ROS_DOMAIN_ID` is the canonical signal — set by
/// `rclpy.init()`, `rclcpp::init()`, and the `ros2 launch` runner.
/// The others reinforce when present but aren't required.
const ROS2_ENV_VARS: &[&str] = &[
    "ROS_DOMAIN_ID",
    "RMW_IMPLEMENTATION",
    "ROS_DISTRO",
    "AMENT_PREFIX_PATH",
    "RMW_FASTRTPS_PUBLICATION_MODE",
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

/// Top-level entry — runs all three signal checks in order and
/// returns a `WorkloadCategory::ROS2` classification on the first
/// match. Returns `None` when no signal fires (the dispatch falls
/// through to the LLM/Vision/Embeddings classifiers).
pub(crate) fn classify(sample: &ProcessSample) -> Option<ClassificationResult> {
    if let Some(var) = ros2_env_signal(sample) {
        let evidence = format!("ROS2 env var present: {var}");
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
    None
}

fn make_ros2_result(evidence: String) -> ClassificationResult {
    ClassificationResult::ai(AICategory::Inference, WorkloadCategory::ROS2, evidence)
}

/// Returns the matched env var name when any ROS2 signal var is
/// present with a non-empty value.
pub(crate) fn ros2_env_signal(sample: &ProcessSample) -> Option<&'static str> {
    ROS2_ENV_VARS.iter().copied().find(|var| {
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
    // Env-var signal
    // ────────────────────────────────────────────────────────────

    #[test]
    fn ros_domain_id_env_signals_ros2() {
        let s = sample_with_env(
            "python3",
            &["python3", "perception_node.py"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        assert_eq!(ros2_env_signal(&s), Some("ROS_DOMAIN_ID"));
    }

    #[test]
    fn rmw_implementation_env_signals_ros2() {
        let s = sample_with_env(
            "rclcpp_component_container",
            &["rclcpp_component_container"],
            &[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")],
        );
        assert_eq!(ros2_env_signal(&s), Some("RMW_IMPLEMENTATION"));
    }

    #[test]
    fn ros_distro_env_signals_ros2() {
        let s = sample_with_env(
            "node_exec",
            &["node_exec"],
            &[("ROS_DISTRO", "humble")],
        );
        assert_eq!(ros2_env_signal(&s), Some("ROS_DISTRO"));
    }

    #[test]
    fn ament_prefix_path_env_signals_ros2() {
        let s = sample_with_env(
            "x",
            &["x"],
            &[("AMENT_PREFIX_PATH", "/opt/ros/humble")],
        );
        assert_eq!(ros2_env_signal(&s), Some("AMENT_PREFIX_PATH"));
    }

    #[test]
    fn empty_env_var_value_does_not_signal_ros2() {
        // Defensive: an env var with an empty value (sometimes set
        // by shell quoting accidents) must not fire. Real ROS2
        // processes always have a non-empty ROS_DOMAIN_ID etc.
        let s = sample_with_env("x", &["x"], &[("ROS_DOMAIN_ID", "")]);
        assert_eq!(ros2_env_signal(&s), None);
    }

    #[test]
    fn no_ros_env_returns_none() {
        let s = sample("python3", &["python3", "train.py"]);
        assert_eq!(ros2_env_signal(&s), None);
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
}
