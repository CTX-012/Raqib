//! L9 / UX_CONTRACT.md §1 region 4 — ROS2 detection.
//!
//! Process-level only for v1.0. Three independent signals trigger
//! `WorkloadCategory::ROS2`:
//!
//! 1. **Linked libraries** — read `/proc/<pid>/maps` and look for
//!    `librcl.so`, `librmw_implementation.so`, `_rclpy_pybind11`,
//!    `librclcpp.so`, and the Fast-DDS RMW libs. Library linkage is
//!    the most reliable signal of the three. Every ROS2 process
//!    loads `librcl.so` + `librmw_implementation.so` at runtime
//!    regardless of distro, RMW backend, or language (Python rclpy
//!    or C++ rclcpp).
//! 2. **Environment variables** — `ROS_DOMAIN_ID`, `ROS_DISTRO`,
//!    `RMW_IMPLEMENTATION`. Reliably exported by `ros2 launch` and
//!    `ros2 run` runners; NOT exported by bare `rclpy.init()` /
//!    `rclcpp::init()` calls. Env signal therefore fires only for
//!    runner-spawned children and operator shells with an explicit
//!    `export ROS_DOMAIN_ID=N`.
//! 3. **Command line** — `ros2 run`, `ros2 launch`, the
//!    `rclcpp_component_container` host process, and bare `rclpy`
//!    invocations. Catches the runners themselves but does NOT
//!    catch realistic `python3 my_node.py` invocations — those
//!    only show up via the library signal.
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

/// Fix-1 / v1.0.3 B-EMPIRICAL-4 — env vars reliably exported by the
/// `ros2 launch` and `ros2 run` runners. Bare `rclpy.init()` /
/// `rclcpp::init()` calls READ these env vars but do not export
/// them, so processes spawned outside of `ros2 launch` / `ros2 run`
/// (e.g. `python3 my_node.py`) will not match this signal — for
/// those, the library signal at [`ROS2_LIBRARY_MARKERS`] is
/// load-bearing.
///
/// Today this list is just `ROS_DOMAIN_ID`; other runner-exported
/// vars can be added here if a future RMW or distro emits something
/// that fires only at runner-init time and never at shell-source
/// time.
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
///
/// v1.0.2 B-NEW-16 (Inspector #5) — dropped `"ros2 topic"`,
/// `"ros2 service"`, `"ros2 node"`. These are introspection CLIs,
/// not node-spawning commands, and they conflated short-lived
/// shell invocations (1-5 s `ros2 topic hz` polls during debug
/// sessions) with actual ROS2 graph participants. The operator's
/// RunStore was carrying 55 transient `"ros2"` records that
/// flooded the workloads panel and the activity feed. Kept:
/// `"ros2 run"`, `"ros2 launch"` (these DO spawn a node),
/// `"rclcpp_component_container"`, `"rclpy"`.
const ROS2_CMDLINE_MARKERS: &[&str] = &[
    "ros2 run",
    "ros2 launch",
    "rclcpp_component_container",
    "rclpy",
];

/// v1.1.4 BUG-P5-1 — read-only `ros2` CLI introspection invocations
/// and the daemon helper. These are short-lived query commands (or a
/// background helper), NOT ROS2 graph participants the operator wants
/// on the workloads panel. The `ros2` CLI is itself a Python process
/// that imports `rclpy` and therefore loads `librcl.so`, so the
/// library signal ([`ROS2_LIBRARY_MARKERS`]) over-classifies them as
/// ROS2 workloads (visible as transient `ros2` rows in the operator's
/// screenshot). Guarded out in [`classify`] before any signal check,
/// the same shape as the v1.0.2 tooling-name and shell-wrapper guards.
///
/// Substring-matched (lowercased) against the joined cmdline so the
/// `python3 /opt/ros/humble/bin/ros2 topic hz …` shape matches the
/// same way a bare `ros2 topic hz …` does. Node-SPAWNING verbs
/// (`ros2 run` / `ros2 launch`) and traffic-GENERATING verbs
/// (`ros2 topic pub`, `ros2 service call`) are deliberately NOT here
/// — those represent real ROS2 activity and keep classifying.
///
/// Also catches B3's own `ros2 topic hz` sampler probe (the
/// self-classification feedback the EDGE_MONITOR_SAMPLER env marker
/// guards against from the other direction).
const ROS2_CLI_INTROSPECTION_MARKERS: &[&str] = &[
    "ros2 topic hz",
    "ros2 topic list",
    "ros2 topic echo",
    "ros2 topic info",
    "ros2 topic bw",
    "ros2 topic find",
    "ros2 node list",
    "ros2 node info",
    "ros2 service list",
    "ros2 service type",
    "ros2 service find",
    "ros2 action list",
    "ros2 action info",
    "ros2 param ",
    "ros2 interface ",
    "ros2 pkg ",
    "ros2 doctor",
    "ros2 wtf",
    // The ROS2 daemon helper — a long-lived Python process that
    // links librcl but is infrastructure, not a workload.
    "_ros2_daemon",
];

/// Library basenames that signal ROS2 linkage. Looked up as
/// substrings of `/proc/<pid>/maps` lines so the absolute install
/// path doesn't matter.
///
/// v1.0.3 B-EMPIRICAL-4 — dropped the fictional `librclpy.so`
/// (rclpy on Humble is a Python package + `_rclpy_pybind11`
/// C-extension; no such library file exists). Added `librcl.so`
/// (the canonical underlying library every ROS2 process loads —
/// closes DESIGN_HANDOFF.md L9 spec drift at lines 128/335/1080),
/// `librmw_implementation.so` (the RMW-discovery shim every ROS2
/// process loads at runtime), and `_rclpy_pybind11` (the
/// C-extension Python rclpy actually links). Kept `librclcpp.so`
/// (C++ nodes), `libfastdds.so` / `libfastrtps.so` (Fast-DDS
/// RMW backends).
const ROS2_LIBRARY_MARKERS: &[&str] = &[
    // Canonical underlying lib — loaded by every ROS2 process,
    // every distro, every RMW backend.
    "librcl.so",
    // C++ rclcpp linkage.
    "librclcpp.so",
    // RMW shim — every ROS2 process loads this to discover the
    // RMW plugin at runtime. Belt-and-braces against unusual
    // distro layouts that don't surface librcl.so on the maps
    // line we expect.
    "librmw_implementation.so",
    // Python rclpy via C-extension — the actual library Python
    // rclpy nodes load (not the fictional `librclpy.so` the
    // pre-v1.0.3 marker named, which does not exist on Humble).
    "_rclpy_pybind11",
    // Fast-DDS RMW backends — Humble + Fast-DDS hosts. Cyclone
    // DDS hosts are caught by librcl.so / librmw_implementation.so
    // above; no Cyclone-specific lib is needed in this list.
    "libfastdds.so",
    "libfastrtps.so",
];

/// v1.0.2 (Inspector #5) — process-name blacklist. These are
/// ROS2 *tooling* (visualisers, build / lint, GUIs), not ROS2
/// graph participants. They link the rcl libraries and inherit
/// the setup.bash env, so without this guard they'd false-fire
/// the library / cmdline signals.
///
/// Just as important: Phase 2 will run `ros2 topic hz <topic>` as
/// a Hz sampler shellout. Without this list (and the bash-`-c`
/// guard below) edge_monitor would classify its own sampler
/// probes as ROS2 nodes, creating a feedback loop. Matching is
/// case-insensitive prefix on `ProcessSample::name`; the entries
/// are all ≤15 chars so the kernel's `TASK_COMM_LEN=16`
/// truncation of `/proc/<pid>/comm` doesn't lose any of them.
const ROS2_TOOLING_NAMES: &[&str] = &[
    "rviz",
    "rviz2",
    "rqt_graph",
    "rqt_plot",
    "rqt_console",
    "rqt_logger_level",
    "rqt_image_view",
    "rqt_reconfigure",
    "rqt",
    "colcon",
    "ament_lint",
    "ament_cpplint",
    "ament_flake8",
    "ament_pep257",
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
    // v1.0.2 (Inspector #5) — tooling-name and shell-wrapper
    // guards. Both run before any signal check: the goal is "we
    // know this process is NOT a ROS2 node even though some of
    // the downstream signals would falsely fire on it".
    if is_ros2_tooling_name(&sample.name) {
        return None;
    }
    if is_shell_wrapped_ros2_invocation(&sample.cmdline) {
        return None;
    }
    // v1.1.4 BUG-P5-1 — read-only `ros2` CLI queries + the daemon
    // helper load librcl (the CLI imports rclpy) and would otherwise
    // fire the library signal below. Guard them out before any signal
    // check.
    if is_ros2_cli_introspection(&sample.cmdline) {
        return None;
    }
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

/// v1.0.2 — case-insensitive prefix match against
/// [`ROS2_TOOLING_NAMES`]. Prefix (not exact) so the kernel's
/// `TASK_COMM_LEN=16` truncation of `/proc/<pid>/comm` still
/// matches the canonical short name on the longer real
/// executable.
pub(crate) fn is_ros2_tooling_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    ROS2_TOOLING_NAMES
        .iter()
        .any(|tool| lowered.starts_with(tool))
}

/// v1.0.2 — `bash -c "ros2 …"` / `sh -c "ros2 …"` and the
/// `/bin/...` variants are NOT ROS2 nodes. The process is the
/// shell wrapper; the embedded `ros2 …` text in argv would
/// otherwise falsely match the cmdline signal. Phase 2's Hz
/// sampler will shell out this exact shape, so the guard also
/// breaks the self-classification feedback loop the sampler
/// would otherwise create.
pub(crate) fn is_shell_wrapped_ros2_invocation(cmdline: &[String]) -> bool {
    let Some(first) = cmdline.first() else {
        return false;
    };
    // Match basename so `/bin/bash` and `/usr/bin/sh` are caught too.
    let basename = first.rsplit('/').next().unwrap_or(first);
    if !matches!(basename, "bash" | "sh") {
        return false;
    }
    let Some(dash_c_idx) = cmdline.iter().position(|a| a == "-c") else {
        return false;
    };
    // The "-c" argument must be followed by at least one shell-
    // command argument. Look for "ros2" in any argv element AFTER
    // the -c so a `bash -c …` that doesn't actually contain ros2
    // text doesn't trip the guard.
    cmdline
        .iter()
        .skip(dash_c_idx + 1)
        .any(|arg| arg.to_ascii_lowercase().contains("ros2"))
}

/// v1.1.4 BUG-P5-1 — true when the cmdline is a read-only `ros2` CLI
/// introspection query or the `_ros2_daemon` helper. Substring match
/// (lowercased) against the joined cmdline; see
/// [`ROS2_CLI_INTROSPECTION_MARKERS`] for the rationale and the
/// deliberate exclusions (node-spawning + traffic-generating verbs).
pub(crate) fn is_ros2_cli_introspection(cmdline: &[String]) -> bool {
    if cmdline.is_empty() {
        return false;
    }
    let joined = cmdline.join(" ").to_ascii_lowercase();
    ROS2_CLI_INTROSPECTION_MARKERS
        .iter()
        .any(|m| joined.contains(m))
}

fn make_ros2_result(evidence: String) -> ClassificationResult {
    ClassificationResult::ai(AICategory::Inference, WorkloadCategory::ROS2, evidence)
}

/// Fix-1 / v1.0.3 B-EMPIRICAL-4 — returns the matched env var name
/// when a *runner-exported* ROS2 env var is present with a non-empty
/// value. Currently just `ROS_DOMAIN_ID` — exported by `ros2 launch`
/// and `ros2 run`. Bare `rclpy.init()` / `rclcpp::init()` calls READ
/// this env var but do not export it, so its presence is strong
/// evidence of a runner-spawned (or explicitly-exported) process,
/// but its absence does NOT rule out a real ROS2 process — see
/// [`ROS2_LIBRARY_MARKERS`] for the load-bearing fallback.
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

    /// v1.0.3 B-EMPIRICAL-4 — replaces the pre-v1.0.3
    /// `rclpy_in_maps_signals_ros2` test that asserted on the
    /// fictional `librclpy.so` (no such file ships on Humble).
    /// Python rclpy actually links the
    /// `_rclpy_pybind11.cpython-<abi>-<arch>.so` C-extension.
    #[test]
    fn _rclpy_pybind11_signals_ros2() {
        let maps = "\
7f1234567000-7f1234600000 r-xp 00000000 00:00 0 /opt/ros/humble/local/lib/python3.10/dist-packages/rclpy/_rclpy_pybind11.cpython-310-x86_64-linux-gnu.so
7f1234600000-7f1234700000 rw-p 00000000 00:00 0 [heap]
";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            Some("_rclpy_pybind11")
        );
    }

    /// v1.0.3 B-EMPIRICAL-4 — every ROS2 process loads
    /// `librcl.so` regardless of distro / RMW / language.
    /// Closes DESIGN_HANDOFF.md L9 spec drift.
    #[test]
    fn librcl_so_alone_signals_ros2() {
        let maps = "\
7f1000000000-7f1000100000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librcl.so.5.3.0
";
        assert_eq!(ros2_library_in_maps_text(maps), Some("librcl.so"));
    }

    /// v1.0.3 B-EMPIRICAL-4 — the RMW-discovery shim is also
    /// universally linked; belt-and-braces against odd distro
    /// layouts where librcl.so wouldn't surface on the maps
    /// line we expect.
    #[test]
    fn librmw_implementation_so_signals_ros2() {
        let maps = "\
7f1000000000-7f1000100000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librmw_implementation.so.5.3.6
";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            Some("librmw_implementation.so")
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

    /// v1.0.3 B-EMPIRICAL-4 — realistic `/proc/<pid>/maps` snippet
    /// for a Python rclpy node on Humble + Cyclone DDS (the operator's
    /// default; Tester-A confirmed this case). The libs enumerated
    /// here are what Tester-A observed in their evidence directory
    /// (`tests/empirical/v1_0_2/tester_a/evidence/positive_with_domain_id_091415/`)
    /// for a `python3 my_node.py` invocation. None of these are the
    /// fictional `librclpy.so` the pre-v1.0.3 marker looked for, so
    /// the new `librcl.so` / `librmw_implementation.so` /
    /// `_rclpy_pybind11` markers are load-bearing for this case.
    #[test]
    fn realistic_humble_cyclone_dds_maps_signals_ros2() {
        let maps = "\
7f1000000000-7f1000100000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librcl.so.5.3.0
7f1000100000-7f1000200000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librcl_action.so.5.3.0
7f1000200000-7f1000300000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librcl_lifecycle.so.5.3.0
7f1000300000-7f1000400000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librcl_yaml_param_parser.so.5.3.0
7f1000400000-7f1000500000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librmw.so.6.1.2
7f1000500000-7f1000600000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librmw_implementation.so.5.3.6
7f1000600000-7f1000700000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librcutils.so.5.1.5
7f1000700000-7f1000800000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/librmw_cyclonedds_cpp.so
7f1000800000-7f1000900000 r-xp 00000000 00:00 0 /opt/ros/humble/lib/libddsc.so.0.10.5
7f1000900000-7f1000a00000 r-xp 00000000 00:00 0 /opt/ros/humble/local/lib/python3.10/dist-packages/rclpy/_rclpy_pybind11.cpython-310-x86_64-linux-gnu.so
";
        // librcl.so fires first per the marker order; if it ever
        // moves position the test should still classify the
        // process as ROS2, so assert via the signal helper rather
        // than the exact marker string.
        assert!(
            ros2_library_in_maps_text(maps).is_some(),
            "realistic Humble + Cyclone DDS rclpy maps must signal ROS2",
        );
    }

    /// v1.0.3 B-EMPIRICAL-4 — negative. A non-ROS2 Python process
    /// whose imports happen to bring in `rcl_interfaces` (the Python
    /// package, not the library) should NOT classify as ROS2. The
    /// substring "rcl" appears in the path but no `librcl.so` SO
    /// is mapped — the markers are .so-anchored.
    #[test]
    fn process_with_rcl_substring_but_no_librcl_so_returns_none() {
        let maps = "\
7f1000000000-7f1000100000 r-xp 00000000 00:00 0 /usr/lib/python3/dist-packages/rcl_interfaces/__init__.py
7f1000100000-7f1000200000 rw-p 00000000 00:00 0 [heap]
";
        assert_eq!(
            ros2_library_in_maps_text(maps),
            None,
            "bare `rcl` substring in a Python path must not fire \
             the library signal — the markers are .so-anchored",
        );
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

    // ────────────────────────────────────────────────────────────
    // v1.0.2 B-NEW-16 — introspection-CLI cmdline markers must
    // NOT classify as ROS2. These shell out for a second or two
    // and were polluting the workloads panel + activity feed.
    // ────────────────────────────────────────────────────────────

    #[test]
    fn ros2_topic_list_does_not_classify_as_ros2() {
        let s = sample("ros2", &["ros2", "topic", "list"]);
        assert!(
            classify(&s).is_none(),
            "B-NEW-16 — `ros2 topic` is introspection, not a node",
        );
    }

    #[test]
    fn ros2_service_list_does_not_classify_as_ros2() {
        let s = sample("ros2", &["ros2", "service", "list"]);
        assert!(
            classify(&s).is_none(),
            "B-NEW-16 — `ros2 service` is introspection, not a node",
        );
    }

    #[test]
    fn ros2_node_list_does_not_classify_as_ros2() {
        let s = sample("ros2", &["ros2", "node", "list"]);
        assert!(
            classify(&s).is_none(),
            "B-NEW-16 — `ros2 node` is introspection, not a node",
        );
    }

    /// Positive guard: dropping the introspection markers must NOT
    /// break detection of real node-spawning commands.
    #[test]
    fn ros2_run_my_node_still_classifies_as_ros2() {
        let s = sample("ros2", &["ros2", "run", "demo_nodes_cpp", "talker"]);
        let result = classify(&s).expect("ros2 run must still classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
    }

    /// Positive guard for the other surviving node-spawn verb.
    #[test]
    fn ros2_launch_still_classifies_as_ros2() {
        let s = sample(
            "ros2",
            &["ros2", "launch", "my_pkg", "system.launch.py"],
        );
        let result = classify(&s).expect("ros2 launch must still classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
    }

    // ────────────────────────────────────────────────────────────
    // v1.0.2 (Inspector #5) — sampler self-classification guards.
    // Tooling names AND `bash -c "ros2 …"` shell wrappers must
    // NOT classify as ROS2, or else Phase 2's Hz sampler will
    // treat its own probes as new ROS2 nodes.
    // ────────────────────────────────────────────────────────────

    #[test]
    fn rviz2_does_not_classify_as_ros2() {
        // rviz2 links the rcl libraries (via the rviz_common
        // packages) so the library signal WOULD fire on it
        // without the tooling-name guard. The guard short-circuits
        // before the library read happens.
        let s = sample("rviz2", &["rviz2", "-d", "config.rviz"]);
        assert!(classify(&s).is_none(), "rviz2 is tooling, not a graph node");
    }

    #[test]
    fn rqt_graph_does_not_classify_as_ros2() {
        let s = sample("rqt_graph", &["rqt_graph"]);
        assert!(classify(&s).is_none());
    }

    /// The Phase 2 ROS Hz sampler will exec `bash -c "ros2 topic
    /// hz /chatter"`. Even after B-NEW-16 dropped the `ros2
    /// topic` cmdline marker, the shell-wrapper guard is the
    /// final defence: a future contract amendment that re-adds a
    /// `ros2`-prefixed marker still won't feedback-loop the
    /// sampler.
    #[test]
    fn bash_dash_c_ros2_topic_hz_does_not_classify_as_ros2() {
        let s = sample("bash", &["bash", "-c", "ros2 topic hz /chatter"]);
        assert!(
            classify(&s).is_none(),
            "Phase 2 sampler shellout must not self-classify",
        );
    }

    /// `/bin/bash`/`/usr/bin/sh` basenames also count.
    #[test]
    fn absolute_path_bash_dash_c_ros2_still_guarded() {
        let s = sample("bash", &["/bin/bash", "-c", "ros2 launch x y"]);
        assert!(classify(&s).is_none());
    }

    /// Negative: `bash -c "…"` with no "ros2" text in the command
    /// must NOT be force-guarded — the guard is intentionally
    /// narrow.
    #[test]
    fn bash_dash_c_non_ros2_does_not_trip_guard() {
        let s = sample("bash", &["bash", "-c", "ls -la"]);
        assert!(
            !is_shell_wrapped_ros2_invocation(&s.cmdline),
            "guard must only fire on ros2-substring -c payloads",
        );
    }

    /// Positive guard: real rclpy-using node must still classify
    /// even after the tooling/shell-wrapper additions.
    #[test]
    fn genuine_ros2_node_with_rclpy_still_classifies() {
        let s = sample(
            "perception_node",
            &["python3", "-m", "rclpy.executors", "perception_node.py"],
        );
        let result = classify(&s).expect("rclpy node must still classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
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

    // ────────────────────────────────────────────────────────────
    // v1.1.4 BUG-P5-1 — `ros2` CLI introspection + daemon must NOT
    // classify as ROS2 workloads. The CLI imports rclpy → loads
    // librcl, so the library signal would otherwise over-classify
    // them (transient `ros2` rows in the operator's screenshot).
    // ────────────────────────────────────────────────────────────

    #[test]
    fn ros2_topic_hz_cli_does_not_classify() {
        // The exact shape B3's own sampler shells out, AND what an
        // operator running `ros2 topic hz` by hand looks like. The
        // real process is python3 running the ros2 CLI entrypoint.
        let s = sample(
            "ros2",
            &[
                "/usr/bin/python3",
                "/opt/ros/humble/bin/ros2",
                "topic",
                "hz",
                "/chatter",
            ],
        );
        assert!(
            classify(&s).is_none(),
            "ros2 topic hz is a read-only CLI query, not a ROS2 workload",
        );
    }

    #[test]
    fn ros2_node_list_cli_does_not_classify() {
        let s = sample("ros2", &["ros2", "node", "list"]);
        assert!(classify(&s).is_none());
    }

    #[test]
    fn ros2_daemon_helper_does_not_classify() {
        let s = sample(
            "_ros2_daemon",
            &["/usr/bin/python3", "/opt/ros/humble/bin/_ros2_daemon"],
        );
        assert!(
            classify(&s).is_none(),
            "_ros2_daemon is infrastructure, not a workload",
        );
    }

    #[test]
    fn ros2_run_still_classifies_after_introspection_guard() {
        // Node-spawning verb must keep classifying — the introspection
        // guard must not catch `ros2 run`.
        let s = sample("ros2", &["ros2", "run", "demo_nodes_cpp", "talker"]);
        let result = classify(&s).expect("ros2 run must still classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
    }

    #[test]
    fn ros2_topic_pub_still_classifies_after_introspection_guard() {
        // Traffic-generating verb is NOT introspection — `ros2 topic
        // pub` actively publishes, so it should keep classifying via
        // the library/env signals (it's deliberately absent from the
        // introspection marker list). Here it carries ROS_DOMAIN_ID
        // so it classifies via the runtime env signal.
        let s = sample_with_env(
            "ros2",
            &["ros2", "topic", "pub", "/chatter", "std_msgs/String"],
            &[("ROS_DOMAIN_ID", "0")],
        );
        let result = classify(&s).expect("ros2 topic pub must still classify");
        assert_eq!(result.workload_category, WorkloadCategory::ROS2);
    }
}
