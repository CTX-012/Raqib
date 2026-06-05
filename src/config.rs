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
/// Defaults bias toward safety: the allowlist covers shells and init,
/// and tick rate is conservative. Kill confirmation is handled by the
/// TUI's kill_confirm card (CAR-17 / v0.3.8); there is no longer a
/// dry-run policy switch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub runtime: RuntimeConfig,
    pub policy: PolicyConfig,
    pub storage: StorageConfig,
    pub regression: RegressionConfig,
    pub telemetry: TelemetryConfig,
    // Sprint 5 — `pub dashboard: DashboardConfig` removed alongside
    // the Grafana integration hard-delete. `[dashboard]` sections in
    // existing user TOMLs are silently ignored by serde because
    // `Config` is `#[serde(default)]` and doesn't deny unknown fields,
    // so old configs continue to load without warnings.
    pub ui: UiConfig,
    /// v1.3.1 / DISPATCH 53 — hybrid threshold-config overrides.
    /// Every field is `Option<>`; missing values use the contract
    /// defaults from `ux_contract::thresholds`. See
    /// [`crate::thresholds::EffectiveThresholds`] for the resolver
    /// and validation invariants.
    pub thresholds: ThresholdsConfig,
    /// v1.3.1 / DISPATCH 60 — opt-in actuation gate. Default false.
    /// No actuation code reads this yet; the field exists so a
    /// future tick-loop actuation site (DISPATCH 60+ step 5) has a
    /// named, explicit, operator-visible gate to consult. See
    /// [`GovernorConfig`] for the v1.0.1 phantom-kill scar rationale.
    pub governor: GovernorConfig,
}

/// v1.3.1 / DISPATCH 60 step 2 — names the opt-in actuation gate.
///
/// `auto_actuate` is a boolean field; setting it `true` does NOT
/// currently enable any actuation. The field exists so that when a
/// future actuation site lands (DISPATCH 60+ step 5+), it has a
/// single named gate to read instead of inventing one at impl
/// time. Default `false` preserves the v1.0.1 phantom-kill scar:
/// the audit-trail bug that shipped in v1.0.0 was a kill-without-
/// signal because the default policy was `Kill` AND `send_sigterm`
/// was unwired. The v1.0.1 fix flipped `default_ai_action` to
/// `Allow` AND left `send_sigterm` unwired; this gate is a third
/// layer in addition so that even when both of those flips reverse
/// in the future, an operator still needs to flip `auto_actuate`
/// to true to consent to automated actuation.
///
/// Schema-firewall reminder (DISPATCH 60 step 1): this is the ONLY
/// field name on the gate side that doesn't trigger the
/// `config_schema_has_no_action_verb_fields` guard. Field names
/// like `auto_kill`, `action_on_breach`, etc. are CI-rejected by
/// that test. `auto_actuate` is the canonical name; do not rename.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernorConfig {
    /// Opt-in gate for any future automated actuation surface.
    /// Default `false` (the v1.0.1 phantom-kill scar — `bool`'s
    /// `Default::default()` agrees, which is why this struct can
    /// `#[derive(Default)]`). No code reads this yet; flipping to
    /// `true` in your TOML today is a no-op. When the future
    /// actuation site lands, it MUST consult this field AND
    /// `policy.default_ai_action` AND have `send_sigterm` wired
    /// before any signal goes out; all three layers must be
    /// operator-flipped before automated kills can happen. See
    /// [`super::Config::validate`] for the cross-layer comment
    /// pointing forward to where those assertions will live.
    pub auto_actuate: bool,
}

/// v1.3.1 — per-field overrides for the contract's class-2
/// "deployment threshold" constants. Every field is optional; missing
/// values fall back to the contract default in
/// [`crate::thresholds::EffectiveThresholds::resolve`]. Validation
/// (amber < red, critical ≥ attention, range bounds) runs at resolve
/// time — invalid combinations reject at startup with an
/// operator-actionable error rather than silently clamping.
///
/// Class-3 sampler constants (`ROS2_ECHO_PROBE_INTERVAL`,
/// `ROS2_ACTIVITY_STALENESS`, `ROS2_SHELLOUT_TIMEOUT`,
/// `EMBEDDINGS_ACTIVE_CPU_PCT`) are deliberately ABSENT — they are
/// correctness-critical (the v1.1.9 leak-fix cadence invariant lives
/// among them) and remain compile-time const-pinned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThresholdsConfig {
    pub thermal_amber_c: Option<f64>,
    pub thermal_red_c: Option<f64>,
    pub vram_attention_pct: Option<f64>,
    pub vram_critical_pct: Option<f64>,
    pub ram_attention_pct: Option<f64>,
    pub ram_critical_pct: Option<f64>,
    pub kv_attention_pct: Option<f64>,
    pub kv_critical_pct: Option<f64>,
    pub alert_sustain_secs: Option<u64>,
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

// Sprint 5 — `DashboardConfig` struct removed alongside the Grafana
// hard-delete. The `[UX-3]` `g` keybinding it controlled is no longer
// wired (see `src/ui/input.rs`); the field was dropped from `Config`
// just above this comment.

/// Toggles for the optional [`crate::telemetry::Dispatcher`] samplers
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
    /// Phase 2 / DISPATCH 2A — SaaS-LLM CLI activity sampler
    /// (claude-code, cursor, aider, continue). Off the box detects
    /// process tree shape only; no network I/O. Default `true`.
    pub agent_claude: bool,
    /// Phase 2 / DISPATCH 2B — ROS2 topic-rate sampler via
    /// `ros2 topic list` + `ros2 topic hz` shellout. Off the box
    /// inherits a ROS2 host's CLI; falls silent on hosts without it.
    /// Default `true`.
    pub ros2_shellout: bool,
    /// Phase 2 / DISPATCH 2B — embeddings-workload activity from
    /// sustained-CPU heuristic. Pure compute, no new I/O. Default
    /// `true`.
    pub embeddings_cpu: bool,
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
            agent_claude: true,
            ros2_shellout: true,
            embeddings_cpu: true,
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
        // v1.3.1 / DISPATCH 60 step 2 — `governor.auto_actuate` is a
        // boolean gate; both `true` and `false` are valid syntactic
        // values. There is no actuation site reading it today, so no
        // cross-layer invariant assertion fires here. When the future
        // actuation site lands (DISPATCH 60+ step 5+) it will assert
        // its own cross-layer prerequisites (`policy.default_ai_action`,
        // `send_sigterm` wiring) at the actuation site rather than
        // here, so this validator stays observation-side.
        let _ = self.governor.auto_actuate;
        Ok(())
    }

    /// Materialize the in-memory `GovernorPolicy` from this config.
    pub fn build_policy(&self) -> GovernorPolicy {
        GovernorPolicy {
            whitelist_names: self.policy.allowlist.clone(),
            blacklist_names: self.policy.blocklist.clone(),
            default_ai_action: self.policy.default_ai_action,
            sigterm_grace_period_secs: self.policy.sigterm_grace_secs,
            rate_limit_max_kills: self.policy.rate_limit_max_kills,
            rate_limit_window_secs: self.policy.rate_limit_window_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        let cfg = Config::from_file(f.path()).unwrap();
        assert_eq!(cfg.runtime.tick_interval_ms, 500);
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

    /// v1.3.1 / DISPATCH 60 step 2 — `governor.auto_actuate` defaults
    /// to `false` so an operator who never opens the TOML never opts
    /// into anything. The v1.0.1 phantom-kill scar lives at this
    /// default.
    #[test]
    fn governor_auto_actuate_defaults_to_false() {
        let cfg = Config::default();
        assert!(
            !cfg.governor.auto_actuate,
            "default Config must have governor.auto_actuate = false \
             (v1.0.1 phantom-kill scar). Found: {}",
            cfg.governor.auto_actuate,
        );
        // And the standalone GovernorConfig default agrees.
        assert!(!GovernorConfig::default().auto_actuate);
    }

    /// Validation accepts both boolean values. The gate is named but
    /// not yet wired; rejecting one value here would force operators
    /// to flip in lockstep with a future actuation site that doesn't
    /// exist yet.
    #[test]
    fn governor_auto_actuate_both_values_validate() {
        let mut cfg = Config::default();
        cfg.governor.auto_actuate = false;
        cfg.validate().expect("auto_actuate=false must validate");
        cfg.governor.auto_actuate = true;
        cfg.validate().expect(
            "auto_actuate=true must validate today (gate is named, \
             not yet wired). When a future step ties it to other \
             safety knobs, the cross-layer assertion lives at the \
             actuation site, not in this validator.",
        );
    }

    /// TOML round-trip: serialize a config with `auto_actuate = true`,
    /// deserialize, and confirm the value survives. Also covers the
    /// section name (`[governor]`) so a future schema split doesn't
    /// silently rename the section under a still-passing test.
    #[test]
    fn governor_auto_actuate_round_trips_through_toml() {
        let mut original = Config::default();
        original.governor.auto_actuate = true;

        let serialized = toml::to_string(&original)
            .expect("serialize default+auto_actuate=true config");
        assert!(
            serialized.contains("[governor]"),
            "serialized TOML must include a `[governor]` section \
             header; got:\n{serialized}",
        );
        assert!(
            serialized.contains("auto_actuate"),
            "serialized TOML must include the `auto_actuate` key; \
             got:\n{serialized}",
        );

        let parsed: Config =
            toml::from_str(&serialized).expect("round-trip deserialize");
        assert!(
            parsed.governor.auto_actuate,
            "round-tripped Config must preserve auto_actuate=true",
        );

        // A TOML with the field explicitly set to false also round-
        // trips, distinguishing "unset (default false)" from
        // "explicit false."
        let mut off = Config::default();
        off.governor.auto_actuate = false;
        let off_toml = toml::to_string(&off).unwrap();
        let off_parsed: Config = toml::from_str(&off_toml).unwrap();
        assert!(!off_parsed.governor.auto_actuate);
    }

    /// v1.3.1 / DISPATCH 60 step 2 — pinning the v1.0.1 phantom-kill
    /// scar. Setting `governor.auto_actuate = true` MUST NOT mutate
    /// any of the three prior safety layers (default action stays
    /// Allow, sigterm_grace stays positive, rate_limit stays bounded).
    /// The gate is a NEW layer, not a replacement for the existing
    /// ones. A future tick-path actuation site must consult ALL
    /// FOUR layers (this gate + the three legacy ones); this test
    /// pins the no-side-effect property at the schema layer.
    #[test]
    fn governor_auto_actuate_does_not_modify_existing_safety_knobs() {
        let baseline = Config::default();
        let mut flipped = Config::default();
        flipped.governor.auto_actuate = true;

        assert_eq!(
            baseline.policy.default_ai_action,
            flipped.policy.default_ai_action,
            "flipping auto_actuate must not change default_ai_action \
             (the v1.0.1 scar layer 1)",
        );
        assert_eq!(
            baseline.policy.sigterm_grace_secs,
            flipped.policy.sigterm_grace_secs,
            "flipping auto_actuate must not change sigterm_grace_secs \
             (CLAUDE.md safety rule 2)",
        );
        assert_eq!(
            baseline.policy.rate_limit_max_kills,
            flipped.policy.rate_limit_max_kills,
            "flipping auto_actuate must not change rate_limit_max_kills \
             (CLAUDE.md safety rule 3)",
        );
        assert_eq!(
            baseline.policy.rate_limit_window_secs,
            flipped.policy.rate_limit_window_secs,
            "flipping auto_actuate must not change rate_limit_window_secs",
        );
    }
}
