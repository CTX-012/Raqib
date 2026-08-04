//! v0.3.13 CAR-22 — `HostVitals` lifted into the contract as the
//! host-level liveness wire type. First field is per-zone thermal
//! readings; power and memory pressure live elsewhere (see notes
//! below).
//!
//! ## Wire-format convention
//!
//! Mirrors [`crate::activity::ActivityState`] and
//! [`crate::WorkloadStatus`]: bare types, no `serde` derives, no
//! `Default` impl. Consumers serialize at their wire boundary
//! (edge_monitor's `src/web/wire.rs`). The
//! `reference_classification_uses_thresholds` test in this
//! module's `#[cfg(test)] mod tests` shows the canonical
//! classification consumers are expected to apply at render time
//! using [`crate::thresholds::THERMAL_AMBER_C`] and
//! [`crate::thresholds::THERMAL_RED_C`].
//!
//! ## Why no embedded severity enum
//!
//! Following the [`crate::WorkloadStatus`] /
//! [`crate::activity::ActivityState`] precedent and the
//! `BAR_ATTENTION_PCT` / `BAR_CRITICAL_PCT` precedent for memory
//! bars: the contract carries raw values plus threshold constants,
//! and the consumer classifies at the renderer. Embedding
//! severity on the wire would force the producer to recompute it
//! on every threshold tweak and would let producer and consumer
//! drift if one cached severity while the other re-classified.
//! Raw temperatures plus thresholds is a single source of truth.
//!
//! ## Why memory pressure is not included
//!
//! Host-level memory pressure (`memory_pct`, `memory_used_mb`,
//! `memory_total_mb`) already ships on the consumer's web wire
//! today via `edge_monitor::web::wire::WireVitals`. Lifting it
//! into the contract here would create a second parallel
//! representation that could drift from the first. v0.3.13 adds
//! thermal only; if memory ever needs contract surface, that's a
//! separate CAR that should consolidate the existing
//! `WireVitals` memory fields in the same change rather than
//! standing up a duplicate.
//!
//! ## CAR-25 (v0.3.16): power rails — INA3221 added
//!
//! The v0.3.13 module deferred power per operator decision; that
//! deferral is closed in v0.3.16 (CAR-25). [`HostVitals`] now
//! carries `power_rails: Vec<PowerRail>` alongside
//! `thermal_zones`, populated on Jetson hosts from
//! `/sys/bus/i2c/drivers/ina3221/<bus-addr>/hwmon/hwmon<N>/`. On
//! x86 the sysfs root is absent — the producer degrades to an
//! empty `Vec<PowerRail>`, identical to the empty-thermal-zones
//! degrade path.
//!
//! Shape decision (computed vs raw): the contract ships
//! [`PowerRail::power_mw`] as computed milliwatts, NOT raw
//! `voltage_mv` + `current_ma`. Inspector's Phase 4 design
//! ratified this shape: there is no per-rail classification
//! tier or threshold on the contract, so the consumer doesn't
//! need raw V and I to derive anything. The display is
//! "rail X: Y W", which is exactly what `power_mw` carries.
//! Shipping V + I would put two numbers on the wire to produce
//! one rendered number, and the producer-side P = V·I / 1000
//! multiply is trivially cheap. This is the deliberate
//! deviation from the dispatch's "raw, mirroring thermal" lean:
//! thermal needs raw because it has classification thresholds
//! ([`crate::thresholds::THERMAL_AMBER_C`] / `THERMAL_RED_C`);
//! power has none.

/// A single thermal zone reading. The label is the canonical zone
/// name (typically the contents of
/// `/sys/class/thermal/thermal_zone*/type` on Linux:
/// `"x86_pkg_temp"`, `"acpitz"`, `"cpu-thermal"`, etc.); the
/// temperature is degrees Celsius.
///
/// Owned [`String`] for the label — labels come from kernel data,
/// not compile-time constants, so `&'static str` would force a
/// specific allocation strategy that the contract does not want
/// to dictate. [`String`] keeps the type [`Clone`] but not
/// [`Copy`]; downstream consumers move or clone as needed.
///
/// `f32` for the temperature: sensors report to roughly 0.1 °C
/// precision, well within `f32` range; using `f64` here would be
/// per-zone padding with no precision gain.
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalZone {
    /// Canonical zone label (e.g. `"x86_pkg_temp"`,
    /// `"cpu-thermal"`).
    pub label: String,
    /// Temperature reading in degrees Celsius.
    pub temp_celsius: f32,
}

/// One INA3221 power rail reading. The label is the canonical
/// rail name as exposed by the kernel driver (e.g. `"VDD_IN"`,
/// `"VDD_CPU_GPU_CV"`, `"VDD_SOC"` on AGX Orin); the value is
/// instantaneous power in milliwatts.
///
/// The producer computes `power_mw` as `voltage_mV * current_mA /
/// 1000` from the INA3221 sysfs files
/// (`/sys/bus/i2c/drivers/ina3221/<addr>/hwmon/hwmon<N>/in<ch>_input`
/// and `curr<ch>_input`). The contract carries the computed
/// value, not the raw V and I — see the module doc-comment
/// ("CAR-25 (v0.3.16): power rails") for the rationale.
///
/// `f32` for `power_mw`: AGX Orin rails range up to ~50 W
/// (`50_000` mW); `f32`'s ~7 significant-digit precision is
/// ample for milliwatt accuracy at that scale.
///
/// Owned [`String`] for the label, matching [`ThermalZone`]:
/// labels come from runtime sysfs reads, not compile-time
/// constants.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerRail {
    /// Canonical rail label (e.g. `"VDD_IN"`,
    /// `"VDD_CPU_GPU_CV"`, `"VDD_SOC"`).
    pub label: String,
    /// Instantaneous power in milliwatts.
    pub power_mw: f32,
}

/// Host-level vitals: the data the consumer renders in the host
/// vitals panel. v0.3.13 introduced thermal zones; v0.3.16
/// (CAR-25) extends with INA3221 power rails. Memory pressure
/// remains outside this struct (see module docs).
///
/// Each field is intentionally a list — sampled per-tick. Empty
/// means "no source discovered" on this host:
///
/// * Empty `thermal_zones` — no `/sys/class/thermal/` exposure
///   (container, exotic kernel, …).
/// * Empty `power_rails` — no INA3221 driver loaded. The most
///   common case is x86 hosts, which have no INA3221 chip and
///   no `/sys/bus/i2c/drivers/ina3221/` directory. Consumers
///   hide the power-rails row when the list is empty, identical
///   to the thermal-empty hide pattern.
///
/// The contract does not distinguish "source absent" from
/// "source present but all reads failed" — that nuance belongs
/// in the consumer's sampler-side tracing, not on the wire.
///
/// Future fields (aggregate memory pressure, fan RPM, …) will
/// be added as additional struct fields in a contract-version
/// bump. Consumers must not pattern-match on a closed set of
/// fields.
#[derive(Debug, Clone, PartialEq)]
pub struct HostVitals {
    /// Per-zone thermal readings sampled this tick.
    pub thermal_zones: Vec<ThermalZone>,
    /// Per-rail INA3221 power readings sampled this tick.
    /// Empty on hosts without an INA3221 chip (x86 dev hosts,
    /// most non-Jetson Linux).
    pub power_rails: Vec<PowerRail>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thresholds::{THERMAL_AMBER_C, THERMAL_RED_C};

    /// `ThermalZone` is constructible from raw kernel-shaped data
    /// (owned label + raw Celsius) without any contract-side
    /// validation. The contract is a pure data carrier; sampler
    /// validation lives in the consumer.
    #[test]
    fn thermal_zone_round_trips_fields() {
        let z = ThermalZone {
            label: "x86_pkg_temp".to_string(),
            temp_celsius: 47.2,
        };
        assert_eq!(z.label, "x86_pkg_temp");
        assert!((z.temp_celsius - 47.2).abs() < f32::EPSILON);
    }

    /// `HostVitals` is constructible empty in both fields. The
    /// empty case represents "no source discovered on this host"
    /// and the consumer must hide the panel rather than render
    /// an empty section (see struct docs). v0.3.16 (CAR-25)
    /// extended this test to include `power_rails`.
    #[test]
    fn host_vitals_empty_thermal_zones_is_valid() {
        let v = HostVitals {
            thermal_zones: Vec::new(),
            power_rails: Vec::new(),
        };
        assert!(v.thermal_zones.is_empty());
        assert!(v.power_rails.is_empty());
    }

    /// `Clone` is part of the contract — consumers pass
    /// `HostVitals` through render layers that may need to retain
    /// the previous tick for delta rendering. Pinned so a future
    /// refactor that drops `Clone` (e.g. by replacing one of the
    /// inner `Vec`s with a non-`Clone` collection) trips this
    /// test first. v0.3.16 (CAR-25) extended this test to
    /// exercise both fields populated.
    #[test]
    fn host_vitals_is_clone() {
        let v = HostVitals {
            thermal_zones: vec![ThermalZone {
                label: "x86_pkg_temp".to_string(),
                temp_celsius: 60.0,
            }],
            power_rails: vec![PowerRail {
                label: "VDD_IN".to_string(),
                power_mw: 4_200.0,
            }],
        };
        let w = v.clone();
        assert_eq!(v, w);
    }

    /// Mirrors how a consumer (edge_monitor's renderer) is
    /// expected to classify a raw temperature into the
    /// nominal / amber / red severity buckets at render time
    /// using the contract's threshold constants. Lives here as a
    /// documented reference implementation so the classification
    /// convention is discoverable on the contract side without
    /// drifting from downstream consumers.
    ///
    /// Mirror of `activity::tests::reference_wire_strings_cover_all_variants`.
    #[test]
    fn reference_classification_uses_thresholds() {
        fn reference_classify(temp_celsius: f32) -> &'static str {
            let c = f64::from(temp_celsius);
            if c >= THERMAL_RED_C {
                "red"
            } else if c >= THERMAL_AMBER_C {
                "amber"
            } else {
                "nominal"
            }
        }
        // Nominal: well below amber.
        assert_eq!(reference_classify(45.0), "nominal");
        // Boundary cases. `>=` semantics: the threshold value
        // itself is the lower edge of the next bucket.
        assert_eq!(reference_classify(THERMAL_AMBER_C as f32 - 0.1), "nominal");
        assert_eq!(reference_classify(THERMAL_AMBER_C as f32), "amber");
        assert_eq!(reference_classify(THERMAL_RED_C as f32 - 0.1), "amber");
        assert_eq!(reference_classify(THERMAL_RED_C as f32), "red");
        // Above red.
        assert_eq!(reference_classify(105.0), "red");
    }

    /// Defensive: the amber threshold must be strictly below the
    /// red threshold and both must be positive. The compile-time
    /// `const _: () = assert!(...)` block in `lib.rs` already
    /// enforces this — the runtime test exists as a discoverable
    /// marker (same pattern as the activity-feed cap tests
    /// added in CAR-19c).
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn thermal_thresholds_are_ordered_and_positive() {
        assert!(THERMAL_AMBER_C > 0.0);
        assert!(THERMAL_RED_C > THERMAL_AMBER_C);
    }

    // ----------------------------------------------------------------
    // CAR-25 (v0.3.16) — INA3221 power rails
    // ----------------------------------------------------------------

    /// `PowerRail` is constructible from raw kernel-shaped data
    /// (canonical rail label + computed milliwatts). The contract
    /// is a pure data carrier; sysfs parsing + the
    /// V·I/1000 multiply live in the consumer (per Inspector
    /// Phase 4 design §4 — `src/platform/ina3221.rs` on the
    /// consumer side).
    ///
    /// The example values mirror what AGX Orin's `VDD_IN` rail
    /// produces at moderate load: ~4.2 W.
    #[test]
    fn power_rail_round_trips_fields() {
        let r = PowerRail {
            label: "VDD_IN".to_string(),
            power_mw: 4_200.0,
        };
        assert_eq!(r.label, "VDD_IN");
        assert!((r.power_mw - 4_200.0).abs() < f32::EPSILON);
    }

    /// **The x86-degrade lock.** On hosts without an INA3221
    /// chip (every x86 dev host, most non-Jetson Linux),
    /// `/sys/bus/i2c/drivers/ina3221/` does not exist and the
    /// consumer's `collect_from_root` returns an empty
    /// `Vec<PowerRail>`. The contract MUST accept this — the
    /// consumer hides the power-rails row on empty, identical
    /// to the empty-thermal-zones hide pattern.
    ///
    /// Pinned so a future edit that adds non-empty validation
    /// (e.g. `assert!(!power_rails.is_empty())`) on the contract
    /// side trips here first; that kind of validation would
    /// break the x86-degrade contract.
    #[test]
    fn host_vitals_empty_power_rails_is_valid() {
        let v = HostVitals {
            thermal_zones: Vec::new(),
            power_rails: Vec::new(),
        };
        assert!(v.power_rails.is_empty());
    }
}
