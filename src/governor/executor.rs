use crate::governor::pid_reuse;
use crate::governor::{GovernorError, GovernorPolicy, GovernorResult, KillAction, PendingKill};
use crate::lifecycle::{LifecycleSnapshot, ProcessLifecycle};
use crate::model::AICategory;
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsFd;
use std::sync::Arc;

/// Executes governor decisions: sends signals and tracks kills.
pub struct GovernorExecutor {
    policy: GovernorPolicy,
    pending_kills: HashMap<u32, PendingKill>,
    /// Timestamps of kills issued in the current rate-limit window. Trimmed
    /// on each evaluate() call so the deque length equals the number of
    /// kills still inside `policy.rate_limit_window_secs`.
    recent_kills: VecDeque<DateTime<Utc>>,
}

impl GovernorExecutor {
    /// Create new executor with policy.
    pub fn new(policy: GovernorPolicy) -> Self {
        Self {
            policy,
            pending_kills: HashMap::new(),
            recent_kills: VecDeque::new(),
        }
    }

    /// Evaluate all processes and determine actions. Mutable because the
    /// rate limiter records each kill intent against the sliding window.
    pub fn evaluate(
        &mut self,
        lifecycle_snapshot: &LifecycleSnapshot,
    ) -> Vec<(u32, KillAction, String)> {
        self.trim_rate_limit_window();
        let mut decisions = Vec::with_capacity(lifecycle_snapshot.processes.len());
        for (pid, lifecycle) in &lifecycle_snapshot.processes {
            let (action, reason) = self.evaluate_process(lifecycle);
            // Record enforced kills against the window so subsequent
            // candidates in the same tick see the budget drop.
            if matches!(action, KillAction::SignalTermSent) {
                self.recent_kills.push_back(Utc::now());
            }
            decisions.push((*pid, action, reason));
        }
        decisions
    }

    /// Evaluate a single process. Mutable for symmetry with `evaluate`, but
    /// callers at the single-process layer usually want the sliding window
    /// frozen at a known state — call `trim_rate_limit_window` first.
    fn evaluate_process(&self, lifecycle: &ProcessLifecycle) -> (KillAction, String) {
        // Already exited: nothing to do
        if lifecycle.is_exited() {
            return (
                KillAction::AlreadyExited,
                "process already exited".to_string(),
            );
        }

        // Check policy
        let category = lifecycle.category;
        let action = self.policy.evaluate(&lifecycle.name, category);

        match action {
            crate::governor::policy::PolicyAction::Allow => (
                KillAction::Whitelisted,
                format!("allowed by policy ({})", lifecycle.name),
            ),
            crate::governor::policy::PolicyAction::Kill => {
                if self.rate_limit_exceeded() {
                    (
                        KillAction::RateLimited,
                        format!(
                            "rate limit: {} kills in {}s window — deferring {}",
                            self.policy.rate_limit_max_kills,
                            self.policy.rate_limit_window_secs,
                            lifecycle.name,
                        ),
                    )
                } else {
                    (
                        KillAction::SignalTermSent,
                        format!(
                            "AI process marked for kill: {:?}",
                            category.unwrap_or(AICategory::NotAi)
                        ),
                    )
                }
            }
        }
    }

    /// Drops kill timestamps that have aged out of the rate-limit window.
    fn trim_rate_limit_window(&mut self) {
        let window = Duration::seconds(self.policy.rate_limit_window_secs as i64);
        let cutoff = Utc::now() - window;
        while let Some(front) = self.recent_kills.front() {
            if *front < cutoff {
                self.recent_kills.pop_front();
            } else {
                break;
            }
        }
    }

    /// Current rate-limit usage. True when the next kill would exceed the
    /// per-window budget. Disabled (always false) if max is 0.
    fn rate_limit_exceeded(&self) -> bool {
        let max = self.policy.rate_limit_max_kills;
        if max == 0 {
            return false;
        }
        self.recent_kills.len() as u32 >= max
    }

    /// Exposes the kill-budget remaining in the current window. Meant for
    /// UI surface ("3/3 kills left") and tests.
    pub fn kills_remaining_in_window(&mut self) -> u32 {
        self.trim_rate_limit_window();
        let used = self.recent_kills.len() as u32;
        self.policy.rate_limit_max_kills.saturating_sub(used)
    }

    /// Send SIGTERM to process.
    ///
    /// Captures a Linux pidfd and `/proc/<pid>/stat` starttime *before*
    /// signalling so the SIGKILL escalation can verify the PID has not
    /// been reused during the grace period (TEST.md G.1.11).
    pub fn send_sigterm(
        &mut self,
        pid: u32,
        name: String,
        category: AICategory,
    ) -> GovernorResult<()> {
        // Capture identity tokens BEFORE the signal — between the open and
        // the kill there is still a tiny window, but anything else (open
        // after kill) would be useless. pidfd_open + read of /proc/.../stat
        // are both O(1) syscalls.
        let pidfd = pid_reuse::try_pidfd_open(pid).map(Arc::new);
        let starttime = pid_reuse::read_starttime(pid);

        tracing::info!("sending SIGTERM to PID {}: {}", pid, name);

        // Send SIGTERM
        unsafe {
            if libc::kill(pid as i32, libc::SIGTERM) != 0 {
                return Err(GovernorError::SignalError(format!(
                    "SIGTERM failed for PID {}: errno={}",
                    pid,
                    std::io::Error::last_os_error()
                )));
            }
        }

        // Track this kill, carrying the captured identity for the SIGKILL
        // escalation to verify against.
        let mut pending = PendingKill::new(pid, name, category);
        pending.pidfd = pidfd;
        pending.starttime_ticks = starttime;
        self.pending_kills.insert(pid, pending);

        Ok(())
    }

    /// Convenience wrapper used by `tests/governor_pid_reuse.rs` and any
    /// caller that prefers the policy-aware naming. Equivalent to
    /// `send_sigterm` (same behaviour: enforce-gated, captures identity).
    pub fn request_kill(
        &mut self,
        pid: u32,
        name: String,
        category: AICategory,
    ) -> GovernorResult<()> {
        self.send_sigterm(pid, name, category)
    }

    /// Send SIGKILL to a process whose grace period has expired.
    ///
    /// Returns `KillAction::SignalKillSent` on a successful escalation, or
    /// `KillAction::PidReusedAborted` when the PID-identity check fails —
    /// meaning the original process is gone and either the PID is now
    /// unassigned or the OS has handed it to an unrelated new process. In
    /// the abort case **no signal is sent** (CLAUDE.md safety rule 1).
    pub fn send_sigkill(&mut self, pid: u32, name: &str) -> GovernorResult<KillAction> {
        // Identity verification. Two layers, in priority order:
        //  1. pidfd captured at SIGTERM — kernel-guaranteed race-free.
        //  2. /proc/<pid>/stat starttime re-read and compared.
        // If neither is available the entry is suspicious enough to abort
        // rather than send a stranger SIGKILL.
        let pending = self.pending_kills.get(&pid).cloned();
        let identity_ok = match &pending {
            Some(p) if p.pidfd.is_some() => true, // pidfd path is race-free below
            Some(p) => match (p.starttime_ticks, pid_reuse::read_starttime(pid)) {
                (Some(then), Some(now)) => then == now,
                // Either we couldn't capture at SIGTERM time or the process
                // is gone now — both refuse the SIGKILL.
                _ => false,
            },
            None => {
                // No prior send_sigterm record for this PID. Without a
                // captured identity we can't verify; refuse rather than
                // signal blind. Callers that genuinely want a one-shot
                // kill should call send_sigterm first.
                false
            }
        };

        if !identity_ok {
            tracing::warn!(
                pid = pid,
                "SIGKILL aborted: PID-reuse guard fired (process exited or PID recycled \
                 during grace period)"
            );
            if let Some(pk) = self.pending_kills.get_mut(&pid) {
                pk.sigkill_time = Some(chrono::Utc::now());
            }
            return Ok(KillAction::PidReusedAborted);
        }

        tracing::info!("sending SIGKILL to PID {}: {}", pid, name);

        // Prefer pidfd_send_signal when available — race-free.
        let send_result: Result<(), std::io::Error> =
            match pending.as_ref().and_then(|p| p.pidfd.as_ref()).cloned() {
                Some(fd) => pid_reuse::pidfd_send_kill(fd.as_fd(), libc::SIGKILL),
                None => {
                    // Fallback path: starttime check passed above, so the PID
                    // still belongs to the same process. SAFETY: libc::kill
                    // with a valid signal number is always sound.
                    let r = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                    if r != 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                }
            };

        if let Err(e) = send_result {
            // ESRCH means the original process is gone (pidfd path) or
            // the PID has been freed and not yet reused (kill path). In
            // either case, refusing the SIGKILL is the right outcome —
            // the kernel's already done it for us. Map to PidReusedAborted
            // so the audit log records the abort consistently.
            if e.raw_os_error() == Some(libc::ESRCH) {
                tracing::warn!(pid = pid, "SIGKILL aborted: target process is gone (ESRCH)");
                if let Some(pk) = self.pending_kills.get_mut(&pid) {
                    pk.sigkill_time = Some(chrono::Utc::now());
                }
                return Ok(KillAction::PidReusedAborted);
            }
            return Err(GovernorError::SignalError(format!(
                "SIGKILL failed for PID {}: {}",
                pid, e
            )));
        }

        if let Some(pending) = self.pending_kills.get_mut(&pid) {
            pending.sigkill_time = Some(chrono::Utc::now());
        }

        Ok(KillAction::SignalKillSent)
    }

    /// Iterate every pending kill whose grace period has expired and
    /// dispatch SIGKILL via `send_sigkill`. Returns `(pid, action)` for
    /// each escalation so the caller can audit-log the outcome.
    ///
    /// Does NOT consume entries on `PidReusedAborted` — the entry stays
    /// so `pending_kills_count()` and `get_pending_kills()` reflect the
    /// abort. Callers can call `clear_pending(pid)` after auditing.
    pub fn execute_after_grace(&mut self) -> Vec<(u32, GovernorResult<KillAction>)> {
        let pids = self.check_grace_period_expired();
        let mut out = Vec::with_capacity(pids.len());
        for pid in pids {
            let name = self
                .pending_kills
                .get(&pid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let result = self.send_sigkill(pid, &name);
            out.push((pid, result));
        }
        out
    }

    /// Check which pending processes need SIGKILL (grace period expired).
    pub fn check_grace_period_expired(&self) -> Vec<u32> {
        let grace_period = Duration::seconds(self.policy.sigterm_grace_period_secs as i64);
        self.pending_kills
            .iter()
            .filter(|(_, pending)| pending.should_send_kill(grace_period))
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// Get count of pending kills waiting for grace period.
    pub fn pending_kills_count(&self) -> usize {
        self.pending_kills.len()
    }

    /// Get pending kills (for testing/auditing).
    pub fn get_pending_kills(&self) -> Vec<PendingKill> {
        self.pending_kills.values().cloned().collect()
    }

    /// Clear a pending kill (process exited on its own).
    pub fn clear_pending(&mut self, pid: u32) {
        self.pending_kills.remove(&pid);
    }

    /// Get policy reference.
    pub fn policy(&self) -> &GovernorPolicy {
        &self.policy
    }

    /// Get mutable policy reference.
    pub fn policy_mut(&mut self) -> &mut GovernorPolicy {
        &mut self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_lifecycle(
        pid: u32,
        name: &str,
        category: Option<AICategory>,
        exited: bool,
    ) -> ProcessLifecycle {
        let sample = crate::model::ProcessSample {
            pid,
            ppid: Some(1),
            name: name.to_string(),
            cmdline: vec![name.to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };

        let mut lc = ProcessLifecycle::new(&sample, category);
        if exited {
            lc.mark_exit(Some(0), None);
        }
        lc
    }

    #[test]
    fn executor_new() {
        let policy = GovernorPolicy::safe_default();
        let executor = GovernorExecutor::new(policy);
        assert_eq!(executor.pending_kills_count(), 0);
    }

    #[test]
    fn executor_evaluate_whitelisted() {
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot
            .processes
            .insert(100, make_lifecycle(100, "bash", None, false));

        let decisions = executor.evaluate(&snapshot);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].1, KillAction::Whitelisted);
    }

    #[test]
    fn executor_evaluate_exited() {
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            101,
            make_lifecycle(101, "ai_proc", Some(AICategory::Inference), true),
        );

        let decisions = executor.evaluate(&snapshot);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].1, KillAction::AlreadyExited);
    }

    #[test]
    fn executor_pending_kills_count() {
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let pending = PendingKill::new(100, "test".to_string(), AICategory::Inference);
        executor.pending_kills.insert(100, pending);

        assert_eq!(executor.pending_kills_count(), 1);
    }

    #[test]
    fn executor_rate_limits_enforced_kills() {
        // 10 kill-eligible processes, budget 3 — only 3 get SignalTermSent,
        // the rest are RateLimited. Matches HANDOFF Module 5 acceptance test.
        let mut policy = GovernorPolicy::safe_default();
        policy.rate_limit_max_kills = 3;
        policy.rate_limit_window_secs = 60;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        for pid in 200..210u32 {
            snapshot.processes.insert(
                pid,
                make_lifecycle(
                    pid,
                    &format!("worker{pid}"),
                    Some(AICategory::Inference),
                    false,
                ),
            );
        }

        let decisions = executor.evaluate(&snapshot);
        let killed = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
            .count();
        let limited = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::RateLimited)
            .count();
        assert_eq!(killed, 3, "must not exceed rate limit");
        assert_eq!(limited, 7, "remaining candidates must be rate-limited");
    }

    #[test]
    fn executor_rate_limit_disabled_when_max_is_zero() {
        let mut policy = GovernorPolicy::safe_default();
        policy.rate_limit_max_kills = 0;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        for pid in 400..410u32 {
            snapshot.processes.insert(
                pid,
                make_lifecycle(pid, &format!("w{pid}"), Some(AICategory::Inference), false),
            );
        }
        let decisions = executor.evaluate(&snapshot);
        let killed = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
            .count();
        assert_eq!(killed, 10, "max_kills=0 means unlimited");
    }

    #[test]
    fn executor_clear_pending() {
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let pending = PendingKill::new(100, "test".to_string(), AICategory::Training);
        executor.pending_kills.insert(100, pending);
        assert_eq!(executor.pending_kills_count(), 1);

        executor.clear_pending(100);
        assert_eq!(executor.pending_kills_count(), 0);
    }
}
