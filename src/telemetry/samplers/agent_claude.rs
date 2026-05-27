//! v1.1.0 B2 — Agent (claude) activity sampler.
//!
//! Built per Inspector #12 Option (i) + operator Q3: uses the
//! `TelemetrySource::sample_with_context` trait extension added by
//! Phase-2 foundation so the sampler can inspect the full per-tick
//! process list (needed to detect Bash-tool subprocesses spawned by
//! a claude agent — the activity signal).
//!
//! Empirical anchor: Tester-A's capture at
//! `tests/empirical/v1_1_0_prep/claude_agent_format/` (11 raw files
//! plus environment metadata). All locked decisions cite that
//! capture in the source comments below.
//!
//! ## Detection (CHANGES 1, 2, 5)
//!
//! `applies_to` is **two-factor + two-reject**:
//!
//! 1. `basename(argv[0]) == "claude"` — argv\[0\]-only classification
//!    (no `comm`, no `/proc/exe`). The claude binary is a multi-call
//!    binary: Tester-A's capture verified 1 of 22 processes whose
//!    `/proc/<pid>/exe` resolved to claude had `argv[0]=ugrep`,
//!    running as a grep replacement for another agent's Bash tool.
//!    Only `argv[0]` is discriminative — same recursive-observation
//!    discipline as B-NEW-16's bash-c-ros2 wrapper guard, different
//!    mechanism, same shape.
//! 2. argv contains the two-token sequence `--output-format`
//!    `stream-json` (Tester-A's `task1_unique_cmdline_shapes.txt`
//!    confirms the two-token form; STOP-AND-SURFACE check #3
//!    confirmed pre-implementation).
//! 3. **Reject** any process whose argv contains
//!    `.claude/shell-snapshots/` — Bash-tool subshells source
//!    `~/.claude/shell-snapshots/<hash>.sh` and would otherwise
//!    false-fire `pgrep claude`.
//!
//! ## Activity signal (CHANGE 4)
//!
//! `sample_with_context` scans `all_procs` for processes where
//! `child.ppid == agent_pid` and the basename of `cmdline[0]` is
//! `bash`. The `ppid` field on `ProcessSnapshot` was added by
//! DISPATCH 1.6 specifically to enable this multi-instance
//! attribution (Tester-A's capture: 22 concurrent agents must
//! NOT each be credited with every bash in the snapshot).
//!
//! Presence-not-count semantics: one bash child is enough; LWPs
//! of a multi-threaded bash child (theoretical, bash is
//! single-thread in practice) collapse to the same Active signal.
//!
//! LWP-filter rationale: claude's own LWPs (Node + libuv pool,
//! 16-27 per agent per Tester-A) are visible at `/proc` top-level
//! on this kernel (verified during STOP-AND-SURFACE check #1),
//! BUT they carry `ppid = parent_of_TGL`, not `ppid = TGL`. So the
//! `child.ppid == agent_pid` filter excludes claude's own LWPs
//! naturally — no explicit Tgid==Pid filter required.
//!
//! ## Idle window (CHANGE 3)
//!
//! `AGENT_IDLE_WINDOW = 60s` — PROVISIONAL: refined post-v1.1.0
//! sampler validation (v1.1.1). Tester-A empirical basis:
//! Bash-tool children persist 1-10 s during tool invocation;
//! 60 s catches sustained inactivity while tolerating
//! thinking-pauses between tool calls. 10 s (the original
//! dispatch placeholder) was too aggressive — agents often pause
//! >10 s while reasoning.
//!
//! ## Multi-instance handling (CHANGE 6)
//!
//! Tester-A observed 22 concurrent agent processes (VS Code
//! extension, 2 distinct extensionHost parents). Each agent is its
//! own workload — the dispatcher invokes `sample_with_context` once
//! per (source, applicable PID) per tick; per-PID state lives in
//! `last_active_at` keyed by PID. No global / cross-instance state.
//! Combined with DISPATCH 1.6's ppid plumbing, agent A's bash
//! children are not attributed to agent B.
//!
//! ## Per-PID state lifecycle (v1.1.1 candidate)
//!
//! The dispatcher does NOT invoke a per-PID cleanup hook on
//! `SourceError::Permanent` (verified during STOP-AND-SURFACE
//! check #4). `last_active_at` therefore accumulates entries until
//! the source struct is dropped. Bounded leak: ~50 bytes per
//! observed claude PID; 22 concurrent empirically; not a memory
//! crisis. PROVISIONAL: refined post-v1.1.0 sampler validation
//! (v1.1.1) — add a dispatcher cleanup hook or in-source GC pass.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::telemetry::source::{
    ActivityState, ProcessSnapshot, SourceResult, TelemetryFrame, TelemetrySource,
};

// PROVISIONAL: refined post-v1.1.0 sampler validation (v1.1.1).
// EMPIRICAL basis (Tester-A): Bash-tool children persist 1-10 s
// during tool invocation. 60 s catches sustained inactivity while
// tolerating thinking-pauses between tool calls.
const AGENT_IDLE_WINDOW: Duration = Duration::from_secs(60);

// STOP-AND-SURFACE check #3 confirmed Tester-A's capture uses the
// two-token form `--output-format stream-json` (not the
// `--output-format=stream-json` single-argv form). Two-token scan
// is primary; single-argv form is accepted defensively in case
// a future claude version adopts it.
const STREAM_JSON_FLAG: &str = "--output-format";
const STREAM_JSON_VALUE: &str = "stream-json";
const STREAM_JSON_SINGLE: &str = "--output-format=stream-json";

// CHANGE 2: argv signature of a Bash-tool subshell sourcing one of
// `~/.claude/shell-snapshots/<hash>.sh`. Subshells false-fire
// `pgrep claude` but are not agent processes; reject explicitly.
const BASH_SHELL_SNAPSHOT_MARKER: &str = ".claude/shell-snapshots/";

pub struct AgentClaudeSource {
    /// claude PID → last tick the agent had a qualifying bash child.
    /// Idle is emitted only when `now - last_active_at >= AGENT_IDLE_WINDOW`.
    /// Keyed by PID rather than model name: agent PIDs are stable for
    /// the lifetime of one Claude Code session.
    last_active_at: HashMap<u32, Instant>,
}

impl AgentClaudeSource {
    pub fn new() -> Self {
        Self {
            last_active_at: HashMap::new(),
        }
    }
}

impl Default for AgentClaudeSource {
    fn default() -> Self {
        Self::new()
    }
}

/// CHANGE 1 / CHANGE 5 — `argv[0]` basename match. The claude
/// binary is a multi-call binary; only `argv[0]` is discriminative.
/// Never reach for `name` (sourced from `/proc/<pid>/comm`,
/// TASK_COMM_LEN-truncated and can be set by the process itself)
/// or `/proc/exe` (resolves to the same path for ugrep-symlinked
/// invocations).
fn is_claude_argv0(cmdline: &[String]) -> bool {
    let Some(argv0) = cmdline.first() else {
        return false;
    };
    Path::new(argv0)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|base| base == "claude")
        .unwrap_or(false)
}

/// CHANGE 3 — accept either the two-token form
/// (`--output-format` `stream-json`, observed in Tester-A's capture)
/// or the single-argv `--output-format=stream-json` form
/// (defensive future-proofing for a possible claude argv change).
fn has_stream_json_flag(cmdline: &[String]) -> bool {
    let positions = cmdline.iter().enumerate();
    for (i, arg) in positions {
        if arg == STREAM_JSON_SINGLE {
            return true;
        }
        if arg == STREAM_JSON_FLAG
            && cmdline.get(i + 1).map(|v| v.as_str()) == Some(STREAM_JSON_VALUE)
        {
            return true;
        }
    }
    false
}

/// CHANGE 2 — reject Bash-tool subshell processes whose argv carries
/// the `.claude/shell-snapshots/` substring.
fn is_bash_shell_snapshot(cmdline: &[String]) -> bool {
    cmdline
        .iter()
        .any(|arg| arg.contains(BASH_SHELL_SNAPSHOT_MARKER))
}

/// CHANGE 4 — `cmdline[0]` basename is `bash`. Used to identify the
/// agent's Bash-tool subprocesses among `all_procs`.
fn is_bash_basename(proc: &ProcessSnapshot) -> bool {
    if let Some(argv0) = proc.cmdline.first()
        && let Some(base) = Path::new(argv0).file_name().and_then(|n| n.to_str())
        && base == "bash"
    {
        return true;
    }
    // Fallback: comm-derived `name` when argv is empty (rare —
    // early-exit / kernel-thread shapes). Comm is TASK_COMM_LEN
    // -truncated to 15 chars; "bash" is well within.
    proc.name == "bash"
}

#[async_trait]
impl TelemetrySource for AgentClaudeSource {
    fn name(&self) -> &str {
        "agent-claude"
    }

    fn applies_to(&self, proc: &ProcessSnapshot) -> bool {
        if !is_claude_argv0(&proc.cmdline) {
            return false;
        }
        if is_bash_shell_snapshot(&proc.cmdline) {
            return false;
        }
        has_stream_json_flag(&proc.cmdline)
    }

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        // Single-PID polyfill — without `all_procs` we have no
        // visibility into bash children, so we cannot meaningfully
        // emit Active. Return a no-state frame; the dispatcher
        // always calls `sample_with_context` on the live path, so
        // this branch only fires from direct unit-test callers.
        Ok(TelemetryFrame::new(proc.pid))
    }

    async fn sample_with_context(
        &mut self,
        proc: &ProcessSnapshot,
        _ai_procs: &[ProcessSnapshot],
        all_procs: &[ProcessSnapshot],
    ) -> SourceResult<TelemetryFrame> {
        let agent_pid = proc.pid;
        // CHANGE 4: a "bash child of this agent" is any
        // ProcessSnapshot whose ppid matches agent_pid AND whose
        // argv[0] basename (or comm) is "bash". Without the ppid
        // filter (added by DISPATCH 1.6) we would credit every
        // bash in the snapshot to every agent — broken for the
        // 22-concurrent-agent case CHANGE 6 documents.
        //
        // v1.1.2 (DISPATCH 7) — read `all_procs` (UNFILTERED), NOT
        // `_ai_procs`. bash tool-children are `NotAi`-classified
        // and the runtime strips them from the AI-filtered list
        // before `Dispatcher::tick`. v1.1.1 read the filtered list
        // here, so `has_bash_child` was ALWAYS false and B2 locked
        // to Idle (DISPATCH 6B). The ppid plumbing (DISPATCH 1.6)
        // and the classifier coverage (v1.1.1) were both correct;
        // the consumer was just reading the wrong list.
        let has_bash_child = all_procs
            .iter()
            .any(|child| child.ppid == Some(agent_pid) && is_bash_basename(child));

        let now = Instant::now();
        let activity = if has_bash_child {
            self.last_active_at.insert(agent_pid, now);
            ActivityState::Active
        } else {
            match self.last_active_at.get(&agent_pid) {
                Some(last) if now.saturating_duration_since(*last) < AGENT_IDLE_WINDOW => {
                    // Still inside the 60 s window — agents often
                    // pause >10 s while reasoning between tool
                    // calls. Treat as Active until the window
                    // closes.
                    ActivityState::Active
                }
                _ => ActivityState::Idle,
            }
        };

        Ok(TelemetryFrame {
            pid: agent_pid,
            activity_state: Some(activity),
            ..TelemetryFrame::new(agent_pid)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdMap;

    fn snap_with_ppid(
        pid: u32,
        ppid: Option<u32>,
        name: &str,
        cmdline: &[&str],
    ) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            environ: StdMap::new(),
            model_name: None,
            cpu_pct: 0.0,
            ppid,
        }
    }

    fn snap(pid: u32, name: &str, cmdline: &[&str]) -> ProcessSnapshot {
        snap_with_ppid(pid, None, name, cmdline)
    }

    /// Test 1 — claude with the two-token `--output-format stream-json`
    /// flag → MATCH (the realistic case captured by Tester-A).
    #[test]
    fn applies_to_claude_with_stream_json_flag_matches() {
        let s = AgentClaudeSource::new();
        let p = snap(
            100,
            "claude",
            &[
                "/home/u/.vscode-server/extensions/anthropic.claude-code/native-binary/claude",
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
            ],
        );
        assert!(s.applies_to(&p));
    }

    /// Test 1b — defensive: accept `--output-format=stream-json`
    /// single-argv form too in case a future claude version adopts it.
    #[test]
    fn applies_to_claude_with_equals_form_also_matches() {
        let s = AgentClaudeSource::new();
        let p = snap(
            100,
            "claude",
            &["/usr/local/bin/claude", "--output-format=stream-json"],
        );
        assert!(s.applies_to(&p));
    }

    /// Test 2 — `claude --help` or any invocation without the
    /// `--output-format stream-json` flag → NO MATCH.
    #[test]
    fn applies_to_claude_without_stream_json_flag_no_match() {
        let s = AgentClaudeSource::new();
        let p = snap(100, "claude", &["claude", "--help"]);
        assert!(!s.applies_to(&p));
        let p = snap(101, "claude", &["claude", "--output-format", "text"]);
        assert!(!s.applies_to(&p));
    }

    /// Test 3 (CHANGE 1 / CHANGE 5) — ugrep symlinked through the
    /// claude multi-call binary (argv\[0\]=ugrep) must NOT match,
    /// even though `/proc/exe` would resolve to the same binary as
    /// genuine claude invocations. Recursive-observation guard:
    /// only argv\[0\] discriminates.
    #[test]
    fn applies_to_ugrep_symlinked_through_claude_no_match() {
        let s = AgentClaudeSource::new();
        let p = snap(
            200,
            "ugrep",
            &["ugrep", "--output-format", "stream-json", "-rn", "pattern"],
        );
        assert!(
            !s.applies_to(&p),
            "ugrep symlinked through claude binary must NOT classify \
             as a claude agent — only argv[0] is discriminative",
        );
    }

    /// Test 4 (CHANGE 2) — Bash-tool subshell sourcing
    /// `~/.claude/shell-snapshots/<hash>.sh` is NOT an agent process.
    #[test]
    fn applies_to_bash_shell_snapshot_no_match() {
        let s = AgentClaudeSource::new();
        let p = snap(
            300,
            "bash",
            &[
                "bash",
                "-c",
                "source /home/u/.claude/shell-snapshots/abc123.sh; cmd",
            ],
        );
        assert!(!s.applies_to(&p));
    }

    /// Test 5 (CHANGE 5 hard rule) — name=="claude" but
    /// argv\[0\]!="claude" must NOT match. Pins the
    /// argv\[0\]-only classification discipline.
    #[test]
    fn applies_to_name_claude_but_argv0_different_no_match() {
        let s = AgentClaudeSource::new();
        // name (sourced from /proc/<pid>/comm) says claude, but
        // argv[0] is something else (e.g. a script that set comm).
        let p = snap(
            400,
            "claude",
            &["/usr/bin/python3", "--output-format", "stream-json"],
        );
        assert!(!s.applies_to(&p));
    }

    /// Test 6 — agent with 1 bash child whose ppid points back to
    /// the agent → Active.
    #[tokio::test]
    async fn sample_with_context_agent_with_bash_child_emits_active() {
        let mut s = AgentClaudeSource::new();
        let agent = snap(
            100,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        let bash_child = snap_with_ppid(101, Some(100), "bash", &["bash", "-c", "ls -la"]);
        let all = vec![agent.clone(), bash_child];
        let frame = s
            .sample_with_context(&agent, &all, &all)
            .await
            .expect("sample should succeed");
        assert_eq!(frame.activity_state, Some(ActivityState::Active));
        assert!(s.last_active_at.contains_key(&100));
    }

    /// Test 7 (CHANGE 4) — agent with many LWPs but no bash
    /// children → Idle. LWPs of claude carry `ppid =
    /// parent_of_TGL` (verified empirically during
    /// STOP-AND-SURFACE check #1), so they're naturally excluded
    /// by the `ppid == agent_pid` filter.
    #[tokio::test]
    async fn sample_with_context_lwps_only_no_bash_children_emits_idle() {
        let mut s = AgentClaudeSource::new();
        let agent_pid = 100;
        let agent = snap(
            agent_pid,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        // 27 "LWP" entries shaped as node threads, each carrying
        // ppid = parent_of_agent (NOT agent_pid). This mirrors
        // the kernel behaviour STOP-AND-SURFACE check #1
        // verified.
        let parent_of_agent = 50;
        let lwps: Vec<ProcessSnapshot> = (0..27)
            .map(|i| snap_with_ppid(200 + i, Some(parent_of_agent), "node", &["node", "--worker"]))
            .collect();
        let mut all = vec![agent.clone()];
        all.extend(lwps);
        let frame = s
            .sample_with_context(&agent, &all, &all)
            .await
            .expect("sample should succeed");
        assert_eq!(
            frame.activity_state,
            Some(ActivityState::Idle),
            "LWPs must not count as activity signal — they carry \
             ppid = parent_of_agent, not ppid = agent_pid",
        );
    }

    /// Test 8 — idle window (60s). With no bash children and no
    /// prior activity, emit Idle. With prior activity inside the
    /// 60 s window, emit Active. After the window expires, emit Idle.
    #[tokio::test]
    async fn sample_with_context_idle_window_60s() {
        let mut s = AgentClaudeSource::new();
        let agent = snap(
            100,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        // Inside the window (just observed Active).
        s.last_active_at.insert(100, Instant::now());
        let all = vec![agent.clone()];
        let frame = s
            .sample_with_context(&agent, &all, &all)
            .await
            .expect("sample should succeed");
        assert_eq!(
            frame.activity_state,
            Some(ActivityState::Active),
            "within 60 s of last bash child, emit Active (thinking-pause tolerance)",
        );

        // Outside the window — backdate last_active_at.
        let stale = Instant::now() - (AGENT_IDLE_WINDOW + Duration::from_secs(1));
        s.last_active_at.insert(100, stale);
        let frame = s
            .sample_with_context(&agent, &all, &all)
            .await
            .expect("sample should succeed");
        assert_eq!(frame.activity_state, Some(ActivityState::Idle));
    }

    /// Test 9 — cold start (no prior activity, no bash children) → Idle.
    /// Pins behaviour for the first tick of a freshly-observed agent.
    #[tokio::test]
    async fn sample_with_context_cold_start_no_children_emits_idle() {
        let mut s = AgentClaudeSource::new();
        let agent = snap(
            100,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        let all = vec![agent.clone()];
        let frame = s
            .sample_with_context(&agent, &all, &all)
            .await
            .expect("sample should succeed");
        assert_eq!(frame.activity_state, Some(ActivityState::Idle));
        // Cold-start sample doesn't seed last_active_at because no
        // bash child was found.
        assert!(!s.last_active_at.contains_key(&100));
    }

    /// Test 10 (CHANGE 6) — two concurrent claude agents with
    /// per-PID state isolation. Only agent A has a bash child;
    /// agent B (no bash child) must emit Idle independently.
    /// Critical: without DISPATCH 1.6's ppid plumbing, the bash
    /// child would be attributed to BOTH agents — the bug that
    /// triggered the foundation extension.
    #[tokio::test]
    async fn sample_with_context_two_concurrent_agents_independent_state() {
        let mut s = AgentClaudeSource::new();
        let agent_a_pid = 100;
        let agent_b_pid = 200;
        let agent_a = snap(
            agent_a_pid,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        let agent_b = snap(
            agent_b_pid,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        // Only agent A has a bash child (ppid → agent_a_pid).
        let bash_a = snap_with_ppid(101, Some(agent_a_pid), "bash", &["bash"]);
        let all = vec![agent_a.clone(), agent_b.clone(), bash_a];

        let frame_a = s
            .sample_with_context(&agent_a, &all, &all)
            .await
            .expect("sample A should succeed");
        let frame_b = s
            .sample_with_context(&agent_b, &all, &all)
            .await
            .expect("sample B should succeed");
        assert_eq!(frame_a.activity_state, Some(ActivityState::Active));
        assert_eq!(
            frame_b.activity_state,
            Some(ActivityState::Idle),
            "agent B has no bash child and no prior activity → Idle. \
             Without ppid filtering (DISPATCH 1.6), agent B would have \
             been spuriously Active by inheriting agent A's bash.",
        );
        assert!(s.last_active_at.contains_key(&agent_a_pid));
        assert!(!s.last_active_at.contains_key(&agent_b_pid));
    }

    /// Constants pin: `AGENT_IDLE_WINDOW` must be 60 s per CHANGE 3.
    /// Casual refactors that reach for the original 10 s placeholder
    /// trip this test first.
    #[test]
    fn agent_idle_window_is_60s() {
        assert_eq!(AGENT_IDLE_WINDOW, Duration::from_secs(60));
    }

    /// v1.1.2 DISPATCH 7 — the regression test for the v1.1.1 B2
    /// active-detection bug (DISPATCH 6B dual-Tester finding).
    ///
    /// ASYMMETRIC FIXTURE (Lesson 25 / asymmetric-fixture
    /// discipline applied to this bug class): `ai_procs` EXCLUDES
    /// the bash tool-child (bash is NotAi and the runtime filters
    /// it out before `Dispatcher::tick`), while `all_procs`
    /// INCLUDES it. The bug was that B2 read the AI-filtered list,
    /// so `has_bash_child` was ALWAYS false → activity locked to
    /// Idle even while the agent was actively running a Bash tool.
    ///
    /// This test would have FAILED on v1.1.1 (the sampler read the
    /// filtered list, found no bash child, emitted Idle) and
    /// PASSES on v1.1.2 (the sampler reads `all_procs`, finds the
    /// bash child, emits Active). If a future refactor points B2
    /// back at `ai_procs`, this test fails loud.
    #[tokio::test]
    async fn sample_with_context_active_via_unfiltered_bash_child() {
        let mut s = AgentClaudeSource::new();
        let agent_pid = 100;
        let agent = snap(
            agent_pid,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        // ai_procs: only the AI-classified agent (matches the
        // runtime's `AICategory != NotAi` filter — bash is NotAi).
        let ai_procs = vec![agent.clone()];
        // all_procs: the UNFILTERED kernel list — includes the
        // bash tool-child whose ppid points back at the agent.
        let bash_child = snap_with_ppid(101, Some(agent_pid), "bash", &["bash", "-c", "grep -rn x"]);
        let all_procs = vec![agent.clone(), bash_child];

        let frame = s
            .sample_with_context(&agent, &ai_procs, &all_procs)
            .await
            .expect("sample should succeed");
        assert_eq!(
            frame.activity_state,
            Some(ActivityState::Active),
            "v1.1.2 fix: B2 must read all_procs (which includes the \
             NotAi bash child), not ai_procs (which the runtime \
             strips bash from). Reading ai_procs was the v1.1.1 bug \
             that locked B2 to Idle (DISPATCH 6B).",
        );
        assert!(s.last_active_at.contains_key(&agent_pid));
    }

    /// v1.1.2 DISPATCH 7 — companion negative: the SAME agent, the
    /// SAME ai_procs, but an all_procs that does NOT contain the
    /// bash child → Idle. Pins that the Active verdict above is
    /// genuinely driven by the bash child's presence in all_procs,
    /// not by some unconditional path.
    #[tokio::test]
    async fn sample_with_context_idle_when_bash_child_absent_from_all_procs() {
        let mut s = AgentClaudeSource::new();
        let agent_pid = 100;
        let agent = snap(
            agent_pid,
            "claude",
            &["claude", "--output-format", "stream-json"],
        );
        let ai_procs = vec![agent.clone()];
        // all_procs has the agent but no bash child this tick.
        let all_procs = vec![agent.clone()];
        let frame = s
            .sample_with_context(&agent, &ai_procs, &all_procs)
            .await
            .expect("sample should succeed");
        assert_eq!(frame.activity_state, Some(ActivityState::Idle));
        assert!(!s.last_active_at.contains_key(&agent_pid));
    }
}
