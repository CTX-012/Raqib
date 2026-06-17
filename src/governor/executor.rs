use crate::governor::pid_reuse;
use crate::governor::threshold_breach::{HostBreach, ThresholdBreach};
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
    ///
    /// v1.3.2 / DISPATCH 78 / step-3 — `breaches` is the narrow
    /// threshold-breach projection (Q6 — VRAM%-first). Built by the
    /// runtime tick layer via [`crate::governor::threshold_breach::
    /// build_threshold_breaches`] and passed in as a slice; the
    /// executor stays decoupled from `&RuntimeState`. A PID with
    /// no entry in `breaches` is treated as "not breached" (the
    /// honesty default — absence is not breach). This is the
    /// narrow-projection contract per DISPATCH 59 M4 option b;
    /// widening to `&RuntimeState` is explicitly rejected by the
    /// design (couples the governor to the whole state graph).
    ///
    /// AUTHORITY: the projection is a SIGNAL surface, not an
    /// ACTUATION surface. `evaluate()` still only RETURNS decisions;
    /// nothing in this function fires a signal. The 3 phantom-kill
    /// scar layers and 4 observe-only firewalls remain intact.
    pub fn evaluate(
        &mut self,
        lifecycle_snapshot: &LifecycleSnapshot,
        breaches: &[ThresholdBreach],
        host_breach: &HostBreach,
    ) -> Vec<(u32, KillAction, String)> {
        self.trim_rate_limit_window();
        // v1.3.2 / DISPATCH 79 / step-4 (Q4) — deterministic
        // candidate ordering. `LifecycleSnapshot.processes` is a
        // HashMap; iteration is non-deterministic. When the rate
        // limiter's per-window budget forces a subset (N < total
        // kill-candidates), WHICH PIDs get the kill decision must
        // not depend on HashMap iteration order — that would make
        // identical inputs produce different `state.decisions`
        // across runs, breaking auditability and burying
        // reproduction bugs.
        //
        // Q4 STOPGAP: sort ascending by PID. Cheap, correct,
        // auditable. The lowest-numbered PID wins the budget when
        // there's contention. This is intentionally NOT the long-
        // term tiebreaker — Q4 v-next will land least-recent-
        // activity ordering once `LiveTelemetry::last_active_at`
        // exists (per DISPATCH 59 M5). Lowest-PID is a workable
        // proxy today: long-lived AI workloads tend to have lower
        // PIDs than short-lived noise, and a deterministic
        // wrong-ish answer is strictly better than a
        // non-deterministic right-ish one.
        //
        // AUTHORITY: this is ORDERING ONLY. The set of decisions
        // a given snapshot produces is unchanged — only the
        // rate-limit truncation becomes stable. No kill is wired;
        // `send_sigterm` production-caller count unchanged. The
        // 3 phantom-kill scar layers and 4 firewalls stay intact.
        let mut sorted_pids: Vec<u32> =
            lifecycle_snapshot.processes.keys().copied().collect();
        sorted_pids.sort_unstable();
        let mut decisions = Vec::with_capacity(sorted_pids.len());
        for pid in sorted_pids {
            // The lifecycle lookup is O(1) on the HashMap; the
            // sort cost is O(N log N) on N ≤ few-hundred AI
            // processes — negligible on the 1 Hz tick budget.
            let Some(lifecycle) = lifecycle_snapshot.processes.get(&pid) else {
                // Defensive: can't happen because we sourced the
                // key set from this same HashMap. If a future
                // refactor mutates the snapshot mid-evaluate,
                // we'd rather skip than panic.
                continue;
            };
            // Per-PID breach lookup; O(N·M) overall but N and M are
            // both bounded by the few-dozen-AI-process headcount on
            // realistic hosts, so a HashMap pre-index would be
            // premature. If a future profile shows it matters, swap
            // to a HashMap built once at evaluate() entry.
            let breach = breaches.iter().find(|b| b.pid == pid);
            let (action, reason) = self.evaluate_process(pid, lifecycle, breach, host_breach);
            // Record enforced kills against the window so subsequent
            // candidates in the same tick see the budget drop.
            //
            // v1.3.2 / DISPATCH 77 / 62-E — `AlreadyPending` is
            // deliberately EXCLUDED from this counter. A stubborn
            // post-SIGTERM PID returns `AlreadyPending` every tick
            // (`evaluate_process`'s pending-check short-circuit
            // below); counting those would re-drain the budget the
            // pre-fix code drained by re-emitting `SignalTermSent`.
            // Only the FIRST `SignalTermSent` for a given PID
            // counts against the 3-per-60s window; subsequent
            // ticks for the same PID surface as `AlreadyPending`
            // and leave the budget free for OTHER PIDs that need
            // fresh kills.
            if matches!(action, KillAction::SignalTermSent) {
                self.recent_kills.push_back(Utc::now());
            }
            decisions.push((pid, action, reason));
        }
        decisions
    }

    /// Evaluate a single process. Mutable for symmetry with `evaluate`, but
    /// callers at the single-process layer usually want the sliding window
    /// frozen at a known state — call `trim_rate_limit_window` first.
    ///
    /// v1.3.2 / DISPATCH 77 — `pid` is now an explicit parameter so
    /// the `AlreadyPending` short-circuit can consult
    /// `self.pending_kills`; the pre-bump signature took just
    /// `lifecycle` because the only-AlreadyExited shortcut needed
    /// nothing more than the lifecycle's `is_exited()` predicate.
    ///
    /// v1.3.2 / DISPATCH 78 / step-3 — `breach` is the per-PID
    /// threshold-breach summary. `None` means "no projection row
    /// for this PID" (e.g. the breach builder hasn't run yet or
    /// the PID arrived between projection and evaluate). `Some(b)`
    /// with `b.vram_breached == false` is the explicit
    /// "measured-but-not-breaching" verdict. Both are treated as
    /// "no breach" for the kill gate. Only the
    /// `Some(b) && b.vram_breached` case can yield a kill DECISION,
    /// and even then only when the policy permits — see the Kill
    /// branch below for the gate ordering.
    fn evaluate_process(
        &self,
        pid: u32,
        lifecycle: &ProcessLifecycle,
        breach: Option<&ThresholdBreach>,
        host_breach: &HostBreach,
    ) -> (KillAction, String) {
        // Already exited: nothing to do
        if lifecycle.is_exited() {
            return (
                KillAction::AlreadyExited,
                "process already exited".to_string(),
            );
        }

        // v1.3.2 / DISPATCH 77 / 62-E — pending-kill short-circuit.
        // Symmetric with the `AlreadyExited` check above: same
        // position (top of `evaluate_process`, before policy /
        // rate-limit branches), same "decision-only, no actuation"
        // shape. Returned for any PID still on `pending_kills`,
        // which today is populated only by `send_sigterm` —
        // currently a zero-production-caller method (the v1.0.1
        // scar). When step-5 of the auto-kill arc wires the
        // tick-loop actuation, this short-circuit prevents the
        // re-emission-of-SignalTermSent-every-tick behaviour that
        // would drain the 3-per-60s budget alone on a stubborn
        // post-SIGTERM workload (DISPATCH 62-E). Until step-5
        // lands, this branch is unreachable in production because
        // `pending_kills` stays empty.
        //
        // AUTHORITY: this is a DECISION, not an action — it
        // records "we know we already SIGTERM'd this PID." No
        // signal is sent here; no caller of `send_sigterm` is
        // added by this change. All four observe-only firewalls
        // and the three phantom-kill scar layers stay intact.
        if self.pending_kills.contains_key(&pid) {
            return (
                KillAction::AlreadyPending,
                format!(
                    "PID {pid} has a pending SIGTERM not yet reaped — \
                     deferring re-emission to avoid rate-limit budget drain",
                ),
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
                // v1.3.2 / DISPATCH 78 / step-3 — breach gate.
                // DISPATCH 84 / step-8 — WIDENED from VRAM-only
                // to (VRAM OR RAM OR host-thermal).
                //
                // A PID becomes a kill candidate when AT LEAST ONE
                // breach signal fires:
                //
                //   * Per-PID VRAM critical (D78).
                //   * Per-PID RAM critical (D84) — this PID's RSS
                //     exceeds ram_critical_pct of host total RAM.
                //   * Host-level thermal red (D84) — ANY zone
                //     exceeds thermal_red_c. The host is shedding
                //     load; AI workloads are the candidates.
                //
                // HONESTY (matches D74/D76 VRAM_UNMEASURED): when a
                // metric is unmeasured (None), the breach builder
                // sets the corresponding `*_breached = false`.
                // Absence ≠ breach. The host-thermal case has no
                // per-PID column to be absent; an empty thermal-
                // zones list maps to `host_breach.thermal_breached
                // = false` at the projection layer.
                //
                // When NO signal fires, fall through to Skipped:
                // the policy SIGNAL is "kill if needed" but the
                // workload is fine right now.
                let vram_breaching = breach.is_some_and(|b| b.vram_breached);
                let ram_breaching = breach.is_some_and(|b| b.ram_breached);
                let host_thermal = host_breach.thermal_breached;
                let any_breach = vram_breaching || ram_breaching || host_thermal;
                if !any_breach {
                    return (
                        KillAction::Skipped,
                        format!(
                            "AI process not breaching VRAM/RAM/thermal: {} \
                             (vram_pct={:?}, ram_pct={:?}, max_temp_c={:?})",
                            lifecycle.name,
                            breach.and_then(|b| b.vram_pct),
                            breach.and_then(|b| b.ram_pct),
                            host_breach.max_temp_c,
                        ),
                    );
                }
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
                    // Reason string names the SPECIFIC signal(s)
                    // that fired so the audit trail attributes
                    // the kill correctly (operator can tell a
                    // VRAM kill from a RAM kill from a thermal
                    // kill at a glance).
                    let triggers: Vec<&str> = [
                        if vram_breaching { Some("vram") } else { None },
                        if ram_breaching { Some("ram") } else { None },
                        if host_thermal { Some("host-thermal") } else { None },
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    (
                        KillAction::SignalTermSent,
                        format!(
                            "AI process marked for kill: {:?} \
                             (triggers=[{}], vram_pct={:?}, ram_pct={:?}, \
                             max_temp_c={:?}, hottest_zone={:?})",
                            category.unwrap_or(AICategory::NotAi),
                            triggers.join(","),
                            breach.and_then(|b| b.vram_pct),
                            breach.and_then(|b| b.ram_pct),
                            host_breach.max_temp_c,
                            host_breach.hottest_zone,
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

    /// DISPATCH 81 — test-only seeder for `pending_kills`.
    ///
    /// Runtime-layer tests need to verify that the auto-escalation
    /// path (`record_governor_audit` calling `execute_after_grace`)
    /// is gated on `auto_actuate` WITHOUT having to spawn a real
    /// child and run `send_sigterm` against it. This accessor lets
    /// the test seed a `PendingKill` with an arbitrary `sigterm_time`
    /// (e.g. far in the past, to immediately exceed `grace_period`).
    ///
    /// Production callers MUST go through `send_sigterm` so the
    /// PID-reuse guard's identity tokens (pidfd + starttime) are
    /// captured BEFORE the kill — the v1.0.1 protection. This
    /// helper deliberately exposes a way to skip that capture, so
    /// it's `#[cfg(test)]`-only and never compiles into the shipped
    /// binary.
    #[cfg(test)]
    pub fn insert_pending_kill_for_test(&mut self, pending: PendingKill) {
        self.pending_kills.insert(pending.pid, pending);
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
    use crate::governor::policy::PolicyAction;
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

        let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
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

        let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
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
        // v1.0.1 — flip default_ai_action back to Kill for THIS test so
        // the rate-limit semantics stay testable; the safe_default's
        // top-level Allow is the production posture, and the rate
        // limiter only fires when an operator opts back into Kill.
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        policy.rate_limit_max_kills = 3;
        policy.rate_limit_window_secs = 60;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        let mut breaches: Vec<ThresholdBreach> = Vec::new();
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
            // v1.3.2 / DISPATCH 78 — all 10 PIDs are breaching, so
            // we reach the rate-limit branch. The test is about
            // the per-window cap; the breach gate is "input" here,
            // not "subject under test."
            breaches.push(ThresholdBreach {
                pid,
                vram_pct: Some(99.0),
                vram_breached: true,
                ..ThresholdBreach::default()

            });
        }

        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
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
        // v1.0.1 — same opt-back-in as the enforced-kills test above:
        // the rate-limit-zero semantics presupposes Kill is the
        // operator-selected default action.
        policy.default_ai_action = PolicyAction::Kill;
        policy.rate_limit_max_kills = 0;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        let mut breaches: Vec<ThresholdBreach> = Vec::new();
        for pid in 400..410u32 {
            snapshot.processes.insert(
                pid,
                make_lifecycle(pid, &format!("w{pid}"), Some(AICategory::Inference), false),
            );
            // v1.3.2 / DISPATCH 78 — all 10 breaching so the
            // unlimited-budget path is reachable.
            breaches.push(ThresholdBreach {
                pid,
                vram_pct: Some(99.0),
                vram_breached: true,
                ..ThresholdBreach::default()

            });
        }
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
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

    // ── v1.3.2 / DISPATCH 77 / 62-E — AlreadyPending tests ────────

    /// Core 62-E fix: a PID with an outstanding entry in
    /// `pending_kills` returns `AlreadyPending` from `evaluate()`
    /// rather than re-emitting `SignalTermSent`. Symmetric with the
    /// `AlreadyExited` shortcut — same position in
    /// `evaluate_process`, same "don't act, just observe" intent.
    #[test]
    fn evaluate_returns_already_pending_for_pid_in_pending_kills() {
        // Operator's opt-in policy so the Kill branch is reachable
        // — without this we'd land in Whitelisted/Allow and never
        // exercise the pending check at all.
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        // Plant a pending kill for PID 555. In production this
        // happens via `send_sigterm`'s `pending_kills.insert(...)`
        // call — that method has no production callers today (the
        // v1.0.1 scar), so the test inserts the record directly.
        executor.pending_kills.insert(
            555,
            PendingKill::new(555, "stubborn".to_string(), AICategory::Inference),
        );

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            555,
            make_lifecycle(555, "stubborn", Some(AICategory::Inference), false),
        );

        let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0].1,
            KillAction::AlreadyPending,
            "PID with pending kill MUST return AlreadyPending, NOT a \
             second SignalTermSent — that would drain the rate-limit \
             budget across ticks (62-E)",
        );
    }

    /// The 62-E bug pinned directly: a single stubborn pending PID
    /// across N evaluate() ticks MUST NOT drain the 3-per-60s rate
    /// budget — leaving room for OTHER PIDs that need a fresh kill.
    /// Pre-fix, each tick re-emitted `SignalTermSent` for the same
    /// PID, consuming the budget over three ticks; post-fix, those
    /// ticks return `AlreadyPending` and the budget stays at 3.
    #[test]
    fn pending_pid_does_not_drain_rate_limit_budget_across_ticks() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        policy.rate_limit_max_kills = 3;
        policy.rate_limit_window_secs = 60;
        let mut executor = GovernorExecutor::new(policy);

        // Stubborn PID with an outstanding SIGTERM.
        executor.pending_kills.insert(
            999,
            PendingKill::new(999, "stubborn".to_string(), AICategory::Inference),
        );

        // Run evaluate() three times against a snapshot containing
        // only the pending PID. Each tick should return
        // AlreadyPending, NOT SignalTermSent — so 0 budget is
        // consumed across three ticks.
        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            999,
            make_lifecycle(999, "stubborn", Some(AICategory::Inference), false),
        );

        for tick in 1..=3 {
            let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
            assert_eq!(decisions.len(), 1, "tick {tick}: one PID in snapshot");
            assert_eq!(
                decisions[0].1,
                KillAction::AlreadyPending,
                "tick {tick}: stubborn pending PID must NOT re-emit \
                 SignalTermSent (would drain budget)",
            );
        }
        assert_eq!(
            executor.kills_remaining_in_window(),
            3,
            "after 3 ticks against a stubborn pending PID, the budget \
             must still be 3/3 — the pre-fix bug drained 3/3 → 0/3",
        );

        // Now confirm the budget is actually available for OTHER
        // PIDs: add three fresh AI workloads and confirm all three
        // get SignalTermSent (the saved budget is real, not
        // accidentally locked).
        let mut snap2 = crate::lifecycle::LifecycleSnapshot::new();
        // Keep the pending PID in the snapshot (the lifecycle
        // tracker would still see it alive until it reaps).
        snap2.processes.insert(
            999,
            make_lifecycle(999, "stubborn", Some(AICategory::Inference), false),
        );
        let mut breaches2: Vec<ThresholdBreach> = Vec::new();
        // v1.3.2 / DISPATCH 78 — the 3 fresh PIDs all need a
        // breach entry to reach the rate-limit path. The stubborn
        // PID 999 hits the AlreadyPending short-circuit BEFORE the
        // breach gate, so its breach entry is moot — left out for
        // clarity.
        for pid in 1000..1003u32 {
            snap2.processes.insert(
                pid,
                make_lifecycle(
                    pid,
                    &format!("worker{pid}"),
                    Some(AICategory::Inference),
                    false,
                ),
            );
            breaches2.push(ThresholdBreach {
                pid,
                vram_pct: Some(99.0),
                vram_breached: true,
                ..ThresholdBreach::default()

            });
        }
        let decisions = executor.evaluate(&snap2, &breaches2, &crate::governor::threshold_breach::HostBreach::default());
        let killed = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
            .count();
        let pending = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::AlreadyPending)
            .count();
        assert_eq!(
            killed, 3,
            "all three fresh PIDs must get SignalTermSent — the \
             pending PID's budget non-drain means the OTHER PIDs are \
             not starved (62-E acceptance)",
        );
        assert_eq!(pending, 1, "the stubborn PID stays AlreadyPending");
    }

    /// Defensive symmetry: `AlreadyExited` behaviour is UNCHANGED
    /// by the `AlreadyPending` insert. A PID that's both pending
    /// AND now exited returns `AlreadyExited` — the exited check
    /// fires first (`evaluate_process` line ordering). This matters
    /// because an exited PID's `pending_kills` entry is stale and
    /// should be cleaned up; surfacing `AlreadyExited` lets a future
    /// reap step do that work, while `AlreadyPending` would mask the
    /// cleanup signal.
    #[test]
    fn exited_pid_with_pending_record_still_returns_already_exited() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        executor.pending_kills.insert(
            777,
            PendingKill::new(777, "ex-pending".to_string(), AICategory::Inference),
        );

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            777,
            // exited=true ⇒ lifecycle.is_exited() returns true
            make_lifecycle(777, "ex-pending", Some(AICategory::Inference), true),
        );

        let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(
            decisions[0].1,
            KillAction::AlreadyExited,
            "exited check must fire BEFORE pending check — a dead PID \
             with a stale pending record is `AlreadyExited`, not \
             `AlreadyPending`. The exited path leaves a cleanup-signal \
             surface for a future reap step.",
        );
    }

    /// Regression hedge for the pre-existing `AlreadyExited`
    /// shortcut: behavior unchanged by the new variant insert. A
    /// PID with NO pending record AND no exit returns the normal
    /// policy outcome; this test reaffirms the executor_evaluate
    /// _exited test still passes against the new structure.
    #[test]
    fn already_exited_shortcut_unchanged_after_pending_insert() {
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            101,
            make_lifecycle(101, "ai_proc", Some(AICategory::Inference), true),
        );

        let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(decisions[0].1, KillAction::AlreadyExited);
    }

    // ── v1.3.2 / DISPATCH 78 / step-3 — breach-gate tests ─────────

    /// Core dispatch invariant: a VRAM-breached PID under an
    /// opted-in policy yields `SignalTermSent` (a kill DECISION).
    /// Production this still doesn't ACTUATE — `send_sigterm` has
    /// no production caller (the tripwire below); step-5 wires
    /// the actuation behind `auto_actuate`. This test is about
    /// the decision-emission shape only.
    #[test]
    fn breached_pid_under_kill_policy_yields_signaltermsent_decision() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            42,
            make_lifecycle(42, "ai-greedy", Some(AICategory::Inference), false),
        );
        let breaches = vec![ThresholdBreach {
            pid: 42,
            vram_pct: Some(99.5),
            vram_breached: true,
            ..ThresholdBreach::default()
        }];
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(decisions[0].1, KillAction::SignalTermSent);
    }

    /// The phantom-kill scar holds: with the production default
    /// (Allow), a VRAM-breached PID produces a `Whitelisted`
    /// decision — NO SignalTermSent. The policy gate fires
    /// BEFORE the threshold matters. v1.0.1 phantom-kill scar
    /// layer 2 explicit:  default Allow ⇒ no automated kills
    /// even when a workload is genuinely over the line.
    #[test]
    fn breached_pid_under_default_allow_policy_does_not_kill() {
        // safe_default() ships with default_ai_action = Allow.
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            42,
            make_lifecycle(42, "ai-greedy", Some(AICategory::Inference), false),
        );
        let breaches = vec![ThresholdBreach {
            pid: 42,
            vram_pct: Some(99.5),
            vram_breached: true,
            ..ThresholdBreach::default()
        }];
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(
            decisions[0].1,
            KillAction::Whitelisted,
            "v1.0.1 scar: default Allow MUST suppress kill decision \
             even with a VRAM breach in evidence — policy gate fires \
             before threshold gate",
        );
    }

    /// Not-breached + opted-in policy: the policy says "kill if
    /// needed," the metrics say "not needed." Decision is
    /// `Skipped`, not `SignalTermSent`. This is the explicit
    /// "measured-but-not-breaching" verdict — distinct from the
    /// unmeasured case below.
    #[test]
    fn not_breached_pid_under_kill_policy_skips_kill() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            43,
            make_lifecycle(43, "ai-quiet", Some(AICategory::Inference), false),
        );
        let breaches = vec![ThresholdBreach {
            pid: 43,
            vram_pct: Some(20.0),
            vram_breached: false,
            ..ThresholdBreach::default()
        }];
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(
            decisions[0].1,
            KillAction::Skipped,
            "policy=Kill + vram_breached=false ⇒ Skipped (no kill); \
             got {:?}",
            decisions[0].1,
        );
    }

    /// Hard rule #5: unmeasured VRAM (no breach entry for the PID)
    /// is treated as NOT breached. The kill DECISION is Skipped,
    /// NOT SignalTermSent. Pinned because the current host (with
    /// the GPU driver unloaded) is in exactly this state — the
    /// dispatch wants this case loud.
    #[test]
    fn unmeasured_vram_pid_under_kill_policy_does_not_decide_kill() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            44,
            make_lifecycle(44, "ai-unmeasured", Some(AICategory::Inference), false),
        );
        // NO breach entry for PID 44 — simulates "GPU driver
        // unloaded, projection can't compute vram_pct."
        let decisions = executor.evaluate(&snapshot, &[], &crate::governor::threshold_breach::HostBreach::default());
        assert_eq!(
            decisions[0].1,
            KillAction::Skipped,
            "Hard rule #5: unmeasured VRAM (no breach row) ⇒ NEVER \
             a kill decision. Got {:?}",
            decisions[0].1,
        );
    }

    /// Selective breach: in a snapshot of multiple PIDs, only the
    /// breaching one gets the kill decision. The OTHERS that
    /// happen to be measured-but-fine OR unmeasured stay
    /// Skipped. This pins the per-PID independence of the breach
    /// gate.
    #[test]
    fn breach_gate_is_per_pid_independent() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        for pid in [50u32, 51, 52] {
            snapshot.processes.insert(
                pid,
                make_lifecycle(pid, "ai", Some(AICategory::Inference), false),
            );
        }
        let breaches = vec![
            ThresholdBreach { pid: 50, vram_pct: Some(99.0), vram_breached: true, ..ThresholdBreach::default() },
            ThresholdBreach { pid: 51, vram_pct: Some(30.0), vram_breached: false, ..ThresholdBreach::default() },
            // pid 52: no entry — unmeasured.
        ];
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        let by_pid: std::collections::HashMap<u32, KillAction> = decisions
            .iter()
            .map(|(p, a, _)| (*p, *a))
            .collect();
        assert_eq!(by_pid[&50], KillAction::SignalTermSent);
        assert_eq!(by_pid[&51], KillAction::Skipped);
        assert_eq!(by_pid[&52], KillAction::Skipped);
    }

    /// Compile-time signature pin: `evaluate` accepts the narrow
    /// projection (`&LifecycleSnapshot`, `&[ThresholdBreach]`, and
    /// the DISPATCH 84 widening: `&HostBreach`). It does NOT take
    /// `&RuntimeState`. If a future refactor accidentally widens
    /// the signature to take state, this fn fails to compile.
    /// STOP #1 from the dispatch: the narrow projection is part
    /// of the design, not a coincidence.
    ///
    /// DISPATCH 84 / step-8 — the projection type grew (added
    /// `HostBreach` for host-level thermal). That's an allowed
    /// widening per the dispatch: "Widening the projection TYPE
    /// is fine; widening to `&RuntimeState` is not."
    #[test]
    fn evaluate_signature_takes_narrow_projection_not_runtime_state() {
        // The function-pointer type assertion below cannot widen
        // to `&RuntimeState`. If someone widens the signature,
        // this `_FN_TYPE` binding fails to type-check.
        type EvaluateFn = fn(
            &mut GovernorExecutor,
            &LifecycleSnapshot,
            &[ThresholdBreach],
            &HostBreach,
        ) -> Vec<(u32, KillAction, String)>;
        let _fn_type: EvaluateFn = GovernorExecutor::evaluate;
        // Runtime smoke: empty inputs are accepted.
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);
        let snapshot = crate::lifecycle::LifecycleSnapshot::new();
        let _ = executor.evaluate(&snapshot, &[], &HostBreach::default());
    }

    // ── v1.3.2 / DISPATCH 79 / step-4 — deterministic ordering ────

    /// The dispatch's core invariant: under a rate-limit budget
    /// smaller than the candidate count, the N PIDs selected for
    /// `SignalTermSent` are deterministically the N LOWEST PIDs —
    /// not whatever the HashMap iteration happened to surface.
    /// Pinned by repeating the SAME snapshot through evaluate()
    /// multiple times and asserting the selected set is invariant.
    #[test]
    fn rate_limit_subset_is_lowest_pids_deterministically() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        policy.rate_limit_max_kills = 3;
        policy.rate_limit_window_secs = 60;

        // 10 kill-eligible candidates all breaching the threshold.
        // With budget=3, only 3 land SignalTermSent; the other 7
        // are RateLimited. Q4 stopgap: the 3 selected MUST be
        // PIDs 500, 501, 502 (the lowest), not some HashMap
        // permutation.
        let pids: Vec<u32> = (500..510).collect();
        let breaches: Vec<ThresholdBreach> = pids
            .iter()
            .map(|p| ThresholdBreach {
                pid: *p,
                vram_pct: Some(99.0),
                vram_breached: true,
                ..ThresholdBreach::default()

            })
            .collect();

        // Repeat the evaluate() call from a fresh executor 16
        // times. HashMap iteration is randomised per-process via
        // SipHash with a per-process key, so within ONE process
        // we may see consistent (but arbitrary) order. The N
        // repetitions guard against the case where the test
        // accidentally pinned an arbitrary order — with the sort
        // in place, all 16 selections must be identical, AND must
        // be the lowest 3.
        let expected: Vec<u32> = pids[..3].to_vec();
        for run in 0..16 {
            let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
            for pid in &pids {
                snapshot.processes.insert(
                    *pid,
                    make_lifecycle(
                        *pid,
                        &format!("ai{pid}"),
                        Some(AICategory::Inference),
                        false,
                    ),
                );
            }
            let mut executor = GovernorExecutor::new(policy.clone());
            let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
            let selected: Vec<u32> = decisions
                .iter()
                .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
                .map(|(p, _, _)| *p)
                .collect();
            assert_eq!(
                selected, expected,
                "run {run}: rate-limit subset MUST be lowest-PID \
                 deterministically; got {selected:?}",
            );
        }
    }

    /// The decisions Vec itself is sorted ascending by PID under
    /// the new ordering — a downstream consumer that relies on
    /// "earlier decisions = lower PIDs" has a stable contract.
    /// Pinned because the existing rate-limit semantics ALREADY
    /// depended on iteration order implicitly; with deterministic
    /// ordering the contract becomes explicit.
    #[test]
    fn decisions_vec_is_sorted_ascending_by_pid() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        // Insert PIDs in scrambled order to defeat any
        // accidentally-sorted HashMap. The output should still
        // be ascending.
        let scramble: Vec<u32> = vec![9000, 100, 5000, 2, 42, 17, 999, 30];
        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        for pid in &scramble {
            snapshot.processes.insert(
                *pid,
                make_lifecycle(*pid, "ai", Some(AICategory::Inference), false),
            );
        }
        let breaches: Vec<ThresholdBreach> = scramble
            .iter()
            .map(|p| ThresholdBreach {
                pid: *p,
                vram_pct: Some(99.0),
                vram_breached: true,
                ..ThresholdBreach::default()

            })
            .collect();
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        let observed_pids: Vec<u32> = decisions.iter().map(|(p, _, _)| *p).collect();
        let mut expected = scramble.clone();
        expected.sort_unstable();
        assert_eq!(
            observed_pids, expected,
            "decisions Vec MUST be sorted ascending by PID",
        );
    }

    /// Ordering does NOT change the action-per-PID. A whitelisted
    /// process stays Whitelisted, a non-breaching process stays
    /// Skipped, etc. The sort changes WHICH PIDs survive the
    /// rate-limit cap, not WHAT verdict each PID receives.
    #[test]
    fn sort_does_not_change_action_per_pid() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        // No rate limit pressure — every Kill-eligible PID gets
        // SignalTermSent, every non-breaching one gets Skipped,
        // every allowlisted gets Whitelisted.
        policy.rate_limit_max_kills = 100;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        // PID 100: allowlisted shell
        snapshot.processes.insert(
            100,
            make_lifecycle(100, "bash", None, false),
        );
        // PID 200: breaching AI workload
        snapshot.processes.insert(
            200,
            make_lifecycle(200, "ai-greedy", Some(AICategory::Inference), false),
        );
        // PID 300: AI workload that is NOT breaching
        snapshot.processes.insert(
            300,
            make_lifecycle(300, "ai-quiet", Some(AICategory::Inference), false),
        );
        let breaches = vec![
            ThresholdBreach { pid: 200, vram_pct: Some(99.0), vram_breached: true, ..ThresholdBreach::default() },
            ThresholdBreach { pid: 300, vram_pct: Some(30.0), vram_breached: false, ..ThresholdBreach::default() },
        ];
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        let by_pid: std::collections::HashMap<u32, KillAction> = decisions
            .iter()
            .map(|(p, a, _)| (*p, *a))
            .collect();
        assert_eq!(by_pid[&100], KillAction::Whitelisted);
        assert_eq!(by_pid[&200], KillAction::SignalTermSent);
        assert_eq!(by_pid[&300], KillAction::Skipped);
    }

    /// Phantom-kill scar (layer 2) survives ordering: with default
    /// Allow policy, ZERO SignalTermSent decisions are emitted
    /// regardless of how many breached candidates exist or what
    /// order they're considered in. Pinned as a regression hedge
    /// against any future refactor that accidentally reorders the
    /// policy / breach / rate-limit gates.
    #[test]
    fn default_allow_policy_emits_no_signaltermsent_even_with_breaches() {
        let policy = GovernorPolicy::safe_default();
        // safe_default() ⇒ default_ai_action = PolicyAction::Allow.
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        let breaches: Vec<ThresholdBreach> = (1..=20)
            .map(|pid| {
                snapshot.processes.insert(
                    pid,
                    make_lifecycle(pid, "ai", Some(AICategory::Inference), false),
                );
                ThresholdBreach { pid, vram_pct: Some(99.0), vram_breached: true, ..ThresholdBreach::default() }
            })
            .collect();
        let decisions = executor.evaluate(&snapshot, &breaches, &crate::governor::threshold_breach::HostBreach::default());
        let killed = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
            .count();
        assert_eq!(
            killed, 0,
            "v1.0.1 scar layer 2: default Allow MUST suppress ALL \
             SignalTermSent decisions, regardless of breach count \
             or ordering. Got {killed}.",
        );
    }

    /// Authority-lock tripwire: in-file caller-count guard for
    /// `send_sigterm` inside `executor.rs`. D80 deliberately added
    /// one production caller — but in `runtime.rs`, NOT here. The
    /// executor-internal count must STAY at 1: the
    /// `request_kill` wrapper (line ~176), an aliased re-entry
    /// preserved for `tests/governor_pid_reuse.rs`. If a future
    /// commit adds a SECOND caller inside `executor.rs`, this test
    /// fires — that's almost certainly a refactor that broke the
    /// single-actuation-site invariant the dispatch maintains.
    ///
    /// The workspace-wide caller-count invariant ("exactly 2 total,
    /// one of them gated on `auto_actuate`") lives in
    /// [`tests::send_sigterm_actuation_site_is_auto_actuate_gated`]
    /// — it reads `runtime.rs` too, so it catches the more
    /// dangerous drift (an UNGATED caller appearing anywhere).
    #[test]
    fn send_sigterm_executor_internal_caller_count_unchanged() {
        // Read this file as-is and confirm the production-section
        // caller count is unchanged from pre-D77:
        //   1. The internal `request_kill` wrapper inside this
        //      same impl block (line ~176, an aliased re-entry).
        // Tests and doc-comments don't count.
        let src = include_str!("executor.rs");
        // Strip the `#[cfg(test)] mod tests { ... }` region — its
        // body contains test-only callers that don't ship.
        let test_marker = "#[cfg(test)]";
        let production_only = match src.find(test_marker) {
            Some(idx) => &src[..idx],
            None => src,
        };
        let call_sites = production_only.matches(".send_sigterm(").count();
        assert_eq!(
            call_sites, 1,
            "send_sigterm executor-internal call sites must equal 1 \
             (the `request_kill` wrapper). D80's actuation caller \
             belongs in `runtime.rs`, NOT here. Found {call_sites} \
             sites in the production region of executor.rs.",
        );
    }

    /// DISPATCH 83 — workspace-wide guard on `send_sigkill` callers,
    /// updated from D81 to admit the manual operator-consent path.
    ///
    /// Two production callers post-D83, each behind its own gate:
    ///
    ///   * AUTO: `record_governor_audit` calls
    ///     `governor.execute_after_grace()` (which loops calling
    ///     `send_sigkill` internally). Gated on `auto_actuate`.
    ///   * MANUAL: `manual_force_kill` calls `governor.send_sigkill`
    ///     directly. Gated by being reachable only from the TUI's
    ///     `force_kill_from_card` (operator pressed Enter on a
    ///     Waiting-state `KillConfirmCard`). The function name
    ///     itself signals the gate; the dispatcher is the only
    ///     consumer of the runtime API method.
    ///
    /// The test pins both gates structurally:
    ///   1. Exactly ONE internal `.send_sigkill(` inside executor.rs
    ///      (the `execute_after_grace` loop).
    ///   2. Exactly ONE runtime `.send_sigkill(` — and it MUST be
    ///      inside `fn manual_force_kill`, not in
    ///      `record_governor_audit` or anywhere else.
    ///   3. Exactly ONE runtime `.execute_after_grace(` call, and
    ///      that call is `auto_actuate`-gated by proximity.
    ///
    /// An unrouted direct `send_sigkill` (one that doesn't sit
    /// inside `manual_force_kill`) fails this test — that's the
    /// drift this guard catches.
    #[test]
    fn send_sigkill_callers_are_gated() {
        use std::fs;
        use std::path::PathBuf;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let test_marker = "#[cfg(test)]";

        let read_production = |rel: &str| -> String {
            let path = root.join(rel);
            let src = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            match src.find(test_marker) {
                Some(idx) => src[..idx].to_string(),
                None => src,
            }
        };

        let executor = read_production("src/governor/executor.rs");
        let runtime = read_production("src/runtime.rs");

        // (1) executor.rs internal callers: `execute_after_grace`'s
        // inner loop. Unchanged from D81.
        let executor_calls = executor.matches(".send_sigkill(").count();
        assert_eq!(
            executor_calls, 1,
            "executor.rs must contain EXACTLY ONE call to \
             .send_sigkill( — the internal `execute_after_grace` \
             loop. Found {executor_calls}.",
        );

        // (2) runtime.rs direct callers: exactly ONE — the
        // `manual_force_kill` operator-consent path. Pre-D83 this
        // was 0 (auto path went through `execute_after_grace`); D83
        // adds the manual direct caller, which MUST be inside
        // `fn manual_force_kill`.
        let runtime_direct_calls = runtime.matches(".send_sigkill(").count();
        assert_eq!(
            runtime_direct_calls, 1,
            "runtime.rs must contain EXACTLY ONE direct call to \
             .send_sigkill( — the operator-consent-gated \
             `manual_force_kill`. A SECOND direct caller anywhere \
             would mean an UNgated SIGKILL path landed; that's the \
             drift this guard catches. Found {runtime_direct_calls}.",
        );

        // (2a) Pin that the direct `send_sigkill` call lives inside
        // `fn manual_force_kill`. We locate `fn manual_force_kill`
        // and check that exactly ONE `.send_sigkill(` call appears
        // between it and the next `pub fn` boundary. Refactoring the
        // call out of this function would strip the consent gate.
        let mf_start = runtime
            .find("fn manual_force_kill")
            .unwrap_or_else(|| panic!("runtime.rs must define fn manual_force_kill"));
        let after = &runtime[mf_start..];
        let body_end = after.find("\n    pub fn ").unwrap_or(after.len());
        let body = &after[..body_end];
        assert_eq!(
            body.matches(".send_sigkill(").count(),
            1,
            "fn manual_force_kill MUST contain exactly ONE \
             .send_sigkill( call. If 0, the direct caller lives \
             somewhere else (drift); if ≥2, the function shape is \
             unexpected.",
        );

        // (3) Auto path: `execute_after_grace` is called exactly
        // once and that call is `auto_actuate`-gated by proximity.
        // Unchanged from D81 — the auto SIGKILL path still flows
        // through the wrapper.
        let execute_after_grace_calls = runtime.matches(".execute_after_grace(").count();
        assert_eq!(
            execute_after_grace_calls, 1,
            "runtime.rs must call execute_after_grace EXACTLY ONCE \
             (the auto-escalation site). Found {execute_after_grace_calls}.",
        );
        let call_idx = runtime
            .find(".execute_after_grace(")
            .unwrap_or_else(|| panic!("execute_after_grace call must be present"));
        let window_start = call_idx.saturating_sub(2048);
        let window = &runtime[window_start..call_idx];
        assert!(
            window.contains("auto_actuate"),
            "the runtime.rs execute_after_grace call must be lexically \
             preceded by an `auto_actuate` reference within 2048 chars. \
             Without that, the auto SIGKILL escalation is unguarded."
        );
    }

    /// DISPATCH 83 — workspace-wide guard on `send_sigterm` callers,
    /// updated from D80 to admit the manual operator-initiated path.
    ///
    /// Three production callers post-D83, each behind its own gate:
    ///
    ///   * `executor.rs::request_kill` (internal alias wrapper, used
    ///     by `tests/governor_pid_reuse.rs`).
    ///   * `runtime.rs::record_governor_audit` (AUTO path —
    ///     auto_actuate-gated, default false).
    ///   * `runtime.rs::manual_kill` (MANUAL path — operator-initiated
    ///     by the TUI's `k` keybinding; NOT auto_actuate-gated, but
    ///     reachable only via deliberate operator action).
    ///
    /// Pre-D83 the manual SIGTERM called `libc::kill` directly via
    /// `ManualKiller::kill_sigterm`, so `pending_kills` never tracked
    /// it and the D81 force-SIGKILL escalation had nothing to
    /// verify identity against. D83/C1 routes it through
    /// `send_sigterm` so the v1.0.1 PID-reuse guard's identity
    /// tokens are captured at SIGTERM time, ready for force-SIGKILL.
    ///
    /// The test pins both gates structurally:
    ///   1. Exactly THREE total production callers.
    ///   2. Exactly TWO runtime.rs callers (auto + manual).
    ///   3. The auto-site call lives inside `fn record_governor_audit`
    ///      AND is `auto_actuate`-proximity-gated.
    ///   4. The manual-site call lives inside `fn manual_kill` (no
    ///      auto_actuate proximity — manual is operator-initiated).
    ///
    /// A new runtime caller that doesn't sit in one of these two
    /// functions fails this test — that's the drift this guard
    /// catches.
    #[test]
    fn send_sigterm_actuation_site_is_auto_actuate_gated() {
        use std::fs;
        use std::path::PathBuf;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let test_marker = "#[cfg(test)]";

        let read_production = |rel: &str| -> String {
            let path = root.join(rel);
            let src = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            match src.find(test_marker) {
                Some(idx) => src[..idx].to_string(),
                None => src,
            }
        };

        // Helper: substring from a `fn <name>` declaration up to the
        // next `pub fn` boundary. Used to pin each runtime caller to
        // its expected function body.
        let fn_body = |src: &str, name: &str| -> String {
            let needle = format!("fn {name}");
            let start = src.find(&needle).unwrap_or_else(|| {
                panic!("runtime.rs must define fn {name} (D83 requires it)")
            });
            let after = &src[start..];
            let end = after.find("\n    pub fn ").unwrap_or(after.len());
            after[..end].to_string()
        };

        let executor = read_production("src/governor/executor.rs");
        let runtime = read_production("src/runtime.rs");
        let executor_calls = executor.matches(".send_sigterm(").count();
        let runtime_calls = runtime.matches(".send_sigterm(").count();
        let total = executor_calls + runtime_calls;

        // (1) Exactly THREE total callers post-D83.
        assert_eq!(
            total, 3,
            "expected exactly THREE production callers of send_sigterm \
             post-D83: (1) the internal `request_kill` wrapper in \
             executor.rs, (2) the auto_actuate-gated tick-loop \
             actuation site in runtime.rs::record_governor_audit, \
             and (3) the manual operator-initiated path in \
             runtime.rs::manual_kill. Found total={total} \
             (executor={executor_calls}, runtime={runtime_calls}). \
             A NEW caller anywhere else is suspicious — actuation \
             must stay funnelled through these three known sites.",
        );
        assert_eq!(
            runtime_calls, 2,
            "runtime.rs must contain EXACTLY TWO calls to \
             .send_sigterm( — the auto path (in record_governor_audit) \
             and the manual path (in manual_kill). Found {runtime_calls}.",
        );

        // (2) Auto-site: inside `fn record_governor_audit`, exactly
        // ONE call, lexically gated on `auto_actuate`.
        let auto_body = fn_body(&runtime, "record_governor_audit");
        assert_eq!(
            auto_body.matches(".send_sigterm(").count(),
            1,
            "fn record_governor_audit must contain exactly ONE \
             .send_sigterm( call (the auto path). If 0, the call \
             moved out (drift); if ≥2, structural surprise.",
        );
        assert!(
            auto_body.contains("auto_actuate"),
            "fn record_governor_audit must reference `auto_actuate` \
             (the gate). Without that reference the default-OFF \
             invariant is unenforceable by inspection.",
        );

        // (3) Manual-site: inside `fn manual_kill`, exactly ONE
        // call. NOT auto_actuate-gated (this is the operator path,
        // reached only when the operator presses `k` + Enter on the
        // kill_confirm card).
        let manual_body = fn_body(&runtime, "manual_kill");
        assert_eq!(
            manual_body.matches(".send_sigterm(").count(),
            1,
            "fn manual_kill must contain exactly ONE .send_sigterm( \
             call (the operator-initiated path). If 0, the call \
             moved out of the operator gate (drift); if ≥2, \
             structural surprise.",
        );
        // (4) Default-off invariant pin: the manual path must not
        // create an `auto_actuate`-relevant caller. The auto path's
        // gate is the early-return in `record_governor_audit`; the
        // manual path uses operator consent at the TUI dispatcher
        // and is allowed to be auto_actuate-free.
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 84 / step-8 — RAM + thermal candidate-eligibility.
    // ─────────────────────────────────────────────────────────────

    /// RAM-only breach + opted-in Kill policy ⇒ SignalTermSent.
    /// The widened gate accepts (vram_breached OR ram_breached OR
    /// host_thermal_breached). With only RAM tripping, a PID with
    /// vram_pct=None and vram_breached=false MUST still be flagged
    /// for kill (the policy permits, ram_breached fires).
    #[test]
    fn ram_only_breach_under_kill_policy_signals_kill() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot
            .processes
            .insert(70, make_lifecycle(70, "ram-hog", Some(AICategory::Inference), false));

        // RAM breach, no VRAM breach.
        let breaches = vec![ThresholdBreach {
            pid: 70,
            vram_pct: None,
            vram_breached: false,
            ram_pct: Some(97.0),
            ram_breached: true,
        }];
        let decisions = executor.evaluate(
            &snapshot,
            &breaches,
            &HostBreach::default(),
        );
        assert_eq!(
            decisions[0].1,
            KillAction::SignalTermSent,
            "policy=Kill + ram_breached=true (vram_breached=false) ⇒ \
             SignalTermSent. The breach gate is (vram OR ram OR thermal). \
             Got {:?}, reason={:?}",
            decisions[0].1,
            decisions[0].2,
        );
        assert!(
            decisions[0].2.contains("ram"),
            "reason string MUST name the RAM trigger so the audit \
             trail attributes the kill correctly; got: {:?}",
            decisions[0].2,
        );
    }

    /// Host-thermal breach + opted-in Kill policy ⇒ SignalTermSent
    /// even when per-PID has NO breach (the "shed load because the
    /// system is overheating" Q6 framing). Pin that the host-level
    /// signal alone is sufficient to flag an AI workload.
    #[test]
    fn host_thermal_breach_alone_signals_kill_under_kill_policy() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot
            .processes
            .insert(80, make_lifecycle(80, "ai-cool", Some(AICategory::Inference), false));

        // No per-PID breach.
        let breaches = vec![ThresholdBreach {
            pid: 80,
            vram_pct: Some(40.0),
            vram_breached: false,
            ram_pct: Some(50.0),
            ram_breached: false,
        }];
        let host = HostBreach {
            thermal_breached: true,
            max_temp_c: Some(95.0),
            hottest_zone: Some("x86_pkg_temp".into()),
        };
        let decisions = executor.evaluate(&snapshot, &breaches, &host);
        assert_eq!(
            decisions[0].1,
            KillAction::SignalTermSent,
            "host-thermal alone (per-PID quiet) MUST signal kill — \
             Q6 framing: thermal triggers load shedding across AI \
             workloads. Got {:?}",
            decisions[0].1,
        );
        assert!(
            decisions[0].2.contains("host-thermal"),
            "reason string MUST name the thermal trigger so the \
             operator can correlate; got: {:?}",
            decisions[0].2,
        );
    }

    /// THE DEFAULT-OFF INVARIANT EXTENDED. With safe_default()
    /// policy (Allow) — D80/D81 scar layer 1 — a PID breaching ANY
    /// combination of (VRAM, RAM, host-thermal) MUST still produce
    /// `Whitelisted`, not `SignalTermSent`. The policy gate fires
    /// BEFORE the threshold gate. More dimensions doesn't break the
    /// scar.
    #[test]
    fn default_allow_policy_suppresses_kill_across_all_widened_dimensions() {
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            90,
            make_lifecycle(90, "ai-everything-breaching", Some(AICategory::Inference), false),
        );

        // ALL signals firing simultaneously.
        let breaches = vec![ThresholdBreach {
            pid: 90,
            vram_pct: Some(99.0),
            vram_breached: true,
            ram_pct: Some(99.0),
            ram_breached: true,
        }];
        let host = HostBreach {
            thermal_breached: true,
            max_temp_c: Some(95.0),
            hottest_zone: Some("acpitz".into()),
        };
        let decisions = executor.evaluate(&snapshot, &breaches, &host);
        assert_eq!(
            decisions[0].1,
            KillAction::Whitelisted,
            "v1.0.1 scar layer 1: default Allow MUST suppress kill \
             decision regardless of how many breach dimensions are \
             firing. Even with VRAM + RAM + thermal ALL tripped, the \
             policy gate wins. Got {:?}.",
            decisions[0].1,
        );
    }

    /// Skipped covers the "policy permits but NOTHING is breaching"
    /// case. With Kill policy + per-PID quiet + no host thermal,
    /// the verdict is Skipped (not SignalTermSent). Pin the broader
    /// not-breaching surface — pre-D84 this was VRAM-only; now it
    /// must also exclude RAM and thermal.
    #[test]
    fn no_breach_anywhere_under_kill_policy_skips() {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        let mut executor = GovernorExecutor::new(policy);

        let mut snapshot = crate::lifecycle::LifecycleSnapshot::new();
        snapshot.processes.insert(
            91,
            make_lifecycle(91, "ai-quiet", Some(AICategory::Inference), false),
        );

        // Measured but well below all thresholds.
        let breaches = vec![ThresholdBreach {
            pid: 91,
            vram_pct: Some(20.0),
            vram_breached: false,
            ram_pct: Some(15.0),
            ram_breached: false,
        }];
        let decisions = executor.evaluate(
            &snapshot,
            &breaches,
            &HostBreach::default(),
        );
        assert_eq!(
            decisions[0].1,
            KillAction::Skipped,
            "no breach anywhere ⇒ Skipped; got {:?}",
            decisions[0].1,
        );
    }
}
