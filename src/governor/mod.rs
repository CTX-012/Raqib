use crate::model::AICategory;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::os::fd::OwnedFd;
use std::sync::Arc;
use thiserror::Error;

pub mod audit;
pub mod executor;
pub mod manual;
pub mod pid_reuse;
pub mod policy;

pub use audit::AuditWriter;
pub use executor::GovernorExecutor;
pub use manual::ManualKiller;
pub use policy::GovernorPolicy;

#[derive(Debug, Clone, Error)]
pub enum GovernorError {
    #[error("kill failed: {0}")]
    KillFailed(String),
    #[error("signal error: {0}")]
    SignalError(String),
    #[error("governance error: {0}")]
    GovernanceError(String),
}

pub type GovernorResult<T> = Result<T, GovernorError>;

/// Action taken by the governor on a single process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillAction {
    /// Process is whitelisted; not touched.
    Whitelisted,
    /// Process is already exited; nothing to do.
    AlreadyExited,
    /// v1.3.2 / DISPATCH 77 / 62-E — process already has an
    /// outstanding SIGTERM that hasn't yet been reaped. Returned
    /// by `evaluate()` for any PID still in `pending_kills` so a
    /// stubborn post-SIGTERM workload doesn't re-emit
    /// `SignalTermSent` every tick (which would drain the
    /// 3-per-60s rate-limit budget alone over three ticks,
    /// starving every other AI process that needs a fresh kill).
    /// Symmetric with [`Self::AlreadyExited`] — same position in
    /// `evaluate_process`, same "don't act, just observe" intent.
    ///
    /// AUTHORITY: this is a DECISION discriminator, NOT an
    /// action. It records "we know we already SIGTERM'd this PID"
    /// — it does not itself send a signal. Step-0 prerequisite
    /// for the actuation arc per
    /// `docs/PHASE4_AUTOKILL_DESIGN.md`; step-5 actuation reads
    /// this variant to skip pending PIDs, but the actuation
    /// wiring itself stays unbuilt until that dispatch lands.
    AlreadyPending,
    /// SIGTERM sent; waiting for graceful shutdown.
    SignalTermSent,
    /// Process did not exit after SIGTERM; SIGKILL sent.
    SignalKillSent,
    /// The per-window kill budget has been exhausted; no action taken.
    /// Protects against kill-storm misfires (CLAUDE.md safety rule 5).
    RateLimited,
    /// SIGKILL refused because the captured pidfd / starttime no longer
    /// matches the live process at this PID. Emitted by the PID-reuse
    /// guard (TEST.md G.1.11): the original process exited during the
    /// grace period and either reaped (no /proc entry) or was succeeded
    /// by an unrelated process at the recycled PID. Refusing the kill
    /// is mandatory — see CLAUDE.md safety rule 1.
    PidReusedAborted,
    /// Skipped for other reasons (not AI, etc.).
    Skipped,
}

/// Tracks a process pending termination.
///
/// Carries two PID-identity tokens captured at SIGTERM time so the
/// SIGKILL escalation can refuse to fire on a recycled PID:
///
/// - `pidfd`: a Linux 5.3+ pidfd that pins this exact process instance.
///   When present, SIGKILL is sent through `pidfd_send_signal` and the
///   kernel guarantees no race with PID reuse.
/// - `starttime_ticks`: kernel-clock-tick start time from
///   `/proc/<pid>/stat` field 22. The fallback when pidfd is unavailable
///   (kernel <5.3, restricted user namespace, etc.). Re-read at SIGKILL
///   time and compared; mismatch → PID was recycled, abort the kill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingKill {
    pub pid: u32,
    pub name: String,
    pub category: AICategory,
    pub sigterm_time: DateTime<Utc>,
    pub sigkill_time: Option<DateTime<Utc>>,
    /// Linux pidfd captured at SIGTERM. `Arc` so `PendingKill` can
    /// derive `Clone` for the `get_pending_kills()` accessor without
    /// dup'ing the underlying file descriptor. Skipped during
    /// (de)serialization — pidfds are only meaningful inside the
    /// running process that opened them.
    #[serde(skip)]
    pub pidfd: Option<Arc<OwnedFd>>,
    /// `/proc/<pid>/stat` field 22 captured at SIGTERM time.
    /// `None` if `/proc` was unreadable when the entry was created.
    pub starttime_ticks: Option<u64>,
}

impl PendingKill {
    pub fn new(pid: u32, name: String, category: AICategory) -> Self {
        Self {
            pid,
            name,
            category,
            sigterm_time: Utc::now(),
            sigkill_time: None,
            pidfd: None,
            starttime_ticks: None,
        }
    }

    /// Time elapsed since SIGTERM was sent.
    pub fn elapsed_since_term(&self) -> Duration {
        Utc::now() - self.sigterm_time
    }

    /// Whether enough time has passed to send SIGKILL.
    pub fn should_send_kill(&self, grace_period: Duration) -> bool {
        self.sigkill_time.is_none() && self.elapsed_since_term() >= grace_period
    }
}

/// Governor decision for a single process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorDecision {
    pub pid: u32,
    pub name: String,
    pub action: KillAction,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_kill_new() {
        let pk = PendingKill::new(100, "test".to_string(), AICategory::Inference);
        assert_eq!(pk.pid, 100);
        assert_eq!(pk.name, "test");
        assert_eq!(pk.category, AICategory::Inference);
        assert!(pk.sigkill_time.is_none());
    }

    #[test]
    fn pending_kill_elapsed() {
        let pk = PendingKill::new(100, "test".to_string(), AICategory::Training);
        let elapsed = pk.elapsed_since_term();
        assert!(elapsed >= Duration::seconds(0));
    }

    #[test]
    fn pending_kill_should_send_kill() {
        let pk = PendingKill::new(100, "test".to_string(), AICategory::Framework);
        // Immediately after creation, should not need kill (grace period not passed)
        assert!(!pk.should_send_kill(Duration::seconds(10)));
    }

    #[test]
    fn governor_decision() {
        let decision = GovernorDecision {
            pid: 200,
            name: "proc".to_string(),
            action: KillAction::Whitelisted,
            reason: "in whitelist".to_string(),
        };

        assert_eq!(decision.pid, 200);
        assert_eq!(decision.action, KillAction::Whitelisted);
    }
}
