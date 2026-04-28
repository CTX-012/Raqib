use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::governor::GovernorPolicy;
use crate::governor::policy::PolicyAction;

/// Errors when loading or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config read failed: {0}")]
    Read(#[from] std::io::Error),
    #[error("config parse failed: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Top-level runtime configuration. Loaded from TOML; CLI flags override.
///
/// Defaults intentionally bias toward safety: dry-run is on, the allowlist
/// covers shells and init, and tick rate is conservative.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub runtime: RuntimeConfig,
    pub policy: PolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Sample-and-decide interval in ms. 1000 ms is the documented default.
    pub tick_interval_ms: u64,
    /// UI redraw interval in ms. UI can render between samples using cached state.
    pub render_interval_ms: u64,
    /// Maximum number of completed run summaries kept in memory.
    pub completed_history: usize,
    /// Maximum number of audit-log entries kept in memory for the UI.
    pub audit_history: usize,
    /// Persistent audit log file. Empty string disables file persistence
    /// (the in-memory ring buffer still feeds the UI). CLAUDE.md safety
    /// rule 6 requires a durable trail in enforce mode; defaults point to
    /// a user-writable location.
    pub audit_log_path: String,
    /// Persistent run-summary log file. Same disable-with-empty semantics.
    pub summary_log_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Process names that are never killed by automated policy.
    pub allowlist: HashSet<String>,
    /// Process names always considered kill candidates regardless of category.
    pub blocklist: HashSet<String>,
    /// Default action for AI-classified processes that match neither list.
    pub default_ai_action: PolicyAction,
    /// SIGTERM → SIGKILL grace period in seconds. Minimum 1s.
    pub sigterm_grace_secs: u64,
    /// True = actually send signals; false = log only. Default false.
    pub enforce: bool,
    /// Max automated kills permitted inside `rate_limit_window_secs`.
    /// Mirrors CLAUDE.md safety rule 5 (default 3).
    pub rate_limit_max_kills: u32,
    /// Sliding window for `rate_limit_max_kills` in seconds (default 60).
    pub rate_limit_window_secs: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            tick_interval_ms: 1000,
            render_interval_ms: 100,
            completed_history: 50,
            audit_history: 100,
            audit_log_path: String::new(),
            summary_log_path: String::new(),
        }
    }
}

impl RuntimeConfig {
    /// None when the string is empty (disabled), else the configured path.
    pub fn audit_log(&self) -> Option<PathBuf> {
        if self.audit_log_path.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.audit_log_path))
        }
    }
    pub fn summary_log(&self) -> Option<PathBuf> {
        if self.summary_log_path.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.summary_log_path))
        }
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        let policy = GovernorPolicy::safe_default();
        Self {
            allowlist: policy.whitelist_names,
            blocklist: policy.blacklist_names,
            default_ai_action: policy.default_ai_action,
            sigterm_grace_secs: policy.sigterm_grace_period_secs,
            enforce: policy.enforce,
            rate_limit_max_kills: policy.rate_limit_max_kills,
            rate_limit_window_secs: policy.rate_limit_window_secs,
        }
    }
}

impl Config {
    /// Load TOML config from disk. Missing fields fall back to defaults.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject impossible values that would otherwise cause silent misbehavior.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.runtime.tick_interval_ms == 0 {
            return Err(ConfigError::Invalid(
                "runtime.tick_interval_ms must be > 0".into(),
            ));
        }
        if self.runtime.render_interval_ms == 0 {
            return Err(ConfigError::Invalid(
                "runtime.render_interval_ms must be > 0".into(),
            ));
        }
        // Safety rule from CLAUDE.md: minimum SIGTERM grace is 1 second.
        if self.policy.sigterm_grace_secs < 1 {
            return Err(ConfigError::Invalid(
                "policy.sigterm_grace_secs must be >= 1".into(),
            ));
        }
        Ok(())
    }

    /// Materialize the in-memory `GovernorPolicy` from this config.
    pub fn build_policy(&self) -> GovernorPolicy {
        GovernorPolicy {
            whitelist_names: self.policy.allowlist.clone(),
            blacklist_names: self.policy.blocklist.clone(),
            default_ai_action: self.policy.default_ai_action,
            sigterm_grace_period_secs: self.policy.sigterm_grace_secs,
            enforce: self.policy.enforce,
            rate_limit_max_kills: self.policy.rate_limit_max_kills,
            rate_limit_window_secs: self.policy.rate_limit_window_secs,
        }
    }

    /// Force dry-run regardless of file contents. Used by `--dry-run` flag.
    pub fn force_dry_run(&mut self) {
        self.policy.enforce = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn default_is_dry_run() {
        let cfg = Config::default();
        assert!(
            !cfg.policy.enforce,
            "safety: default config must be dry-run"
        );
    }

    #[test]
    fn default_validates() {
        Config::default().validate().expect("default must validate");
    }

    #[test]
    fn zero_tick_rejected() {
        let mut cfg = Config::default();
        cfg.runtime.tick_interval_ms = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_grace_rejected() {
        let mut cfg = Config::default();
        cfg.policy.sigterm_grace_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn force_dry_run_overrides_enforce() {
        let mut cfg = Config::default();
        cfg.policy.enforce = true;
        cfg.force_dry_run();
        assert!(!cfg.policy.enforce);
    }

    #[test]
    fn build_policy_roundtrips_allowlist() {
        let mut cfg = Config::default();
        cfg.policy.allowlist.insert("my_app".into());
        let pol = cfg.build_policy();
        assert!(pol.whitelist_names.contains("my_app"));
    }

    #[test]
    fn from_file_loads_minimal_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // Empty TOML — every field falls back to default.
        writeln!(f, "[runtime]\ntick_interval_ms = 500").unwrap();
        writeln!(f, "[policy]\nenforce = false").unwrap();
        let cfg = Config::from_file(f.path()).unwrap();
        assert_eq!(cfg.runtime.tick_interval_ms, 500);
        assert!(!cfg.policy.enforce);
    }

    #[test]
    fn from_file_rejects_invalid() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[runtime]\ntick_interval_ms = 0").unwrap();
        assert!(Config::from_file(f.path()).is_err());
    }
}
