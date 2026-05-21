use crate::model::AICategory;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Governance action for a specific AI category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAction {
    /// Allow the process to run (whitelisted).
    Allow,
    /// Kill the process with SIGTERM→SIGKILL.
    Kill,
}

/// Policy defining which processes can run and which should be killed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorPolicy {
    /// Process names (exact match) that are always whitelisted.
    pub whitelist_names: HashSet<String>,
    /// Process names (exact match) that should be killed.
    pub blacklist_names: HashSet<String>,
    /// Default action for AI processes not in whitelist/blacklist.
    pub default_ai_action: PolicyAction,
    /// Grace period (seconds) between SIGTERM and SIGKILL.
    pub sigterm_grace_period_secs: u64,
    /// Maximum number of automated kills the governor may issue in any
    /// `rate_limit_window_secs` span. Safety rule 5 in CLAUDE.md; guards
    /// against runaway kill storms when a policy misfires.
    pub rate_limit_max_kills: u32,
    /// Sliding-window size (seconds) for `rate_limit_max_kills`.
    pub rate_limit_window_secs: u64,
}

impl GovernorPolicy {
    /// Create a safe default policy (whitelist-first).
    pub fn safe_default() -> Self {
        Self {
            whitelist_names: HashSet::from([
                "sshd".to_string(),
                "bash".to_string(),
                "zsh".to_string(),
                "sh".to_string(),
                "systemd".to_string(),
                "init".to_string(),
                "kworker".to_string(),
                "kthreadd".to_string(),
            ]),
            blacklist_names: HashSet::new(),
            // v1.0.1 B-NEW-1 — default flipped from Kill to Allow.
            // Inspector #1 found that `evaluate()` ran with Kill but
            // `send_sigterm` was never wired from the runtime tick
            // loop, producing audit entries with `success=true` for
            // kills that never happened. Flipping the default to
            // Allow closes the phantom-kill gap until the policy
            // semantics + actual send-signal path are wired in a
            // future minor release. Operators who want automated
            // kill enable it via `policy.default_ai_action = "Kill"`
            // in `edge_monitor.toml` — documented in the help
            // overlay (FIX 10) and edge_monitor.toml.example.
            default_ai_action: PolicyAction::Allow,
            sigterm_grace_period_secs: 5,
            // CLAUDE.md safety rule 5: max 3 automated kills per 60s window.
            rate_limit_max_kills: 3,
            rate_limit_window_secs: 60,
        }
    }

    /// Get action for a process based on policy.
    pub fn evaluate(&self, name: &str, category: Option<AICategory>) -> PolicyAction {
        // Whitelist takes priority: always allow
        if self.whitelist_names.contains(name) {
            return PolicyAction::Allow;
        }

        // Blacklist: always kill
        if self.blacklist_names.contains(name) {
            return PolicyAction::Kill;
        }

        // Non-AI processes: allow by default
        let Some(cat) = category else {
            return PolicyAction::Allow;
        };

        // AI processes: use default action
        match cat {
            AICategory::Inference => self.default_ai_action,
            AICategory::Training => self.default_ai_action,
            AICategory::ModelDownload => self.default_ai_action,
            AICategory::Framework => self.default_ai_action,
            AICategory::NotAi => PolicyAction::Allow,
        }
    }

    /// Add process name to whitelist.
    pub fn whitelist(&mut self, name: impl Into<String>) {
        self.whitelist_names.insert(name.into());
    }

    /// Add process name to blacklist.
    pub fn blacklist(&mut self, name: impl Into<String>) {
        self.blacklist_names.insert(name.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_safe_default() {
        let policy = GovernorPolicy::safe_default();
        assert!(policy.whitelist_names.contains("bash"));
        assert!(policy.whitelist_names.contains("sshd"));
        // v1.0.1 B-NEW-1 — flipped from Kill to Allow to close the
        // phantom-kill gap (Inspector #1). Operators opt in via
        // `policy.default_ai_action = "Kill"` in edge_monitor.toml.
        assert_eq!(policy.default_ai_action, PolicyAction::Allow);
    }

    #[test]
    fn governor_default_ai_action_is_allow_v1_0_1() {
        // v1.0.1 B-NEW-1 regression guard. Explicit name so a
        // grep for "phantom kill" or "B-NEW-1" finds this test.
        let policy = GovernorPolicy::safe_default();
        assert_eq!(
            policy.default_ai_action,
            PolicyAction::Allow,
            "v1.0.1 default must be Allow until send_sigterm is wired",
        );
    }

    #[test]
    fn policy_whitelist_priority() {
        let policy = GovernorPolicy::safe_default();
        // Even if we had a rule to kill, whitelist takes priority
        let action = policy.evaluate("bash", Some(AICategory::Training));
        assert_eq!(action, PolicyAction::Allow);
    }

    #[test]
    fn policy_blacklist() {
        let mut policy = GovernorPolicy::safe_default();
        policy.blacklist("dangerous_ai");
        let action = policy.evaluate("dangerous_ai", Some(AICategory::Inference));
        assert_eq!(action, PolicyAction::Kill);
    }

    #[test]
    fn policy_default_ai_action() {
        let policy = GovernorPolicy::safe_default();
        let action = policy.evaluate("unknown_ai", Some(AICategory::Inference));
        assert_eq!(action, policy.default_ai_action);
    }

    #[test]
    fn policy_non_ai_always_allowed() {
        let policy = GovernorPolicy::safe_default();
        let action = policy.evaluate("random_process", None);
        assert_eq!(action, PolicyAction::Allow);
    }

    #[test]
    fn policy_add_whitelist() {
        let mut policy = GovernorPolicy::safe_default();
        policy.whitelist("my_important_app");
        let action = policy.evaluate("my_important_app", Some(AICategory::Training));
        assert_eq!(action, PolicyAction::Allow);
    }
}
