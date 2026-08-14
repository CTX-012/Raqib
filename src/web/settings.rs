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

    // (2) Atomic swap of the shared tunables. This happens BEFORE
    // the disk write so a reader (next tick loop) never sees a disk
    // value ahead of the in-memory one. The tick loop reads only
    // SharedTunables, never the disk, so the persist step below is
    // durability-only.
    {
        let mut guard = tunables.write().unwrap_or_else(|p| p.into_inner());
        guard.thresholds = proposed;
        guard.kill_sustain_secs = new_kill_sustain;
    }

    // Audit line — every web-triggered settings mutation, with the
    // ARMED-state flag so `warn` or grep-on-armed can surface live
    // threshold edits against an armed governor. The armed flag is
    // load-time (see WebState.auto_actuate_at_load); if the operator
    // arms via TOML+restart, this line will fire with armed=true and
    // the operator has an audit trail of every subsequent live
    // threshold edit.
    tracing::info!(
        armed = state.auto_actuate_at_load,
        vram_critical_pct = ?update.thresholds.vram_critical_pct,
        vram_attention_pct = ?update.thresholds.vram_attention_pct,
        ram_critical_pct = ?update.thresholds.ram_critical_pct,
        ram_attention_pct = ?update.thresholds.ram_attention_pct,
        thermal_red_c = ?update.thresholds.thermal_red_c,
        thermal_amber_c = ?update.thresholds.thermal_amber_c,
        kv_critical_pct = ?update.thresholds.kv_critical_pct,
        kv_attention_pct = ?update.thresholds.kv_attention_pct,
        alert_sustain_secs = ?update.thresholds.alert_sustain_secs,
        kill_sustain_secs = new_kill_sustain,
        "web: settings updated"
    );

    // (3) Partial TOML update — preserves every field NOT in the
    // web allowlist (notably auto_actuate, policy, audit history)
    // AND every comment/blank line/key order in the operator's
    // hand-authored file (via toml_edit).
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

/// Persist the partial update to disk. Reads the existing TOML as
/// a `toml_edit::DocumentMut`, mutates ONLY the 10 web-writable keys
/// ([thresholds]'s 9 numeric fields + [governor].kill_sustain_secs),
/// writes back atomically. The write allowlist is exactly the field
/// set of [`SettingsUpdate`] — auto_actuate, default_ai_action,
/// allowlist, blocklist, rate_limit_* are NEVER touched. The
/// existing pin [`tests::persist_partial_preserves_auto_actuate_and_policy`]
/// enforces this.
///
/// Concurrency:
///   * In-process — a static `Mutex` serializes the read-modify-
///     write so two concurrent web POSTs cannot interleave and
///     produce a torn file.
///   * On-disk — the new file is written to a sibling tempfile, then
///     `rename(2)`d over the target. POSIX rename is atomic within a
///     filesystem, so any reader (the operator's editor, another
///     process) sees either the whole old file or the whole new one,
///     never a partial write mid-flight.
///
/// Comment / order preservation:
///   * `toml_edit` is a comment-preserving TOML round-tripper. Blank
///     lines, comments (leading, trailing, inline), key order, and
///     table order in the operator's hand-authored file all survive.
///     Contrast with the prior `toml::Table` implementation, which
///     scrambled every one of those on every Save.
fn persist_partial_toml(
    path: &PathBuf,
    update: &SettingsUpdate,
) -> Result<(), Box<dyn std::error::Error>> {
    use toml_edit::{DocumentMut, Item, Table, value};

    // In-process serialization guard. Two web clients POSTing at
    // the same second would otherwise both read the file, mutate
    // their own copy, and race on the write — the later writer
    // would clobber the earlier's mutation without seeing it. The
    // Mutex enforces read-modify-write-happens-under-one-lock.
    static WRITE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE_MUTEX.lock().unwrap_or_else(|p| p.into_inner());

    let raw = std::fs::read_to_string(path)?;
    let mut doc: DocumentMut = raw.parse()?;

    // [thresholds] — overwrite ONLY the keys the update carried.
    // Missing keys stay as whatever the TOML had.
    {
        let thresholds_item = doc
            .entry("thresholds")
            .or_insert(Item::Table(Table::new()));
        let thresholds_table = thresholds_item
            .as_table_mut()
            .ok_or("`thresholds` is not a table in the existing config")?;
        // Ensure a newly-created (empty) section renders as
        // `[thresholds]` rather than being elided.
        thresholds_table.set_implicit(false);
        if let Some(v) = update.thresholds.thermal_amber_c {
            thresholds_table["thermal_amber_c"] = value(v);
        }
        if let Some(v) = update.thresholds.thermal_red_c {
            thresholds_table["thermal_red_c"] = value(v);
        }
        if let Some(v) = update.thresholds.vram_attention_pct {
            thresholds_table["vram_attention_pct"] = value(v);
        }
        if let Some(v) = update.thresholds.vram_critical_pct {
            thresholds_table["vram_critical_pct"] = value(v);
        }
        if let Some(v) = update.thresholds.ram_attention_pct {
            thresholds_table["ram_attention_pct"] = value(v);
        }
        if let Some(v) = update.thresholds.ram_critical_pct {
            thresholds_table["ram_critical_pct"] = value(v);
        }
        if let Some(v) = update.thresholds.kv_attention_pct {
            thresholds_table["kv_attention_pct"] = value(v);
        }
        if let Some(v) = update.thresholds.kv_critical_pct {
            thresholds_table["kv_critical_pct"] = value(v);
        }
        if let Some(v) = update.thresholds.alert_sustain_secs {
            thresholds_table["alert_sustain_secs"] = value(v as i64);
        }
    }

    // [governor].kill_sustain_secs — same partial-update shape.
    // Critically, we do NOT TOUCH `[governor].auto_actuate` here.
    // Any future edit that touches auto_actuate on this path would
    // trip `tests::persist_partial_preserves_auto_actuate_and_policy`
    // (and the serde-layer boundary at `SettingsUpdate` would have
    // rejected the request body long before we got here).
    if let Some(v) = update.kill_sustain_secs {
        let governor_item = doc
            .entry("governor")
            .or_insert(Item::Table(Table::new()));
        let governor_table = governor_item
            .as_table_mut()
            .ok_or("`governor` is not a table in the existing config")?;
        governor_table.set_implicit(false);
        governor_table["kill_sustain_secs"] = value(v as i64);
    }

    let serialized = doc.to_string();

    // Atomic write: tempfile in the same directory, then rename(2).
    // Same-filesystem rename is POSIX-atomic. On failure, best-
    // effort cleanup of the tempfile to avoid stragglers. We stamp
    // the tempfile name with the pid + a per-call counter so
    // parallel calls (which the Mutex already serializes, but
    // defense-in-depth) don't collide on the tempfile itself.
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("config path has no file-name component")?;
    static TMP_COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = parent.join(format!(
        ".{file_name}.raqib-tmp.{}.{n}",
        std::process::id()
    ));
    std::fs::write(&tmp, serialized)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Box::new(e));
    }
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

    /// A1 PROOF — the persist path preserves comments, blank
    /// lines, and key order in the operator's hand-authored TOML.
    /// The prior `toml::Table` round-trip scrambled all three on
    /// every Save; the `toml_edit::DocumentMut` rewrite keeps
    /// them. If this test ever fails, the persist path regressed
    /// to a non-round-tripping serializer and every operator's
    /// hand-formatted file is being reformatted on every web Save.
    #[test]
    fn persist_partial_toml_preserves_comments_blank_lines_and_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("em.toml");
        let initial = "\
# Top-of-file operator comment — MUST survive round-trips.
# This file is hand-edited; don't scramble it.

[governor]
auto_actuate = true   # KILLER IS ARMED — leave this alone
kill_sustain_secs = 15

[policy]
default_ai_action = \"Kill\"

# Thresholds tuned for the RTX 3060 dev host.
[thresholds]
vram_critical_pct = 95.0
ram_critical_pct = 90.0
";
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

        // Comments survive (leading block + inline + section):
        for expected_comment in [
            "# Top-of-file operator comment",
            "# This file is hand-edited",
            "# KILLER IS ARMED",
            "# Thresholds tuned for the RTX 3060 dev host.",
        ] {
            assert!(
                after.contains(expected_comment),
                "comment `{expected_comment}` MUST survive the round-trip. \
                 If this fails, the persist path regressed to a non-comment-\
                 preserving serializer. Post-write file:\n{after}"
            );
        }

        // Blank-line separator between the top comment block and
        // `[governor]` survives:
        let head_split: Vec<_> = after.split("[governor]").collect();
        assert!(
            head_split.len() >= 2,
            "TOML no longer contains a `[governor]` header:\n{after}"
        );
        assert!(
            head_split[0].contains("\n\n"),
            "blank line between top comment block and `[governor]` MUST \
             survive. Post-write file:\n{after}"
        );

        // Section order preserved (`[governor]` before `[policy]`
        // before `[thresholds]`, as in the source):
        let gov_pos = after.find("[governor]").unwrap();
        let pol_pos = after.find("[policy]").unwrap();
        let thr_pos = after.find("[thresholds]").unwrap();
        assert!(
            gov_pos < pol_pos && pol_pos < thr_pos,
            "section order MUST be preserved (governor, policy, thresholds). \
             Got positions gov={gov_pos} pol={pol_pos} thr={thr_pos}. File:\n{after}"
        );

        // Key order INSIDE [governor] preserved (auto_actuate before
        // kill_sustain_secs, as in the source):
        let gov_section = &after[gov_pos..pol_pos];
        let aa_pos = gov_section
            .find("auto_actuate")
            .expect("auto_actuate key must be present");
        let ks_pos = gov_section
            .find("kill_sustain_secs")
            .expect("kill_sustain_secs key must be present");
        assert!(
            aa_pos < ks_pos,
            "in-section key order MUST be preserved. Got positions \
             auto_actuate={aa_pos} kill_sustain_secs={ks_pos} within [governor] \
             section:\n{gov_section}"
        );

        // The touched value reflects the update:
        let parsed: toml::Table = after.parse().unwrap();
        assert!(
            (parsed["thresholds"]["vram_critical_pct"]
                .as_float()
                .unwrap()
                - 80.0)
                .abs()
                < 0.01,
            "vram_critical_pct must reflect the POST"
        );
        // Untouched values unchanged:
        assert_eq!(
            parsed["thresholds"]["ram_critical_pct"].as_float(),
            Some(90.0),
            "ram_critical_pct MUST NOT change (POST didn't touch it)"
        );
        assert_eq!(
            parsed["governor"]["auto_actuate"].as_bool(),
            Some(true),
            "auto_actuate MUST stay armed through the round-trip"
        );
        assert_eq!(
            parsed["policy"]["default_ai_action"].as_str(),
            Some("Kill"),
            "policy verb MUST stay unchanged"
        );
    }

    /// A2 PROOF — the in-process WRITE_MUTEX inside
    /// `persist_partial_toml` serializes concurrent web POSTs so
    /// two callers cannot produce a torn or invalid TOML. Also
    /// verifies the atomic tempfile+rename doesn't leave stragglers.
    ///
    /// This is not just a paranoia test — before the mutex, two
    /// simultaneous POSTs would each `read_to_string` the pre-write
    /// state, apply their own patch, and race on `fs::write`. The
    /// later writer would silently clobber the earlier's patch with
    /// only its own change on top of the same base — losing the
    /// earlier operator's write entirely.
    #[test]
    fn persist_partial_toml_concurrent_writes_do_not_corrupt() {
        use std::sync::Arc;
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let path = dir.path().join("em.toml");
        let initial = "\
[governor]
auto_actuate = true
kill_sustain_secs = 15

[thresholds]
vram_critical_pct = 95.0
";
        std::fs::write(&path, initial).unwrap();
        let path_arc: Arc<PathBuf> = Arc::new(path.clone());

        // 8 concurrent writers, each writing a distinct
        // vram_critical_pct in [50.0, 57.0].
        let handles: Vec<_> = (0..8u32)
            .map(|i| {
                let p = path_arc.clone();
                std::thread::spawn(move || {
                    let update = SettingsUpdate {
                        thresholds: ThresholdsConfig {
                            vram_critical_pct: Some(50.0 + f64::from(i)),
                            ..Default::default()
                        },
                        kill_sustain_secs: None,
                    };
                    persist_partial_toml(&*p, &update)
                        .expect("each concurrent write must succeed");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker panicked");
        }

        // File must still be valid TOML (no torn write):
        let after = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Table = after.parse().unwrap_or_else(|e| {
            panic!(
                "post-concurrent-write file MUST parse as TOML. err={e}\nfile:\n{after}"
            );
        });

        // Arming preserved through the churn:
        assert_eq!(
            parsed["governor"]["auto_actuate"].as_bool(),
            Some(true),
            "auto_actuate MUST NOT drift through concurrent writes. \
             File:\n{after}"
        );
        assert_eq!(
            parsed["governor"]["kill_sustain_secs"].as_integer(),
            Some(15),
            "kill_sustain_secs MUST NOT drift through concurrent writes \
             (no writer touched it). File:\n{after}"
        );

        // A winner's value survives — must be one of the 8 we wrote:
        let v = parsed["thresholds"]["vram_critical_pct"]
            .as_float()
            .expect("vram_critical_pct MUST be a number");
        assert!(
            (50.0..=57.0).contains(&v),
            "final vram_critical_pct MUST be one of the concurrent \
             writes (50.0..=57.0); got {v}. File:\n{after}"
        );

        // No tempfile stragglers left behind — every raqib-tmp.*
        // should have been renamed into place.
        let stragglers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("raqib-tmp"))
            .collect();
        assert!(
            stragglers.is_empty(),
            "tempfile stragglers left in config dir after concurrent writes: \
             {stragglers:?}"
        );
    }

    /// REGRESSION — `SettingsView.config_path` must be `Some(path
    /// as string)` whenever `WebState.config_path` is populated. This
    /// pins the wire-side projection that FIX 1 (main.rs sourcing
    /// config_path from `ConfigSource` instead of `cli.config`)
    /// relies on: main.rs now supplies the real path for both
    /// `--config`-launched AND discovery-launched instances, and
    /// this test ensures the wire mapper doesn't quietly drop it.
    #[test]
    fn build_view_emits_config_path_when_state_has_one() {
        use crate::web::WebState;
        use std::sync::Arc;
        use tokio::sync::watch;

        let (_tx, rx) = watch::channel(crate::web::WireSnapshot::empty());
        let path = std::path::PathBuf::from("/home/tester/.config/raqib/raqib.toml");
        let cfg = crate::config::Config::default();
        let tunables = crate::web::tunables::shared_from_config(&cfg);
        let state = WebState {
            rx,
            auth_token: None::<Arc<str>>,
            tunables: Some(tunables),
            config_path: Some(path.clone()),
            auto_actuate_at_load: false,
            default_ai_action_at_load: "Allow".into(),
            history_view: None,
        };
        let view = build_view(&state);
        assert_eq!(
            view.config_path.as_deref(),
            Some(path.display().to_string().as_str()),
            "SettingsView.config_path must reflect WebState.config_path — \
             a Some(path) in state cannot be silently dropped to null",
        );
    }

    /// Complementary pin: when `WebState.config_path` is None
    /// (running on built-in defaults, no config file anywhere),
    /// the wire view honestly reports null. The two-case pin
    /// prevents a lazy fix that hard-codes Some(...) unconditionally.
    #[test]
    fn build_view_reports_none_when_no_config_path() {
        use crate::web::WebState;
        use std::sync::Arc;
        use tokio::sync::watch;

        let (_tx, rx) = watch::channel(crate::web::WireSnapshot::empty());
        let cfg = crate::config::Config::default();
        let tunables = crate::web::tunables::shared_from_config(&cfg);
        let state = WebState {
            rx,
            auth_token: None::<Arc<str>>,
            tunables: Some(tunables),
            config_path: None,
            auto_actuate_at_load: false,
            default_ai_action_at_load: "Allow".into(),
            history_view: None,
        };
        let view = build_view(&state);
        assert!(
            view.config_path.is_none(),
            "SettingsView.config_path must be None when no config was loaded (defaults path)",
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
