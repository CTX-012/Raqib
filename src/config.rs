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
    pub storage: StorageConfig,
    pub regression: RegressionConfig,
    pub telemetry: TelemetryConfig,
    pub dashboard: DashboardConfig,
    pub ui: UiConfig,
}

/// TUI presentation knobs. Today this is only the §13 theme name; future
/// rows that add presentation toggles (e.g. dim-mode, no-color override)
/// land here so they share one `[ui]` section in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// One of `dark` / `light` / `high-contrast` (or `high_contrast`).
    /// Case-insensitive. Unknown values fall back to `dark` at render
    /// time — the validator only warns rather than rejecting so a
    /// freshly-pulled `--theme` value that this binary doesn't know
    /// yet still lets the operator launch the TUI.
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
        }
    }
}

/// [UX-3] — what the `g` keybinding opens in the user's default
/// browser. Empty `url_template` disables the keybinding (the TUI
/// prints a status hint pointing at this section). Substitution
/// tokens supported:
///
/// * `{model}` — focused workload's `model_name`, empty if unknown
/// * `{pid}`   — focused workload's PID as a decimal integer
///
/// Static URLs (no tokens) are accepted — some operators just want a
/// fixed dashboard link for the box.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    pub url_template: String,
}

/// Toggles for the optional [`telemetry::Dispatcher`] samplers
/// (latest.md cross-cutting requirements + Tier 1.2). All on by
/// default; sampling is cheap on idle (HTTP scrapers fail fast on
/// connection refused) and the dispatcher's `applies_to` gate keeps
/// non-AI processes from being touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    pub vllm_scrape: bool,
    pub llamacpp_scrape: bool,
    pub ollama_api: bool,
    /// Empty disables the in-process Prometheus exporter.
    /// Format: `host:port` (e.g. `127.0.0.1:9472`). Tier 2.3.
    pub prometheus_bind: String,
    /// Empty disables the Tier 3.6 vision probe socket. Otherwise a
    /// Unix-domain stream socket path users can connect to to push
    /// frame timestamps from their vision inference loops.
    pub vision_probe_socket: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            vllm_scrape: true,
            llamacpp_scrape: true,
            ollama_api: true,
            prometheus_bind: String::new(),
            vision_probe_socket: String::new(),
        }
    }
}

/// Knobs for the regression detector that runs at every AI process exit
/// (Tier 1.3). Defaults match latest.md's text — 10% warn / 25% critical
/// / 10-run rolling baseline / refuse to flag below 3 samples. Set
/// `min_baseline_samples = u32::MAX` to disable detection entirely
/// without removing the config block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RegressionConfig {
    pub warn_pct: f32,
    pub critical_pct: f32,
    pub baseline_window: u32,
    pub min_baseline_samples: u32,
    /// Per-metric central-tendency strategy (latest.md / [C-5]).
    /// `"mean"` (default, historical) or `"median"` (robust to a single
    /// bad run). Validated case-insensitively at load time.
    pub baseline_strategy: String,
    /// Drop runs whose key metric is >2σ from the median before
    /// computing the baseline. The flagged ids still surface on
    /// `Baseline.outlier_run_ids` so a reviewer can see them. Default
    /// `false` (historical).
    pub drop_outliers: bool,
}

impl Default for RegressionConfig {
    fn default() -> Self {
        Self {
            warn_pct: 10.0,
            critical_pct: 25.0,
            baseline_window: 10,
            min_baseline_samples: 3,
            baseline_strategy: "mean".to_string(),
            drop_outliers: false,
        }
    }
}

impl RegressionConfig {
    /// Resolve the string-form `baseline_strategy` into the typed
    /// enum from `analysis::compare`. Returns the historical default
    /// (`Mean`) on validation failure; `validate()` is the gate that
    /// rejects bad input early.
    pub fn strategy(&self) -> crate::analysis::compare::BaselineStrategy {
        match self.baseline_strategy.to_ascii_lowercase().as_str() {
            "median" => crate::analysis::compare::BaselineStrategy::Median,
            _ => crate::analysis::compare::BaselineStrategy::Mean,
        }
    }
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

/// Storage paths for the typed run store + ancillary caches.
/// `run_store_path` defaults to `~/.local/share/edge_monitor` so the
/// history subcommand and the regression detector "just work" without
/// any config; set to `""` to disable persistence entirely (in-memory
/// only, useful for tests and the headless CI smoke run).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub run_store_path: String,
    pub fingerprint_cache: String,
    /// Hard cap per model. Pruning is implemented in Tier 1.1+; this
    /// field reserves the config slot now so future versions don't need
    /// a schema bump.
    pub keep_runs_per_model: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            run_store_path: default_run_store_path(),
            fingerprint_cache: default_fingerprint_cache(),
            keep_runs_per_model: 200,
        }
    }
}

impl StorageConfig {
    /// Returns the configured `run_store_path` with `~/` expanded to
    /// `$HOME`. Returns `None` when the path is empty (persistence
    /// disabled).
    pub fn run_store(&self) -> Option<PathBuf> {
        if self.run_store_path.is_empty() {
            None
        } else {
            Some(expand_tilde(&self.run_store_path))
        }
    }
}

fn default_run_store_path() -> String {
    "~/.local/share/edge_monitor".to_string()
}

fn default_fingerprint_cache() -> String {
    "~/.cache/edge_monitor/fingerprints.json".to_string()
}

/// Replace a leading `~/` with `$HOME/`. Returns the original path
/// unchanged when `$HOME` is unset or the path doesn't start with `~/`.
pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
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
        if self.storage.keep_runs_per_model == 0 {
            return Err(ConfigError::Invalid(
                "storage.keep_runs_per_model must be > 0 (disable persistence \
                 by setting storage.run_store_path = \"\" instead)"
                    .into(),
            ));
        }
        if !self.regression.warn_pct.is_finite() || self.regression.warn_pct < 0.0 {
            return Err(ConfigError::Invalid(
                "regression.warn_pct must be a non-negative finite number".into(),
            ));
        }
        if !self.regression.critical_pct.is_finite() || self.regression.critical_pct < 0.0 {
            return Err(ConfigError::Invalid(
                "regression.critical_pct must be a non-negative finite number".into(),
            ));
        }
        if self.regression.critical_pct < self.regression.warn_pct {
            return Err(ConfigError::Invalid(
                "regression.critical_pct must be >= warn_pct".into(),
            ));
        }
        if self.regression.baseline_window == 0 {
            return Err(ConfigError::Invalid(
                "regression.baseline_window must be > 0".into(),
            ));
        }
        match self.regression.baseline_strategy.to_ascii_lowercase().as_str() {
            "mean" | "median" => {}
            other => {
                return Err(ConfigError::Invalid(format!(
                    "regression.baseline_strategy must be \"mean\" or \"median\", got {other:?}"
                )));
            }
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

    #[test]
    fn storage_defaults_use_home_relative_paths() {
        let cfg = Config::default();
        assert!(cfg.storage.run_store_path.starts_with("~/"));
        assert_eq!(cfg.storage.keep_runs_per_model, 200);
    }

    #[test]
    fn storage_run_store_returns_none_when_empty() {
        let mut cfg = Config::default();
        cfg.storage.run_store_path.clear();
        assert!(cfg.storage.run_store().is_none());
    }

    #[test]
    fn storage_keep_zero_rejected() {
        let mut cfg = Config::default();
        cfg.storage.keep_runs_per_model = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_critical_below_warn_rejected() {
        let mut cfg = Config::default();
        cfg.regression.warn_pct = 25.0;
        cfg.regression.critical_pct = 10.0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_negative_threshold_rejected() {
        let mut cfg = Config::default();
        cfg.regression.warn_pct = -1.0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_zero_window_rejected() {
        let mut cfg = Config::default();
        cfg.regression.baseline_window = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_baseline_strategy_default_is_mean() {
        let cfg = Config::default();
        assert_eq!(cfg.regression.baseline_strategy, "mean");
        assert!(!cfg.regression.drop_outliers);
        assert_eq!(
            cfg.regression.strategy(),
            crate::analysis::compare::BaselineStrategy::Mean
        );
    }

    #[test]
    fn regression_baseline_strategy_median_resolves() {
        let mut cfg = Config::default();
        cfg.regression.baseline_strategy = "median".to_string();
        cfg.validate().expect("median is a valid strategy");
        assert_eq!(
            cfg.regression.strategy(),
            crate::analysis::compare::BaselineStrategy::Median
        );
    }

    #[test]
    fn regression_baseline_strategy_is_case_insensitive() {
        let mut cfg = Config::default();
        cfg.regression.baseline_strategy = "MEDIAN".to_string();
        cfg.validate().expect("MEDIAN normalises to median");
        cfg.regression.baseline_strategy = "Mean".to_string();
        cfg.validate().expect("Mean normalises to mean");
    }

    #[test]
    fn regression_unknown_strategy_rejected() {
        let mut cfg = Config::default();
        cfg.regression.baseline_strategy = "harmonic".to_string();
        let err = cfg.validate().expect_err("unknown strategy must error");
        assert!(format!("{err}").contains("baseline_strategy"));
    }
}
