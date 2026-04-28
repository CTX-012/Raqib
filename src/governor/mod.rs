use crate::model::AICategory;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod audit;
pub mod executor;
pub mod manual;
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
    /// SIGTERM sent; waiting for graceful shutdown.
    SignalTermSent,
    /// Process did not exit after SIGTERM; SIGKILL sent.
    SignalKillSent,
    /// Dry-run mode; would send SIGTERM if enabled.
    DryRunTermWould,
    /// Dry-run mode; would send SIGKILL if enabled.
    DryRunKillWould,
    /// The per-window kill budget has been exhausted; no action taken.
    /// Protects against kill-storm misfires (CLAUDE.md safety rule 5).
    RateLimited,
    /// Skipped for other reasons (not AI, etc.).
    Skipped,
}

/// Tracks a process pending termination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingKill {
    pub pid: u32,
    pub name: String,
    pub category: AICategory,
    pub sigterm_time: DateTime<Utc>,
    pub sigkill_time: Option<DateTime<Utc>>,
}

impl PendingKill {
    pub fn new(pid: u32, name: String, category: AICategory) -> Self {
        Self {
            pid,
            name,
            category,
            sigterm_time: Utc::now(),
            sigkill_time: None,
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
