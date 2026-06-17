//! v1.3.2 / DISPATCH 86 — web settings handlers (GET + POST).
//!
//! ## The boundary (the most important property of this module)
//!
//! [`SettingsUpdate`] is the POST request body type. It carries
//! ONLY the web-tunable fields (numeric thresholds + the auto-kill
//! sustain window). `serde(deny_unknown_fields)` makes any
//! attempt to send `auto_actuate`, `default_ai_action`, or any
//! other non-tunable a HARD 400 — the field cannot be smuggled in
//! by a crafted request, the deserializer rejects the whole body
//! before the handler runs.
//!
//! The structural pin lives on [`super::tunables::RuntimeTunables`]:
//! that type IS the allowlist of "what the web can write." Adding a
//! new tunable is a deliberate two-place change (the type + this
//! request struct); a new `Config` field does not silently grow the
//! web-writable surface.
//!
//! ## GET /api/settings
//!
//! Returns the CURRENT runtime tunables plus a READ-ONLY view of
//! `auto_actuate` and `default_ai_action`. The UI displays the
//! read-only fields as "Auto-actuate: OFF — set in config file to
//! enable" — visibility without a control. Display honesty: the
//! operator sees the state, the web doesn't offer the lever.
//!
//! ## POST /api/settings
//!
//! Validates → updates the shared `RuntimeTunables` (visible to the
//! tick loop on the next iteration) → persists a PARTIAL TOML
//! update (preserves auto_actuate, policy, audit, and every other
//! non-web field in the existing config file).
//!
//! Re-runs the D80 sustain-validation chain (`kill_sustain_secs >=
//! alert_sustain_secs`) on the merged config — a web POST cannot
//! bypass the safety invariant the config loader enforces.

use std::path::PathBuf;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use super::WebState;
use crate::config::{ConfigError, ThresholdsConfig};
use crate::thresholds::EffectiveThresholds;

/// POST body for `/api/settings`. Structural allowlist of what the
/// web may write. `deny_unknown_fields` makes any extra key (the
/// archetypal case: a crafted `auto_actuate: true`) a 400 before
/// the handler runs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsUpdate {
    /// Numeric breach thresholds — `[thresholds]` section of the
    /// config TOML. Every field on `ThresholdsConfig` is
    /// `Option<f64>` / `Option<u64>`; missing fields keep their
    /// current resolved value (partial update).
    pub thresholds: ThresholdsConfig,
    /// Auto-kill sustain window (D80 Q3). `None` ⇒ keep current.
    pub kill_sustain_secs: Option<u64>,
}

/// GET response for `/api/settings`. Carries the current tunables
/// AND a read-only view of the boundary fields (`auto_actuate`,
/// `default_ai_action`) for honest display.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub thresholds: EffectiveThresholdsWire,
    pub kill_sustain_secs: u64,
    /// READ-ONLY. The web cannot flip this. Sourced from
    /// `config.governor.auto_actuate` at the moment of the read;
    /// changing it requires editing TOML and restarting.
    pub auto_actuate_readonly: bool,
    /// READ-ONLY. The policy verb (Allow vs Kill). Web cannot
    /// change this; arming the killer is a console act.
    pub default_ai_action_readonly: String,
    /// Path the web POST persists to. `None` when the binary was
    /// launched with built-in defaults (no `--config`, no
    /// `./edge_monitor.toml` in cwd). A POST in that case still
    /// updates the running tunables but is NOT persisted — the
    /// response carries `persisted: false` so the operator sees
    /// the gap.
    pub config_path: Option<String>,
}

/// Wire-shape twin of [`EffectiveThresholds`]. Serializable so the
/// GET response can carry the resolved values. Same field names so
/// the frontend doesn't need a translation layer.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveThresholdsWire {
    pub thermal_amber_c: f64,
    pub thermal_red_c: f64,
    pub vram_attention_pct: f64,
    pub vram_critical_pct: f64,
    pub ram_attention_pct: f64,
    pub ram_critical_pct: f64,
    pub kv_attention_pct: f64,
    pub kv_critical_pct: f64,
    pub alert_sustain_secs: u64,
}

impl From<EffectiveThresholds> for EffectiveThresholdsWire {
    fn from(t: EffectiveThresholds) -> Self {
        Self {
            thermal_amber_c: t.thermal_amber_c,
            thermal_red_c: t.thermal_red_c,
            vram_attention_pct: t.vram_attention_pct,
            vram_critical_pct: t.vram_critical_pct,
            ram_attention_pct: t.ram_attention_pct,
            ram_critical_pct: t.ram_critical_pct,
            kv_attention_pct: t.kv_attention_pct,
            kv_critical_pct: t.kv_critical_pct,
            alert_sustain_secs: t.alert_sustain_secs,
        }
    }
}

/// POST response. Wraps [`SettingsView`] with a `persisted` flag
/// so the operator sees whether the TOML write happened.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsPostResponse {
    pub settings: SettingsView,
    /// `true` ⇒ the change was written to disk. `false` ⇒ the
    /// running config was updated but no TOML path is configured;
    /// the change will be lost on restart.
    pub persisted: bool,
}

/// `GET /api/settings`.
pub async fn get_settings(State(state): State<WebState>) -> impl IntoResponse {
    Json(build_view(&state))
}

/// `POST /api/settings`. The handler:
///
///   1. Validates `update` against the current settings snapshot
///      (kill_sustain_secs >= alert_sustain_secs).
///   2. Atomically replaces the shared tunables.
///   3. Persists a PARTIAL TOML update (preserves auto_actuate +
///      policy + audit + every other field in the existing TOML).
///
/// Errors:
///   * 400 with the validator's message when the sustain invariant
///     fails OR when the request body carried unknown fields
///     (handled by `deny_unknown_fields` in the deserializer).
///   * 500 if the TOML rewrite errors (disk full, permission).
///     The running config is STILL updated in that case so the
///     operator at least sees the change take effect; the
///     `persisted: false` flag tells them disk save failed.
pub async fn update_settings(
    State(state): State<WebState>,
    Json(update): Json<SettingsUpdate>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(tunables) = state.tunables.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "settings endpoint requires SharedTunables (web companion launched without it)"
                .to_string(),
        ));
    };

    // (1) Validate by reconstructing the EFFECTIVE config the
    // proposed update would produce. Reuses the load-time resolver
    // so the web write path applies the SAME validation as a fresh
    // TOML load — no skipped checks, no divergent path.
    let proposed = resolve_proposed(&update).map_err(|e| {
        (StatusCode::BAD_REQUEST, format!("settings invalid: {e}"))
    })?;
    let new_kill_sustain = update.kill_sustain_secs.unwrap_or_else(|| {
        // Caller didn't supply one ⇒ keep the current value.
        let guard = tunables.read().unwrap_or_else(|p| p.into_inner());
        guard.kill_sustain_secs
    });
    if new_kill_sustain < proposed.alert_sustain_secs {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "kill_sustain_secs ({new_kill_sustain}) must be >= \
                 alert_sustain_secs ({}). The kill path must never \
                 undercut the alert-smoothing window — raise \
                 kill_sustain_secs or lower thresholds.alert_sustain_secs.",
                proposed.alert_sustain_secs,
            ),
        ));
    }

    // (2) Atomic swap of the shared tunables.
    {
        let mut guard = tunables.write().unwrap_or_else(|p| p.into_inner());
        guard.thresholds = proposed;
        guard.kill_sustain_secs = new_kill_sustain;
    }

    // (3) Partial TOML update — preserves every field NOT in the
    // web allowlist (notably auto_actuate, policy, audit history).
    let persisted = match state.config_path.as_ref() {
        Some(path) => {
            persist_partial_toml(path, &update).is_ok()
        }
        None => false,
    };

    Ok(Json(SettingsPostResponse {
        settings: build_view(&state),
        persisted,
    }))
}

/// Build the GET response from current state. Reads the shared
/// tunables (the LIVE values, post-any-recent-POST) for the
/// editable section and the static config snapshot for the
/// read-only boundary fields.
fn build_view(state: &WebState) -> SettingsView {
    let (thresholds, kill_sustain_secs) = match state.tunables.as_ref() {
        Some(tunables) => {
            let guard = tunables.read().unwrap_or_else(|p| p.into_inner());
            (guard.thresholds, guard.kill_sustain_secs)
        }
        None => (EffectiveThresholds::default(), 10),
    };
    SettingsView {
        thresholds: thresholds.into(),
        kill_sustain_secs,
        auto_actuate_readonly: state.auto_actuate_at_load,
        default_ai_action_readonly: state.default_ai_action_at_load.clone(),
        config_path: state.config_path.as_ref().map(|p| p.display().to_string()),
    }
}

/// Re-validate the proposed thresholds via the same resolver the
/// config loader uses. Catches the same errors a fresh TOML load
/// would (range bounds, critical >= attention, etc.). Returns the
/// resolved struct on success; the caller then enforces the
/// kill_sustain vs alert_sustain invariant.
fn resolve_proposed(update: &SettingsUpdate) -> Result<EffectiveThresholds, ConfigError> {
    EffectiveThresholds::resolve(&update.thresholds)
}

/// Persist the partial update to disk. Reads the existing TOML
/// as a `toml::Table`, mutates the in-scope keys, writes back.
/// The `[governor].auto_actuate` line is NEVER touched — the
/// table-level merge only writes the keys we know about.
fn persist_partial_toml(
    path: &PathBuf,
    update: &SettingsUpdate,
) -> Result<(), Box<dyn std::error::Error>> {
    use toml::{Table, Value};

    let raw = std::fs::read_to_string(path)?;
    let mut table: Table = raw.parse()?;

    // [thresholds] — overwrite ONLY the keys the update carried.
    // Missing keys in the update stay as whatever the TOML had.
    let thresholds_table = table
        .entry("thresholds")
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .ok_or("`thresholds` is not a table in the existing config")?;
    if let Some(v) = update.thresholds.thermal_amber_c {
        thresholds_table.insert("thermal_amber_c".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.thermal_red_c {
        thresholds_table.insert("thermal_red_c".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.vram_attention_pct {
        thresholds_table.insert("vram_attention_pct".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.vram_critical_pct {
        thresholds_table.insert("vram_critical_pct".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.ram_attention_pct {
        thresholds_table.insert("ram_attention_pct".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.ram_critical_pct {
        thresholds_table.insert("ram_critical_pct".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.kv_attention_pct {
        thresholds_table.insert("kv_attention_pct".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.kv_critical_pct {
        thresholds_table.insert("kv_critical_pct".into(), Value::Float(v));
    }
    if let Some(v) = update.thresholds.alert_sustain_secs {
        thresholds_table.insert(
            "alert_sustain_secs".into(),
            Value::Integer(v as i64),
        );
    }

    // [governor].kill_sustain_secs — same partial-update shape.
    // Critically, we do NOT TOUCH `[governor].auto_actuate` here.
    // If a future contributor adds an auto_actuate write here,
    // `auto_actuate_persist_preserves_existing_value` fires.
    if let Some(v) = update.kill_sustain_secs {
        let governor_table = table
            .entry("governor")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .ok_or("`governor` is not a table in the existing config")?;
        governor_table.insert(
            "kill_sustain_secs".into(),
            Value::Integer(v as i64),
        );
    }

    let serialized = toml::to_string(&table)?;
    std::fs::write(path, serialized)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::tunables::shared_from_config;

    #[test]
    fn settings_update_rejects_auto_actuate_via_deny_unknown_fields() {
        // The headline boundary test at the serde layer. A
        // crafted body with `auto_actuate` triggers
        // deny_unknown_fields and fails to deserialize ⇒ the
        // handler never runs ⇒ the boundary cannot be crossed.
        let body = r#"{
            "thresholds": {},
            "auto_actuate": true
        }"#;
        let result: Result<SettingsUpdate, _> = serde_json::from_str(body);
        assert!(
            result.is_err(),
            "POST body with `auto_actuate` MUST be rejected by serde \
             (deny_unknown_fields). Got: {result:?}",
        );
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("auto_actuate") && err.contains("unknown"),
            "deserialization error must name the rejected field; got: {err}",
        );
    }

    #[test]
    fn settings_update_rejects_default_ai_action_via_deny_unknown_fields() {
        let body = r#"{
            "thresholds": {},
            "default_ai_action": "Kill"
        }"#;
        let result: Result<SettingsUpdate, _> = serde_json::from_str(body);
        assert!(
            result.is_err(),
            "POST body with `default_ai_action` MUST be rejected — \
             policy verbs are not web-writable (boundary). Got: {result:?}",
        );
    }

    #[test]
    fn settings_update_accepts_threshold_only_body() {
        let body = r#"{
            "thresholds": { "vram_critical_pct": 80.0 }
        }"#;
        let parsed: SettingsUpdate = serde_json::from_str(body)
            .expect("threshold-only body MUST parse");
        assert_eq!(parsed.thresholds.vram_critical_pct, Some(80.0));
        assert!(parsed.kill_sustain_secs.is_none());
    }

    #[test]
    fn persist_partial_preserves_auto_actuate_and_policy() {
        // THE PARTIAL-PERSIST BOUNDARY PIN. Start with a TOML that
        // explicitly arms auto_actuate and sets a policy verb;
        // POST a thresholds update; confirm both the auto_actuate
        // line AND the policy verb are UNCHANGED on disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("em.toml");
        let initial = r#"[governor]
auto_actuate = true
kill_sustain_secs = 15

[policy]
default_ai_action = "Kill"
sigterm_grace_secs = 7

[thresholds]
vram_critical_pct = 95.0
"#;
        std::fs::write(&path, initial).unwrap();

        let update = SettingsUpdate {
            thresholds: ThresholdsConfig {
                vram_critical_pct: Some(80.0),
                ..Default::default()
            },
            kill_sustain_secs: None,
        };
        persist_partial_toml(&path, &update).expect("persist must succeed");

        let after = std::fs::read_to_string(&path).unwrap();
        // The auto_actuate line MUST still read true.
        let after_table: toml::Table = after.parse().unwrap();
        assert_eq!(
            after_table["governor"]["auto_actuate"].as_bool(),
            Some(true),
            "partial persist MUST preserve auto_actuate=true. \
             Post-update TOML:\n{after}",
        );
        assert_eq!(
            after_table["policy"]["default_ai_action"].as_str(),
            Some("Kill"),
            "partial persist MUST preserve default_ai_action='Kill'. \
             Post-update TOML:\n{after}",
        );
        // The threshold we changed reflects.
        assert!(
            (after_table["thresholds"]["vram_critical_pct"].as_float().unwrap() - 80.0).abs() < 0.01,
            "vram_critical_pct must reflect the POST. Post-update TOML:\n{after}",
        );
        // Other thresholds preserved (none touched in this update).
        // Other policy fields preserved.
        assert_eq!(
            after_table["policy"]["sigterm_grace_secs"].as_integer(),
            Some(7),
        );
        assert_eq!(
            after_table["governor"]["kill_sustain_secs"].as_integer(),
            Some(15),
        );
    }

    #[test]
    fn shared_from_config_produces_resolvable_tunables() {
        let cfg = crate::config::Config::default();
        let shared = shared_from_config(&cfg);
        let guard = shared.read().unwrap();
        assert_eq!(guard.kill_sustain_secs, cfg.governor.kill_sustain_secs);
        // Resolved against the contract defaults.
        assert!(guard.thresholds.thermal_amber_c > 0.0);
    }
}
