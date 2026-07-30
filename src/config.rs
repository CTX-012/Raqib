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
    /// v1.3.2 / DISPATCH 57 — per-workload suppression rules. Empty
    /// vec by default. Each rule names a process (exact `comm`
    /// match) and may suppress its alerts and/or recommendations.
    /// SCHEMA FIREWALL: every field on [`WorkloadRule`] is
    /// observation-side — there is no action verb. The OOM-detected
    /// alert is un-suppressable (see [`Self::resolve_workload_rules`]
    /// and `runtime::observe_alerts`'s `OomDetected` carve-out).
    pub workloads: Vec<WorkloadRule>,
    /// v1.3.2 / DISPATCH 85 — web-companion auth + open-access
    /// opt-out. Default: empty token + `allow_no_auth = false` →
    /// the binary REFUSES TO START with an operator-actionable
    /// message. Pre-D85 the web UI was unconditionally open ("NO
    /// AUTH, trusted LAN only"); D85 makes "no auth" a CONSCIOUS
    /// choice (set `web.allow_no_auth = true` to keep the legacy
    /// behavior), not a silent default. See [`WebConfig`].
    pub web: WebConfig,
}

/// v1.3.2 / DISPATCH 85 — shared-bearer-token auth for the web
/// companion. A single secret string, required on every `/api/*`
/// request as `Authorization: Bearer <token>`. No login page, no
/// sessions, no per-user. This is "you need the shared secret to
/// hit the API at all" — casual same-LAN lockout, NOT remote-
/// hardening (no TLS, no session expiry, no rate-limit by user).
///
/// ## Default behavior
///
/// Empty `auth_token` + `allow_no_auth = false` ⇒ `Config::validate`
/// REJECTS. The binary refuses to start with an operator-actionable
/// message naming both fields. This forces the operator to make a
/// CONSCIOUS choice: either set the token, or explicitly opt into
/// open access. Pre-D85 the web UI was unconditionally open with
/// only a `tracing::warn!` line — easy to miss in production logs.
///
/// ## SCHEMA-firewall posture
///
/// `auth_token` is a SECRET STRING, NOT an action verb. The schema
/// firewall (`config_schema_has_no_action_verb_fields`) scans for
/// action-verb tokens like `kill_when`, `enforce_kill`,
/// `auto_kill` — neither `auth_token` nor `allow_no_auth` matches
/// those patterns. The web stays a policy editor / dashboard, not
/// a kill driver: this auth gate protects the EXISTING surface; it
/// does NOT add any kill-triggering route. The D80/D81 invariant
/// "web stays OUT of the kill path" is unchanged by D85.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Shared bearer token. Required on every `/api/*` request as
    /// `Authorization: Bearer <token>`. Compared with the operator-
    /// supplied header via `subtle::ConstantTimeEq` (constant-time
    /// — avoids the timing oracle a naive `==` would leak).
    ///
    /// Empty by default (`String::default()` ⇒ `""`) — the operator
    /// MUST either set a token or flip `allow_no_auth = true`. The
    /// token is NEVER logged, echoed in error responses, or
    /// persisted to the audit trail.
    pub auth_token: String,
    /// Explicit opt-out for the auth gate. Set to `true` to
    /// preserve the pre-D85 "NO AUTH, trusted LAN only" posture
    /// (existing deployments). Logged as a loud `warn!` at server
    /// startup so the operator can't accidentally lose track of
    /// the choice.
    ///
    /// Default `false` (`bool::default()`) — combined with an empty
    /// `auth_token`, this means a fresh install REFUSES TO START
    /// until the operator makes the call. The legacy silent-open
    /// path is gone.
    pub allow_no_auth: bool,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernorConfig {
    /// Opt-in gate for the automated actuation surface (DISPATCH 80).
    /// Default `false` (the v1.0.1 phantom-kill scar). Setting this
    /// to `true` IS the operator's consent to automated kills —
    /// when `true`, [`crate::runtime::Runtime::record_governor_audit`]
    /// walks `state.decisions` for `KillAction::SignalTermSent` and
    /// calls [`crate::governor::GovernorExecutor::send_sigterm`]
    /// for any PID whose VRAM breach has persisted at least
    /// [`Self::kill_sustain_secs`]. Default-OFF means the shipped
    /// binary is byte-identical to v1.3.2's observe-only behaviour;
    /// an operator must NAME the verb in their TOML to cross the
    /// line. Pinned by `default_off_emits_zero_kills` (the headline
    /// regression guard).
    pub auto_actuate: bool,
    /// DISPATCH 80 / Q3 — sustain window for the auto-kill path.
    /// A VRAM-breach must persist at least this many seconds before
    /// the actuation site fires a SIGTERM. Default **10 s**.
    ///
    /// Validated `>= thresholds.alert_sustain_secs` at config load:
    /// a kill MUST NOT undercut the alert-smoothing window. The
    /// operator sees the alert first; only sustained breaches
    /// escalate to kill. A briefly-flashing breach (e.g. a model
    /// load that spikes VRAM for one tick) must never kill.
    ///
    /// Independent of `policy.sigterm_grace_secs` (which gates the
    /// SIGTERM → SIGKILL escalation, a separate after-kill timer).
    /// This is the BEFORE-kill timer.
    pub kill_sustain_secs: u64,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            auto_actuate: false,
            // DISPATCH 80 / Q3 — 10 s default. Matches the
            // design-doc text and exceeds the contract's 5 s
            // ALERT_SUSTAIN_SECS default, so a fresh-install
            // config validates without operator intervention.
            kill_sustain_secs: 10,
        }
    }
}

/// v1.3.2 / DISPATCH 57 — per-workload suppression rule.
///
/// EXACTLY 3 fields. Adding a 4th — especially anything that names
/// an action verb (`auto_kill`, `action_on_breach`, etc.) — is
/// CI-rejected by two independent guards:
///
///   * `tests/config_schema_firewall.rs` (DISPATCH 60 C1) — scans
///     `src/config.rs` for the forbidden token set. `name`,
///     `suppress_alerts`, `suppress_recommendations` are all
///     non-action nouns and clear it.
///   * `tests/workload_rule_field_count_guard.rs` (this dispatch) —
///     counts the fields on this struct. Adding a 4th field
///     trips it, even if the new field is benign.
///
/// The combination is by design: schema-firewall keeps action
/// verbs out, field-count guard keeps even non-verb additions
/// from sliding in without a deliberate decision. This struct is
/// the *only* place per-workload behaviour can be tuned; growing
/// it requires both guards to be updated, both deliberately.
///
/// ## Match semantics
///
/// `name` is matched against the process `comm` (Linux 15-char
/// truncated command, see `/proc/<pid>/comm`). Exact match,
/// case-sensitive. Q5 LOCKED: rule names >15 chars warn at
/// startup but ARE accepted — a process can self-set its own
/// `comm` via `prctl(PR_SET_NAME)` to a longer string the kernel
/// then truncates, so a longer rule name is sometimes
/// intentional. The warning gives the operator one chance to
/// notice the typo path.
///
/// ## OOM carve-out
///
/// `suppress_alerts = true` does NOT silence `AlertId::OomDetected`.
/// OOM is the first brick in the actuation safety wall — a
/// workload that was killed by the kernel's OOM-killer must
/// always surface, even when the operator silenced its routine
/// pressure alerts. See `runtime::observe_alerts` for the
/// `OomDetected` carve-out.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkloadRule {
    /// Process `comm` to match (exact, case-sensitive). Empty
    /// strings are rejected at resolve time; duplicates across
    /// rules are also rejected — a single workload should not be
    /// configured twice. Rules naming a not-currently-running
    /// workload are accepted silently (the rule lights up when /
    /// if the workload appears).
    pub name: String,
    /// When true, the workload's alerts are NOT recorded via
    /// `AlertState::observe`. The OOM-detected alert is the sole
    /// carve-out and ALWAYS fires regardless of this flag.
    pub suppress_alerts: bool,
    /// When true, the workload's recommendations are NOT projected
    /// from `AlertEntry` to `Recommendation`. Independent of
    /// `suppress_alerts`: a workload can show alerts but be muted
    /// from the suggested-action surface, or vice versa. Q6 lock:
    /// setting both to true emits an `info!` at resolve time
    /// noting the redundancy.
    pub suppress_recommendations: bool,
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
    /// v1.3.2 / DISPATCH 89 / PHASE 5 step 0 — per-PID trajectory ring
    /// bound. Caps the number of samples retained per live AI PID in
    /// the History.trajectories cell. Default **1800** ≈ 30 min @ 1 Hz
    /// (matches `tick_interval_ms = 1000`).
    ///
    /// Memory math (see [`docs/PHASE5_HISTORY_DESIGN.md`]): 32 B/sample
    /// × 32 worst-case AI PIDs × this cap. Default settings ≈ 1.84 MB.
    /// Upper guard at 18000 (10× default) capped at ~18.4 MB worst
    /// case — prevents a fat-finger config from requesting gigabytes.
    ///
    /// **Pure config plumbing for D89; no readers in production yet.**
    /// The trajectory CAPTURE site (PHASE 5 step 3) is a future
    /// dispatch; this field exists today so the doc-locked default
    /// has a single named home.
    pub history_trajectory_samples_per_pid: usize,
    /// v1.3.2 / DISPATCH 89 / PHASE 5 step 0 — event-archive ring
    /// bound. Caps the cross-PID `HistoryEvent` ring that aggregates
    /// exit / kill / regression events for the history view. Default
    /// **500** (~150 KB at ~300 B/entry) covers ~1 hour of busy
    /// operator activity.
    ///
    /// Additive structure: the existing `audit_history` /
    /// `completed_history` rings (cap 100 / 50) feed the LIVE
    /// activity panel + wire snapshot at the locked
    /// `ACTIVITY_FEED_WIRE_MAX = 50` cap; this NEW archive is a
    /// SEPARATE structure read only on demand by the future history
    /// view. The live wire is NOT bloated by raising this cap.
    ///
    /// Upper guard at 5000 (10× default) caps the structure at
    /// ~1.5 MB worst case.
    ///
    /// **Pure config plumbing for D89; no readers in production yet.**
    pub history_event_archive_cap: usize,
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
            // v1.3.2 / DISPATCH 89 / PHASE 5 step 0 — doc-locked
            // defaults. Memory math + upper-guard rationale on the
            // field doc-comments above.
            history_trajectory_samples_per_pid: 1800,
            history_event_archive_cap: 500,
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
        // v1.3.2 / DISPATCH 89 / PHASE 5 step 0 — bounds on the new
        // history tunables. Lower bound at 1 because a zero-cap ring
        // would mean "discard every sample / event," which is what
        // disabling History entirely should look like — and we
        // don't expose a separate enable flag this early; an
        // operator who really wants to disable can set both to 1
        // and accept the tiny noise. Upper guards are 10× defaults
        // (see field doc-comments for the memory math).
        if self.runtime.history_trajectory_samples_per_pid == 0 {
            return Err(ConfigError::Invalid(
                "runtime.history_trajectory_samples_per_pid must be > 0".into(),
            ));
        }
        if self.runtime.history_trajectory_samples_per_pid > 18000 {
            return Err(ConfigError::Invalid(format!(
                "runtime.history_trajectory_samples_per_pid ({}) must be <= 18000 \
                 (10\u{00d7} the doc-locked default of 1800 ≈ 5 hours @ 1 Hz). \
                 Memory math: 32 B \u{00d7} 32 PIDs \u{00d7} this cap; ceiling \
                 \u{2248} 18.4 MB worst case.",
                self.runtime.history_trajectory_samples_per_pid,
            )));
        }
        if self.runtime.history_event_archive_cap == 0 {
            return Err(ConfigError::Invalid(
                "runtime.history_event_archive_cap must be > 0".into(),
            ));
        }
        if self.runtime.history_event_archive_cap > 5000 {
            return Err(ConfigError::Invalid(format!(
                "runtime.history_event_archive_cap ({}) must be <= 5000 \
                 (10\u{00d7} the doc-locked default of 500). Memory math: \
                 \u{2248} 300 B/entry \u{00d7} this cap; ceiling \u{2248} 1.5 MB.",
                self.runtime.history_event_archive_cap,
            )));
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
        // values. The cross-layer invariant that `default_ai_action`
        // must also be `Kill` for a kill to fire is asserted at the
        // actuation site (`record_governor_audit`), not here — that
        // way operators can flip `auto_actuate` and `default_ai_action`
        // in either order and validate() rejects neither alone.
        let _ = self.governor.auto_actuate;

        // v1.3.2 / DISPATCH 80 / C3 (Q3) — `kill_sustain_secs` must
        // be >= `alert_sustain_secs`. A kill MUST NOT undercut the
        // alert-smoothing window: the operator sees the alert first,
        // only sustained breaches escalate to kill. Comparing against
        // the resolved alert sustain (config override or contract
        // default) catches both "missing override" and "operator set
        // alert higher than kill" misconfigures.
        let effective_alert_sustain = self
            .thresholds
            .alert_sustain_secs
            .unwrap_or(ux_contract::thresholds::ALERT_SUSTAIN_SECS);
        if self.governor.kill_sustain_secs < effective_alert_sustain {
            return Err(ConfigError::Invalid(format!(
                "governor.kill_sustain_secs ({}) must be >= thresholds.alert_sustain_secs ({}). \
                 The kill path must never undercut the alert-smoothing window — \
                 raise governor.kill_sustain_secs to at least {}.",
                self.governor.kill_sustain_secs,
                effective_alert_sustain,
                effective_alert_sustain,
            )));
        }

        Ok(())
    }

    /// v1.3.2 / DISPATCH 85 — web auth-posture validation. SEPARATE
    /// from [`Self::validate`] because the auth check only matters
    /// when the web server is going to start: `--no-web` runs MUST
    /// pass without an auth_token, and the auth gate is a runtime
    /// concern (does the server bind with credentials?), not a
    /// config-internal-consistency concern.
    ///
    /// Called from `main.rs` IMMEDIATELY BEFORE `spawn_web_server`
    /// when the operator hasn't passed `--no-web`. An empty token +
    /// `allow_no_auth = false` ⇒ refuse to start. This makes "no
    /// auth" a CONSCIOUS choice; pre-D85 the web was silently open
    /// with only a `tracing::warn!` line that an operator could
    /// easily miss in production logs.
    ///
    /// The token VALUE is NEVER echoed in this error message — we
    /// name the field, not its content.
    pub fn validate_web_auth(&self) -> Result<(), ConfigError> {
        if self.web.auth_token.is_empty() && !self.web.allow_no_auth {
            // ONBOARDING dispatch — front-load the fix, drop D85
            // jargon. A new user needs "here's what to add," not a
            // history lesson. The 3 concrete choices are named
            // literally.
            return Err(ConfigError::Invalid(
                "the web dashboard needs an auth choice. In your config, set ONE of:\n\
                 \n\
                 \u{20}\u{20}[web]\n\
                 \u{20}\u{20}allow_no_auth = true    # OK on localhost / trusted LAN; no token needed\n\
                 \n\
                 \u{20}\u{20}[web]\n\
                 \u{20}\u{20}auth_token = \"<secret>\" # required for remote / untrusted networks;\n\
                 \u{20}\u{20}                         # clients send Authorization: Bearer <token>\n\
                 \n\
                 Or run without the web dashboard at all: pass --no-web on the command line.\n\
                 (Your auth_token value is never echoed in logs or error messages.)".into(),
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
            rate_limit_max_kills: self.policy.rate_limit_max_kills,
            rate_limit_window_secs: self.policy.rate_limit_window_secs,
        }
    }

    /// v1.3.2 / DISPATCH 57 — resolve the `[[workloads]]` array into
    /// a name-indexed lookup, the form runtime consumers use.
    ///
    /// Returns `Err(ConfigError::Invalid)` on:
    ///   * empty `name` (a rule with no match key is meaningless)
    ///   * duplicate `name` across rules (ambiguous which rule wins)
    ///
    /// Accepts (with a `tracing::warn!`):
    ///   * `name` longer than 15 chars — Linux `/proc/<pid>/comm` is
    ///     a 15-byte buffer; the kernel truncates longer process
    ///     names. A rule naming `"my_long_workload_binary"` will
    ///     match nothing for most processes. BUT a process can
    ///     self-set its `comm` via `prctl(PR_SET_NAME, ...)` to an
    ///     arbitrary string the kernel still truncates at the
    ///     comm-read boundary, so a >15-char rule MIGHT be
    ///     intentional. Warn-but-accept gives the operator one
    ///     chance to see the typo path.
    ///
    /// Accepts silently:
    ///   * `name` that matches no currently-running workload — the
    ///     rule lights up when / if the workload appears later.
    ///
    /// Side-effects (Q6 Q7 LOCKED):
    ///   * `tracing::info!` at resolve time listing how many rules
    ///     loaded and which names are suppressing alerts. Covers
    ///     the Point A "no audit trail" gap — an operator running
    ///     headless sees the rule load in `journalctl`.
    ///   * `tracing::info!` per rule when BOTH `suppress_alerts`
    ///     AND `suppress_recommendations` are true — Q6: this is
    ///     equivalent to a workload-level mute, and naming the
    ///     redundancy helps operators spot the simpler
    ///     `[[workloads]]` shape.
    pub fn resolve_workload_rules(
        &self,
    ) -> Result<std::collections::HashMap<String, WorkloadRule>, ConfigError> {
        let mut map: std::collections::HashMap<String, WorkloadRule> =
            std::collections::HashMap::with_capacity(self.workloads.len());
        let mut suppress_alerts_names: Vec<String> = Vec::new();
        for rule in &self.workloads {
            if rule.name.is_empty() {
                return Err(ConfigError::Invalid(
                    "[[workloads]] rule with empty `name` rejected: every \
                     rule must name a process `comm` to match"
                        .into(),
                ));
            }
            if map.contains_key(&rule.name) {
                return Err(ConfigError::Invalid(format!(
                    "[[workloads]] duplicate name {:?} — each workload may \
                     have at most one rule",
                    rule.name,
                )));
            }
            // Linux PROC_COMM_LEN is 16 (TASK_COMM_LEN = 16 incl.
            // NUL); userspace observes 15 bytes. A longer rule
            // name CAN still match a process that set its own
            // `comm` to a string the kernel later truncated to the
            // same prefix, but warning here keeps the typo path
            // visible.
            if rule.name.len() > 15 {
                tracing::warn!(
                    rule = %rule.name,
                    "[[workloads]] rule name is >15 chars; Linux `/proc/<pid>/comm` is \
                     truncated to 15 bytes, so this rule may never match unless the \
                     process self-set its `comm` via `prctl(PR_SET_NAME, ...)`.",
                );
            }
            if rule.suppress_alerts && rule.suppress_recommendations {
                tracing::info!(
                    rule = %rule.name,
                    "[[workloads]] rule has BOTH `suppress_alerts` and \
                     `suppress_recommendations` set — this is effectively a \
                     workload-level mute; same effect as either flag alone for the \
                     visible surface.",
                );
            }
            if rule.suppress_alerts {
                suppress_alerts_names.push(rule.name.clone());
            }
            map.insert(rule.name.clone(), rule.clone());
        }
        // Point A audit gap closure: log rule load at startup so an
        // operator running headless sees in `journalctl` exactly
        // which workloads have suppression active. The OOM
        // carve-out is also called out so an operator who set
        // `suppress_alerts = true` doesn't assume OOM events are
        // hidden too.
        tracing::info!(
            count = map.len(),
            suppressing_alerts = ?suppress_alerts_names,
            "[[workloads]] rules loaded; OomDetected is un-suppressable regardless",
        );
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn default_validates() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.validate().expect("default + allow_no_auth must validate");
    }

    #[test]
    fn zero_tick_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.tick_interval_ms = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_grace_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.policy.sigterm_grace_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn build_policy_roundtrips_allowlist() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.policy.allowlist.insert("my_app".into());
        let pol = cfg.build_policy();
        assert!(pol.whitelist_names.contains("my_app"));
    }

    #[test]
    fn from_file_loads_minimal_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // v1.3.2 / DISPATCH 85 — `Config::from_file` validates, and
        // the default `[web]` section without an auth_token/opt-out
        // rejects. Adding `allow_no_auth = true` here exercises the
        // toml loader on a minimal-but-valid config; the auth gate
        // has dedicated tests.
        writeln!(f, "[runtime]\ntick_interval_ms = 500\n\n[web]\nallow_no_auth = true").unwrap();
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
        cfg.web.allow_no_auth = true;
        cfg.storage.run_store_path.clear();
        assert!(cfg.storage.run_store().is_none());
    }

    #[test]
    fn storage_keep_zero_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.storage.keep_runs_per_model = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_critical_below_warn_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.regression.warn_pct = 25.0;
        cfg.regression.critical_pct = 10.0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_negative_threshold_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.regression.warn_pct = -1.0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn regression_zero_window_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
        cfg.regression.baseline_strategy = "MEDIAN".to_string();
        cfg.validate().expect("MEDIAN normalises to median");
        cfg.regression.baseline_strategy = "Mean".to_string();
        cfg.validate().expect("Mean normalises to mean");
    }

    #[test]
    fn regression_unknown_strategy_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
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

    /// v1.3.2 / DISPATCH 57 — `resolve_workload_rules` is the
    /// runtime-facing form of the `[[workloads]]` TOML array. An
    /// empty `[[workloads]]` resolves to an empty map and is the
    /// default state for any config that doesn't declare rules.
    #[test]
    fn workload_rules_empty_resolves_to_empty_map() {
        let cfg = Config::default();
        let map = cfg.resolve_workload_rules().expect("empty must resolve");
        assert!(map.is_empty());
    }

    /// Resolve a typical multi-rule config. Each rule appears in
    /// the resolved map under its `name` key.
    #[test]
    fn workload_rules_resolve_to_name_indexed_map() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.workloads.push(WorkloadRule {
            name: "vllm".into(),
            suppress_alerts: false,
            suppress_recommendations: true,
        });
        cfg.workloads.push(WorkloadRule {
            name: "ollama".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let map = cfg.resolve_workload_rules().expect("two rules must resolve");
        assert_eq!(map.len(), 2);
        let vllm = map.get("vllm").expect("vllm rule indexed by name");
        assert!(!vllm.suppress_alerts);
        assert!(vllm.suppress_recommendations);
        let ollama = map.get("ollama").expect("ollama rule indexed by name");
        assert!(ollama.suppress_alerts);
        assert!(!ollama.suppress_recommendations);
    }

    /// Empty `name` is meaningless (matches every / no workload
    /// depending on how you read it) — reject at resolve time so
    /// the operator sees a startup error rather than confusing
    /// runtime behaviour.
    #[test]
    fn workload_rules_empty_name_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.workloads.push(WorkloadRule {
            name: "".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let err = cfg
            .resolve_workload_rules()
            .expect_err("empty name must reject");
        assert!(format!("{err}").contains("empty `name`"));
    }

    /// Two rules naming the same workload is ambiguous (which
    /// flag set wins?). Reject so the operator picks one.
    #[test]
    fn workload_rules_duplicate_name_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.workloads.push(WorkloadRule {
            name: "vllm".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        cfg.workloads.push(WorkloadRule {
            name: "vllm".into(),
            suppress_alerts: false,
            suppress_recommendations: true,
        });
        let err = cfg
            .resolve_workload_rules()
            .expect_err("dup name must reject");
        assert!(format!("{err}").contains("duplicate name"));
    }

    /// Q5 LOCKED: rule names longer than 15 chars are ACCEPTED
    /// (warn-but-pass). Linux `/proc/<pid>/comm` truncates to 15
    /// bytes, so an over-length rule may not match anything in
    /// practice — but it MAY match a process that self-set its
    /// `comm` via `prctl(PR_SET_NAME)`, so rejecting would be
    /// over-eager.
    #[test]
    fn workload_rules_long_name_accepted_warn_only() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.workloads.push(WorkloadRule {
            name: "a_long_workload_binary_name".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let map = cfg
            .resolve_workload_rules()
            .expect("long name must accept");
        assert!(map.contains_key("a_long_workload_binary_name"));
    }

    /// A rule naming a not-currently-running workload is accepted
    /// silently. The rule lights up when the workload appears.
    #[test]
    fn workload_rules_unknown_workload_accepted_silently() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.workloads.push(WorkloadRule {
            name: "future_workload".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let map = cfg.resolve_workload_rules().expect("unknown name OK");
        assert!(map.contains_key("future_workload"));
    }

    /// TOML round-trip of the [[workloads]] array preserves rule
    /// order and flag values. Pins the section header + key
    /// names so a future schema rename can't drift silently.
    #[test]
    fn workload_rules_round_trip_through_toml() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.workloads.push(WorkloadRule {
            name: "phi3".into(),
            suppress_alerts: false,
            suppress_recommendations: true,
        });
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("[[workloads]]"),
            "serialized TOML must include `[[workloads]]` header; got:\n{serialized}",
        );
        assert!(
            serialized.contains("suppress_recommendations"),
            "serialized TOML must include the suppress_recommendations key; got:\n{serialized}",
        );
        let parsed: Config =
            toml::from_str(&serialized).expect("round-trip deserialize");
        assert_eq!(parsed.workloads.len(), 1);
        assert_eq!(parsed.workloads[0].name, "phi3");
        assert!(parsed.workloads[0].suppress_recommendations);
        assert!(!parsed.workloads[0].suppress_alerts);
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

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 85 — web auth posture (validate_web_auth).
    // ─────────────────────────────────────────────────────────────

    /// THE HEADLINE: Config::default() has an empty auth_token AND
    /// allow_no_auth=false, so validate_web_auth REJECTS. Silent-
    /// open regression pin: an out-of-the-box install MUST make a
    /// deliberate choice (set the token, OR flip allow_no_auth=true,
    /// OR run --no-web).
    ///
    /// ONBOARDING dispatch: the error message wording softened to
    /// name each of the three fixes verbatim (both field names + the
    /// --no-web escape hatch), so a new user sees a checklist rather
    /// than jargon. This test pins the checklist — all three named,
    /// no D85 language, no token value echoed.
    #[test]
    fn default_web_config_rejects_validate_web_auth() {
        let cfg = Config::default();
        let err = cfg
            .validate_web_auth()
            .expect_err("empty token + !allow_no_auth MUST reject");
        let msg = format!("{err}");
        // Both settings named (any occurrence is enough — the
        // message wraps them inside a `[web]` TOML snippet).
        assert!(
            msg.contains("auth_token") && msg.contains("allow_no_auth"),
            "error must name BOTH fields so the operator can act on \
             it; got: {msg}",
        );
        // The --no-web escape hatch is offered.
        assert!(
            msg.contains("--no-web"),
            "error must mention --no-web as an alternative; got: {msg}",
        );
        // Schema-firewall + dispatch C1: the token VALUE is never
        // echoed. The error message lists the FIELD NAMES, never a
        // secret. (Default token is empty so nothing to echo, but
        // pin the discipline.)
        assert!(
            !msg.contains("hunter2") && !msg.contains("secret-value"),
            "validate_web_auth error MUST NOT echo any token value; got: {msg}",
        );
        // De-jargon: the pre-D85 language must be gone. A new user
        // has no idea what D85 was; the message is a first-run
        // fixture, not a history lesson.
        assert!(
            !msg.contains("D85") && !msg.contains("pre-D85"),
            "auth error must NOT reference D85 jargon; got: {msg}",
        );
    }

    /// Operator's explicit opt-out — `allow_no_auth = true` with
    /// empty `auth_token` PASSES validate_web_auth. This is the
    /// pre-D85 posture, preserved behind a conscious config flip.
    #[test]
    fn allow_no_auth_opt_out_passes_validate_web_auth() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.validate_web_auth()
            .expect("allow_no_auth=true MUST allow empty token");
    }

    /// Set token passes validate_web_auth regardless of
    /// allow_no_auth — a configured token IS the lock; the opt-out
    /// flag becomes redundant. Pin both combinations.
    #[test]
    fn set_token_passes_validate_web_auth_either_way() {
        let mut cfg = Config::default();
        cfg.web.auth_token = "hunter2".to_string();
        cfg.web.allow_no_auth = false;
        cfg.validate_web_auth()
            .expect("token set + allow_no_auth=false MUST pass");
        cfg.web.allow_no_auth = true;
        cfg.validate_web_auth()
            .expect("token set + allow_no_auth=true MUST pass");
    }

    /// `Config::validate` does NOT enforce the web auth posture —
    /// that lives on `validate_web_auth` so `--no-web` runs work
    /// without an auth_token. Pin the split: a default Config with
    /// no auth_token but otherwise valid fields must pass the
    /// general `validate` (auth gate is a separate, web-only check).
    #[test]
    fn validate_does_not_enforce_web_auth_posture() {
        let cfg = Config::default();
        cfg.validate().expect(
            "Config::validate is config-internal-consistency only; \
             the web auth check lives on validate_web_auth so \
             --no-web bypasses it cleanly.",
        );
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 89 / PHASE 5 step 0 — history config fields.
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn history_defaults_match_design_doc() {
        let cfg = Config::default();
        assert_eq!(
            cfg.runtime.history_trajectory_samples_per_pid, 1800,
            "doc-locked default: 1800 samples/PID (≈ 30 min @ 1 Hz)"
        );
        assert_eq!(
            cfg.runtime.history_event_archive_cap, 500,
            "doc-locked default: 500 events archive-wide"
        );
    }

    #[test]
    fn history_trajectory_samples_zero_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_trajectory_samples_per_pid = 0;
        let err = cfg
            .validate()
            .expect_err("zero sample cap must reject");
        assert!(
            format!("{err}").contains("history_trajectory_samples_per_pid"),
            "rejection must name the field; got {err}"
        );
    }

    #[test]
    fn history_trajectory_samples_over_upper_guard_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_trajectory_samples_per_pid = 18001;
        let err = cfg
            .validate()
            .expect_err("> 18000 (10× default) must reject — memory ceiling");
        let msg = format!("{err}");
        assert!(
            msg.contains("18001") && msg.contains("18000"),
            "rejection must report both the value and the cap; got {msg}",
        );
    }

    #[test]
    fn history_trajectory_samples_at_upper_guard_accepted() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_trajectory_samples_per_pid = 18000;
        cfg.validate().expect("18000 exactly must pass (boundary)");
    }

    #[test]
    fn history_event_archive_zero_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_event_archive_cap = 0;
        let err = cfg.validate().expect_err("zero archive cap must reject");
        assert!(
            format!("{err}").contains("history_event_archive_cap"),
            "rejection must name the field; got {err}"
        );
    }

    #[test]
    fn history_event_archive_over_upper_guard_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_event_archive_cap = 5001;
        let err = cfg
            .validate()
            .expect_err("> 5000 (10× default) must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("5001") && msg.contains("5000"),
            "rejection must report both the value and the cap; got {msg}",
        );
    }

    #[test]
    fn history_fields_round_trip_through_toml() {
        let mut original = Config::default();
        original.web.allow_no_auth = true;
        original.runtime.history_trajectory_samples_per_pid = 600;
        original.runtime.history_event_archive_cap = 250;
        let serialized = toml::to_string(&original).expect("serialize");
        assert!(
            serialized.contains("history_trajectory_samples_per_pid"),
            "TOML must include the new field name; got: {serialized}",
        );
        assert!(
            serialized.contains("history_event_archive_cap"),
            "TOML must include the new field name; got: {serialized}",
        );
        let parsed: Config = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(parsed.runtime.history_trajectory_samples_per_pid, 600);
        assert_eq!(parsed.runtime.history_event_archive_cap, 250);
    }
}
