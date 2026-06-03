//! v1.3.1 / DISPATCH 53 — hybrid threshold-config resolver.
//!
//! Phase 4 step 2 (the meatiest). The contract's
//! [`ux_contract::thresholds`] constants are the deployment DEFAULTS;
//! the consumer's `[thresholds]` config section overrides them per
//! deployment. This module is the resolver: takes the optional
//! per-field overrides from [`crate::config::ThresholdsConfig`],
//! falls back to the contract default per field, then validates the
//! resolved set against the same invariants the contract pins at
//! compile time.
//!
//! ## The hybrid model (DISPATCH 47 §7 / DISPATCH 52 §2)
//!
//! Three threshold classes, three treatments:
//!
//! 1. **Wire-format caps** — absolute (`REC_MAX_VISIBLE`,
//!    `ALERT_MAX_VISIBLE`, `ACTIVITY_FEED_*_MAX`). Protocol
//!    invariants; not in this struct.
//! 2. **Deployment thresholds** — overridable defaults (this
//!    struct's 9 fields). The contract value is the default; this
//!    consumer can override per-deployment.
//! 3. **Implementation / correctness thresholds** — absolute
//!    (`ROS2_ECHO_PROBE_INTERVAL`, `ROS2_ACTIVITY_STALENESS`,
//!    `ROS2_SHELLOUT_TIMEOUT`, `EMBEDDINGS_ACTIVE_CPU_PCT`). Pinned
//!    by existing tests; correctness-critical, not for tuning.
//!
//! [`EffectiveThresholds`] carries class-2 only. The other two
//! classes stay where they are (contract for class-1, sampler-side
//! consts for class-3).
//!
//! ## Resolution semantics
//!
//! Per-field: if the config supplies `Some(v)`, the field becomes
//! `v`; otherwise the contract default. The struct is then
//! [`EffectiveThresholds::validate`]'d before being returned —
//! invalid combinations (amber ≥ red, critical < attention,
//! out-of-range pct) produce [`crate::config::ConfigError::Invalid`]
//! with an operator-actionable message. **No silent clamp.** v1.0.1's
//! phantom-kill lesson stands: a system that silently overrides
//! operator intent is worse than one that fails to start with a
//! fixable error.
//!
//! ## Authority lock
//!
//! Observation-side only. Every field here is a numeric value that
//! feeds the AlertState observe / classify_workload_status /
//! classify_thermal pipelines. No `action_on_breach` field. No
//! callable variants. Config tunes WHAT VALUE is compared against —
//! never WHAT HAPPENS on comparison. The eighth observe-only
//! confirmation.

use crate::config::{ConfigError, ThresholdsConfig};

/// All class-2 deployment thresholds, resolved against the contract
/// defaults and validated. Constructed once at [`crate::runtime::Runtime::new`]
/// and stored on [`crate::runtime::RuntimeState::thresholds`] so every
/// read site reads from a single source of truth.
///
/// `Copy` so read sites can pass it by value (it's eight `f64`s plus
/// one `u64`; cheaper than chasing references through async tasks).
#[derive(Debug, Clone, Copy)]
pub struct EffectiveThresholds {
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

impl EffectiveThresholds {
    /// Resolve against the contract defaults. Returns the validated
    /// struct or [`ConfigError::Invalid`] with an operator-actionable
    /// message naming the offending field and its actual vs expected.
    ///
    /// Validation is the same shape as the contract's compile-time
    /// `const _: () = assert!(...)` block in `ux_contract::lib.rs`:
    /// thermal_red strictly greater than thermal_amber, every
    /// `*_critical_pct` at least its `*_attention_pct`, all `*_pct`
    /// in `0..=100` (typo-protection), and `alert_sustain_secs` in
    /// `1..=600` (a sustain beyond ten minutes is almost certainly
    /// a misconfigure).
    pub fn resolve(cfg: &ThresholdsConfig) -> Result<Self, ConfigError> {
        use ux_contract::thresholds as defaults;
        let r = Self {
            thermal_amber_c: cfg.thermal_amber_c.unwrap_or(defaults::THERMAL_AMBER_C),
            thermal_red_c: cfg.thermal_red_c.unwrap_or(defaults::THERMAL_RED_C),
            vram_attention_pct: cfg
                .vram_attention_pct
                .unwrap_or(defaults::VRAM_ATTENTION_PCT),
            vram_critical_pct: cfg
                .vram_critical_pct
                .unwrap_or(defaults::VRAM_CRITICAL_PCT),
            ram_attention_pct: cfg
                .ram_attention_pct
                .unwrap_or(defaults::RAM_ATTENTION_PCT),
            ram_critical_pct: cfg
                .ram_critical_pct
                .unwrap_or(defaults::RAM_CRITICAL_PCT),
            kv_attention_pct: cfg.kv_attention_pct.unwrap_or(defaults::KV_ATTENTION_PCT),
            kv_critical_pct: cfg.kv_critical_pct.unwrap_or(defaults::KV_CRITICAL_PCT),
            alert_sustain_secs: cfg
                .alert_sustain_secs
                .unwrap_or(defaults::ALERT_SUSTAIN_SECS),
        };
        r.validate()?;
        Ok(r)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        // Thermal: amber positive; red strictly greater than amber.
        // Mirrors the contract's `THERMAL_AMBER_C > 0.0` +
        // `THERMAL_RED_C > THERMAL_AMBER_C` const_asserts at runtime.
        if self.thermal_amber_c <= 0.0 {
            return Err(ConfigError::Invalid(format!(
                "thermal_amber_c ({:.1}) must be > 0",
                self.thermal_amber_c
            )));
        }
        if self.thermal_red_c <= self.thermal_amber_c {
            return Err(ConfigError::Invalid(format!(
                "thermal_red_c ({:.1}) must be > thermal_amber_c ({:.1})",
                self.thermal_red_c, self.thermal_amber_c
            )));
        }
        // Pressure tier ordering: critical >= attention for VRAM /
        // RAM / KV. Mirrors the contract's three `*_CRITICAL_PCT >=
        // *_ATTENTION_PCT` const_asserts.
        check_pair(
            "vram_critical_pct",
            self.vram_critical_pct,
            "vram_attention_pct",
            self.vram_attention_pct,
        )?;
        check_pair(
            "ram_critical_pct",
            self.ram_critical_pct,
            "ram_attention_pct",
            self.ram_attention_pct,
        )?;
        check_pair(
            "kv_critical_pct",
            self.kv_critical_pct,
            "kv_attention_pct",
            self.kv_attention_pct,
        )?;
        // Typo-protection: every `*_pct` in `0..=100`. Catches an
        // operator who meant `85.0` and wrote `850` (the contract
        // can't typecheck f64 values).
        for (name, v) in [
            ("vram_attention_pct", self.vram_attention_pct),
            ("vram_critical_pct", self.vram_critical_pct),
            ("ram_attention_pct", self.ram_attention_pct),
            ("ram_critical_pct", self.ram_critical_pct),
            ("kv_attention_pct", self.kv_attention_pct),
            ("kv_critical_pct", self.kv_critical_pct),
        ] {
            if !(0.0..=100.0).contains(&v) {
                return Err(ConfigError::Invalid(format!(
                    "{name} ({v:.1}) must be in 0.0..=100.0"
                )));
            }
        }
        // Sustain window: a 0-second sustain would fire on every
        // observe call (no smoothing) — the v1.0.x behavior the
        // contract's 5-second default replaced. A sustain > 10 min
        // is almost certainly a misconfigure (and would silence the
        // alert system for half-an-hour-class events).
        if self.alert_sustain_secs == 0 {
            return Err(ConfigError::Invalid(
                "alert_sustain_secs (0) must be ≥ 1 (a 0-second sustain disables smoothing)"
                    .to_string(),
            ));
        }
        if self.alert_sustain_secs > 600 {
            return Err(ConfigError::Invalid(format!(
                "alert_sustain_secs ({}) > 600 — sustain windows beyond 10 minutes likely wrong",
                self.alert_sustain_secs
            )));
        }
        Ok(())
    }
}

fn check_pair(
    critical_name: &str,
    critical: f64,
    attention_name: &str,
    attention: f64,
) -> Result<(), ConfigError> {
    if critical < attention {
        return Err(ConfigError::Invalid(format!(
            "{critical_name} ({critical:.1}) must be ≥ {attention_name} ({attention:.1})"
        )));
    }
    Ok(())
}

impl Default for EffectiveThresholds {
    /// Contract defaults. The compile-time `const_assert`s in
    /// `ux_contract::lib.rs` guarantee these validate; if they ever
    /// don't, the contract itself is broken and the binary should
    /// fail loudly at startup before any tick fires.
    fn default() -> Self {
        // ok: expect — contract const_asserts guarantee the defaults
        // satisfy validate(); a failure here means the contract crate
        // was shipped with violating constants, which is a build-time
        // bug we catch before the binary boots.
        Self::resolve(&ThresholdsConfig::default())
            .expect("contract defaults must satisfy EffectiveThresholds::validate")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Defaults round-trip: a `ThresholdsConfig::default()` (every
    /// field None) resolves to the contract constants.
    #[test]
    fn defaults_resolve_to_contract_constants() {
        use ux_contract::thresholds as d;
        let r = EffectiveThresholds::resolve(&ThresholdsConfig::default()).unwrap();
        assert!((r.thermal_amber_c - d::THERMAL_AMBER_C).abs() < f64::EPSILON);
        assert!((r.thermal_red_c - d::THERMAL_RED_C).abs() < f64::EPSILON);
        assert!((r.vram_attention_pct - d::VRAM_ATTENTION_PCT).abs() < f64::EPSILON);
        assert!((r.vram_critical_pct - d::VRAM_CRITICAL_PCT).abs() < f64::EPSILON);
        assert!((r.ram_attention_pct - d::RAM_ATTENTION_PCT).abs() < f64::EPSILON);
        assert!((r.ram_critical_pct - d::RAM_CRITICAL_PCT).abs() < f64::EPSILON);
        assert!((r.kv_attention_pct - d::KV_ATTENTION_PCT).abs() < f64::EPSILON);
        assert!((r.kv_critical_pct - d::KV_CRITICAL_PCT).abs() < f64::EPSILON);
        assert_eq!(r.alert_sustain_secs, d::ALERT_SUSTAIN_SECS);
    }

    /// `EffectiveThresholds::default()` (via the `Default` impl)
    /// matches the resolve-from-default-config path. Guards a future
    /// refactor that decoupled the two from breaking the implicit
    /// "Default = contract defaults" semantic.
    #[test]
    fn default_impl_matches_resolve_of_empty_config() {
        let via_resolve = EffectiveThresholds::resolve(&ThresholdsConfig::default()).unwrap();
        let via_default = EffectiveThresholds::default();
        assert!((via_resolve.thermal_amber_c - via_default.thermal_amber_c).abs() < f64::EPSILON);
        assert_eq!(via_resolve.alert_sustain_secs, via_default.alert_sustain_secs);
    }

    /// Override happy path: a config supplying tighter Jetson-style
    /// thermal limits resolves with those values, not the contract
    /// 85 / 95.
    #[test]
    fn override_takes_effect_when_present() {
        let cfg = ThresholdsConfig {
            thermal_amber_c: Some(80.0),
            thermal_red_c: Some(92.0),
            ..Default::default()
        };
        let r = EffectiveThresholds::resolve(&cfg).unwrap();
        assert!((r.thermal_amber_c - 80.0).abs() < f64::EPSILON);
        assert!((r.thermal_red_c - 92.0).abs() < f64::EPSILON);
        // Unset pressure thresholds still pull from the contract.
        use ux_contract::thresholds as d;
        assert!((r.vram_attention_pct - d::VRAM_ATTENTION_PCT).abs() < f64::EPSILON);
    }

    // ── Validation rejections — each invariant produces a specific
    // ConfigError::Invalid with an operator-actionable message ──

    fn err_msg(cfg: ThresholdsConfig) -> String {
        match EffectiveThresholds::resolve(&cfg) {
            Err(ConfigError::Invalid(msg)) => msg,
            Ok(_) => panic!("expected resolve to reject; it accepted"),
            Err(e) => panic!("expected ConfigError::Invalid; got {e:?}"),
        }
    }

    #[test]
    fn rejects_thermal_red_not_greater_than_amber() {
        let msg = err_msg(ThresholdsConfig {
            thermal_amber_c: Some(95.0),
            thermal_red_c: Some(85.0),
            ..Default::default()
        });
        assert!(
            msg.contains("thermal_red_c") && msg.contains("thermal_amber_c"),
            "message must name both fields; got: {msg}"
        );
    }

    #[test]
    fn rejects_thermal_red_equal_to_amber() {
        // Strict greater-than: equal is NOT okay (the buckets would
        // collapse at the boundary).
        let msg = err_msg(ThresholdsConfig {
            thermal_amber_c: Some(85.0),
            thermal_red_c: Some(85.0),
            ..Default::default()
        });
        assert!(msg.contains("thermal_red_c"));
    }

    #[test]
    fn rejects_thermal_amber_non_positive() {
        let msg = err_msg(ThresholdsConfig {
            thermal_amber_c: Some(0.0),
            thermal_red_c: Some(95.0),
            ..Default::default()
        });
        assert!(msg.contains("thermal_amber_c"));
    }

    #[test]
    fn rejects_vram_critical_below_attention() {
        let msg = err_msg(ThresholdsConfig {
            vram_attention_pct: Some(90.0),
            vram_critical_pct: Some(80.0),
            ..Default::default()
        });
        assert!(msg.contains("vram_critical_pct") && msg.contains("vram_attention_pct"));
    }

    #[test]
    fn rejects_ram_critical_below_attention() {
        let msg = err_msg(ThresholdsConfig {
            ram_attention_pct: Some(95.0),
            ram_critical_pct: Some(90.0),
            ..Default::default()
        });
        assert!(msg.contains("ram_critical_pct") && msg.contains("ram_attention_pct"));
    }

    #[test]
    fn rejects_kv_critical_below_attention() {
        let msg = err_msg(ThresholdsConfig {
            kv_attention_pct: Some(90.0),
            kv_critical_pct: Some(80.0),
            ..Default::default()
        });
        assert!(msg.contains("kv_critical_pct") && msg.contains("kv_attention_pct"));
    }

    #[test]
    fn rejects_pct_above_100_typo() {
        // Catches `vram_attention_pct = 850` (operator meant 85.0
        // but dropped the decimal).
        let msg = err_msg(ThresholdsConfig {
            vram_attention_pct: Some(850.0),
            vram_critical_pct: Some(950.0),
            ..Default::default()
        });
        assert!(
            msg.contains("0.0..=100.0"),
            "range-error message must show the expected bounds; got: {msg}"
        );
    }

    #[test]
    fn rejects_pct_below_zero() {
        let msg = err_msg(ThresholdsConfig {
            ram_attention_pct: Some(-5.0),
            ..Default::default()
        });
        assert!(msg.contains("ram_attention_pct"));
    }

    #[test]
    fn rejects_alert_sustain_zero() {
        let msg = err_msg(ThresholdsConfig {
            alert_sustain_secs: Some(0),
            ..Default::default()
        });
        assert!(msg.contains("alert_sustain_secs"));
    }

    #[test]
    fn rejects_alert_sustain_above_600() {
        let msg = err_msg(ThresholdsConfig {
            alert_sustain_secs: Some(601),
            ..Default::default()
        });
        assert!(
            msg.contains("alert_sustain_secs") && msg.contains("600"),
            "message must name the field and the cap; got: {msg}"
        );
    }

    /// Boundary: sustain=1 is the strict-minimum-acceptable value
    /// (a 1-second sustain still smooths a single-tick blip on the
    /// 10 Hz render cadence).
    #[test]
    fn accepts_alert_sustain_at_lower_bound() {
        let r = EffectiveThresholds::resolve(&ThresholdsConfig {
            alert_sustain_secs: Some(1),
            ..Default::default()
        })
        .expect("sustain=1 must be valid (boundary case)");
        assert_eq!(r.alert_sustain_secs, 1);
    }

    /// Boundary: sustain=600 is the strict-maximum-acceptable.
    #[test]
    fn accepts_alert_sustain_at_upper_bound() {
        let r = EffectiveThresholds::resolve(&ThresholdsConfig {
            alert_sustain_secs: Some(600),
            ..Default::default()
        })
        .expect("sustain=600 must be valid (boundary case)");
        assert_eq!(r.alert_sustain_secs, 600);
    }

    /// Boundary: vram_attention=0 and vram_critical=0 both at the
    /// lower edge of `0..=100`. Edge of validity; valid as far as
    /// validate() cares (operator who explicitly sets both to 0 is
    /// deliberately disabling VRAM alerts).
    #[test]
    fn accepts_pct_at_zero_lower_bound() {
        EffectiveThresholds::resolve(&ThresholdsConfig {
            vram_attention_pct: Some(0.0),
            vram_critical_pct: Some(0.0),
            ..Default::default()
        })
        .expect("vram pcts at 0.0 should be valid (operator disabling VRAM alerts)");
    }
}
