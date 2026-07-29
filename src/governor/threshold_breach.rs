//! v1.3.2 / DISPATCH 78 / step-3 — threshold-breach projection.
//!
//! Per-PID derived view that [`crate::governor::GovernorExecutor::evaluate`]
//! consumes alongside [`crate::lifecycle::LifecycleSnapshot`]. The
//! projection is the narrow alternative to widening the governor's
//! input signature to `&RuntimeState` — the executor stays decoupled
//! from the runtime's whole state graph and only sees the derived
//! breach summary.
//!
//! ## Scope (Q6 — VRAM%-first)
//!
//! VRAM-only this step. RAM + thermal triggers are a follow-up
//! (step 8 in `docs/PHASE4_AUTOKILL_DESIGN.md`); the type
//! deliberately carries ONLY `vram_pct` / `vram_breached` so a future
//! step-8 dispatch adds the fields explicitly rather than silently
//! inheriting empty defaults. When the projection is extended,
//! `evaluate_process`'s breach gate widens at the same step.
//!
//! ## Observe-only line (still uncrossed)
//!
//! This module computes a READ-only derived view. It allocates a
//! `Vec<ThresholdBreach>` and returns it. It does NOT:
//!
//! * call `send_sigterm` or any signal-emission API,
//! * mutate `pending_kills` or `recent_kills`,
//! * read `governor.auto_actuate`,
//! * add an action-shaped field to any config schema.
//!
//! The four firewalls and three phantom-kill scar layers stay
//! intact: this is a SIGNAL surface, not an ACTUATION surface.
//!
//! ## Honesty discriminator
//!
//! `vram_pct: None` means "not measured" (no GPU on the host, NVML
//! unloaded, the workload never reported VRAM, …). When VRAM is
//! unmeasured `vram_breached` MUST be false — you cannot breach a
//! threshold you can't measure. Same discipline as the D74/D76
//! `VRAM_UNMEASURED` display flag.

use crate::platform::GpuSnapshot;
use crate::runtime::AnnotatedProcess;
use crate::thresholds::EffectiveThresholds;
use ux_contract::host_vitals::HostVitals;

/// Per-PID threshold-breach summary projected from the latest
/// platform snapshot. Built by [`build_threshold_breaches`] at the
/// runtime tick layer; consumed by `evaluate_process` as the kill
/// decision's metric input.
///
/// DISPATCH 84 / step-8 — widens D78's VRAM-only projection to
/// include per-PID RAM. Thermal is host-level (not per-PID) and
/// lives on the sibling [`HostBreach`] struct — see its doc-comment
/// for the rationale.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThresholdBreach {
    pub pid: u32,
    /// Device-relative VRAM percentage. `None` when the workload's
    /// VRAM is unmeasured (no GPU snapshot, NVML failed, the PID
    /// never appeared in `per_process_vram`, total VRAM was zero).
    pub vram_pct: Option<f32>,
    /// `true` IFF `vram_pct` is `Some(p)` AND `p >= vram_critical_pct`.
    /// The unmeasured case (`vram_pct = None`) MUST keep this
    /// `false` — never treat absence as breach. Pinned by
    /// [`tests::unmeasured_vram_never_breaches`].
    pub vram_breached: bool,
    /// DISPATCH 84 — workload-relative RAM percentage:
    /// `rss_mb / (system total RAM in MB) * 100`. `None` when no
    /// platform snapshot is available (very first tick) OR when
    /// the system reports zero total RAM (degenerate read). Same
    /// honesty discriminator as `vram_pct`: absence ≠ zero ≠ breach.
    pub ram_pct: Option<f32>,
    /// `true` IFF `ram_pct` is `Some(p)` AND `p >= ram_critical_pct`.
    /// The unmeasured case (`ram_pct = None`) MUST keep this
    /// `false`. Pinned by [`tests::unmeasured_ram_never_breaches`].
    pub ram_breached: bool,
}

/// DISPATCH 84 / step-8 — HOST-LEVEL breach signal. Thermal is the
/// "shed load because the system is overheating" trigger; it makes
/// no sense as a per-PID field (a thermal zone reading isn't
/// attributable to one workload), so it lives here alongside the
/// per-PID [`ThresholdBreach`] vec.
///
/// ## Multi-zone handling
///
/// Hosts can expose multiple `/sys/class/thermal/thermal_zone*` —
/// the operator's dev host shows two `acpitz` zones; Jetson hosts
/// expose CPU + GPU + SOC + AO + thermal zones. The breach is the
/// MAX across all zones: if ANY zone crosses `thermal_red_c`, the
/// host is overheating. `hottest_zone` carries the label of the
/// zone that drove the decision so the operator sees which sensor
/// flagged it.
///
/// Alternative considered: a configured-zone-only path (operator
/// names the zone label they care about). Rejected for D84 because:
///   * No existing config field; would add schema surface.
///   * Multi-zone hosts vary widely — defaulting to max-across-all
///     is the safe "any zone is hot ⇒ host is hot" stance.
///   * The contract already carries the per-zone reading list for
///     the host vitals panel; the operator sees both zones and can
///     correlate against `hottest_zone` if the breach surprises them.
///
/// ## Honesty (same discipline as ThresholdBreach)
///
/// `max_temp_c = None` ⇒ `thermal_breached = false`, ALWAYS. Empty
/// `thermal_zones` (no `/sys/class/thermal/` exposure, container,
/// exotic kernel) means we can't read temperature, which means we
/// can't claim a breach — the same "absence ≠ breach" stance the
/// per-PID projection enforces for VRAM and RAM.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HostBreach {
    /// `true` IFF [`Self::max_temp_c`] is `Some(t)` AND
    /// `t >= thermal_red_c`. Unmeasured (`None`) stays `false`.
    pub thermal_breached: bool,
    /// Hottest temperature across all zones, in Celsius. `None`
    /// when `thermal_zones` was empty (no zones discovered).
    pub max_temp_c: Option<f32>,
    /// Label of the zone that produced [`Self::max_temp_c`]
    /// (e.g. `"acpitz"`, `"x86_pkg_temp"`, `"GPU-therm"`). `None`
    /// when no temperature was measured.
    pub hottest_zone: Option<String>,
}

/// Build the per-PID threshold-breach projection for one tick.
///
/// Mirrors the per-PID VRAM% computation the v1.1.11 alert path
/// (`runtime::observe_alerts`) already uses — same inputs, same
/// formula. The auto-kill SIGNAL surface and the alert SIGNAL
/// surface read the same projection so an operator who's seen the
/// VRAM alert won't be surprised by a different number on the kill
/// decision.
///
/// Threshold choice: we breach against `vram_critical_pct` (95.0 %
/// by contract default), NOT `vram_attention_pct` (85.0 %). The
/// alert surfaces at attention (the operator should look); the kill
/// surfaces at critical (the workload is about to OOM). Mirrors the
/// CLAUDE.md safety stance — never act on attention alone.
///
/// ## Inputs (the narrow projection per DISPATCH 59 M4 option b)
///
/// * `annotated`: per-PID `AnnotatedProcess`, source of
///   `vram_bytes: Option<u64>`. Populated by the runtime's GPU
///   sampler — `None` when the PID never appeared in
///   `GpuSnapshot::per_process_vram` or NVML failed.
/// * `gpu`: the latest `GpuSnapshot`. Used only for
///   `total_vram_all_devices` (the device-total denominator).
/// * `thresholds`: the resolved [`EffectiveThresholds`] for the
///   tick — accounts for any `[thresholds]` config override (D58).
pub fn build_threshold_breaches(
    annotated: &[AnnotatedProcess],
    gpu: &GpuSnapshot,
    total_system_ram_bytes: Option<u64>,
    thresholds: &EffectiveThresholds,
) -> Vec<ThresholdBreach> {
    // Device-aggregate VRAM denominator. `None` when no GPU is
    // exposed (CPU-only host) OR when every device reports zero
    // total VRAM (NVML returned a degenerate read). Either way we
    // CAN'T compute a percentage, so every PID stays unmeasured.
    let total_vram = {
        let total = gpu.total_vram_all_devices();
        if total > 0 { Some(total) } else { None }
    };
    let vram_critical = thresholds.vram_critical_pct;

    // DISPATCH 84 — RAM denominator. `None` when there's no
    // platform snapshot yet (very first tick) OR the system
    // reported zero total memory (degenerate read on a stub
    // sysinfo). Either way we cannot compute a percent, so every
    // PID stays unmeasured. Same honesty discriminator as VRAM:
    // absence of denominator ⇒ ram_pct = None ⇒ ram_breached =
    // false, never silently zero-fill.
    let total_ram_mb: Option<u64> = total_system_ram_bytes
        .filter(|&b| b > 0)
        .map(|b| b / (1024 * 1024))
        .filter(|&mb| mb > 0);
    let ram_critical = thresholds.ram_critical_pct;

    annotated
        .iter()
        .map(|p| {
            let vram_pct: Option<f32> = match (total_vram, p.vram_bytes) {
                (Some(total), Some(used)) => {
                    Some((used as f64 / total as f64 * 100.0) as f32)
                }
                // Either no GPU (total_vram=None) OR no per-PID VRAM
                // for this workload (vram_bytes=None). HONESTY: we
                // cannot compute a percent → it's None → breach
                // stays false. Never silently zero-fill.
                _ => None,
            };
            let vram_breached = vram_pct.is_some_and(|p| f64::from(p) >= vram_critical);

            // DISPATCH 84 — per-PID RAM%. The PID's RSS as a
            // fraction of the host's total RAM. `rss_mb` comes
            // from the runtime annotation pass (sysinfo-derived);
            // a fresh-just-arrived PID may legitimately have
            // `rss_mb = 0` for a tick, which would compute as 0.0%
            // and never breach — that's correct (a not-yet-loaded
            // workload isn't pressuring RAM).
            let ram_pct: Option<f32> = total_ram_mb.map(|total| {
                (p.rss_mb as f64 / total as f64 * 100.0) as f32
            });
            let ram_breached = ram_pct.is_some_and(|p| f64::from(p) >= ram_critical);

            ThresholdBreach {
                pid: p.pid,
                vram_pct,
                vram_breached,
                ram_pct,
                ram_breached,
            }
        })
        .collect()
}

/// DISPATCH 84 / step-8 — build the host-level breach signal for
/// one tick. Thermal is host-level (not per-PID), so it lives on a
/// separate struct that the executor reads alongside the per-PID
/// [`ThresholdBreach`] vec.
///
/// ## Multi-zone aggregation: MAX
///
/// `thermal_zones` is the per-tick read from `/sys/class/thermal/
/// thermal_zone*`. The breach is the maximum across all zones:
/// if any zone crosses `thermal_red_c`, the host is overheating.
/// `hottest_zone` carries the label of the zone that drove the
/// reading so the operator can correlate against the host-vitals
/// panel.
///
/// Why max-across-all: hosts vary (x86 acpitz × 2, Jetson CPU/GPU/
/// SOC/AO × N, container with no zones). The safe default is "any
/// zone hot ⇒ host hot." A per-zone-configured path could be added
/// later under config — D84 sticks with the safe default and
/// surfaces the chosen zone so the operator can audit the choice.
///
/// ## Honesty: empty zones ≠ breach
///
/// Empty `thermal_zones` (no `/sys/class/thermal/` exposure)
/// produces `max_temp_c = None` and `thermal_breached = false`. We
/// cannot claim a breach against a measurement we don't have.
pub fn build_host_breach(
    vitals: &HostVitals,
    thresholds: &EffectiveThresholds,
) -> HostBreach {
    if vitals.thermal_zones.is_empty() {
        return HostBreach::default();
    }
    let hottest = vitals
        .thermal_zones
        .iter()
        .max_by(|a, b| {
            a.temp_celsius
                .partial_cmp(&b.temp_celsius)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let Some(zone) = hottest else {
        return HostBreach::default();
    };
    let max_temp_c = zone.temp_celsius;
    let thermal_breached = f64::from(max_temp_c) >= thresholds.thermal_red_c;
    HostBreach {
        thermal_breached,
        max_temp_c: Some(max_temp_c),
        hottest_zone: Some(zone.label.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AICategory, WorkloadCategory};
    use std::time::Instant;

    fn ann(pid: u32, vram_bytes: Option<u64>) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: format!("p{pid}"),
            category: AICategory::Inference,
            workload_category: WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes,
            first_observed_at: Instant::now(),
            probe_endpoint: None,
        }
    }

    fn gpu_with_total(total: u64) -> GpuSnapshot {
        use crate::platform::GpuDeviceMetrics;
        use std::collections::HashMap;
        GpuSnapshot {
            devices: vec![GpuDeviceMetrics {
                device_id: 0,
                device_name: "test-gpu".into(),
                total_vram: total,
                used_vram: 0,
                free_vram: total,
                per_process_vram: HashMap::new(),
                power_watts: None,
                temp_c: None,
            }],
        }
    }

    fn thresholds_default() -> EffectiveThresholds {
        EffectiveThresholds::default()
    }

    /// A PID using above-critical VRAM fraction → vram_breached =
    /// true; vram_pct is populated with the actual percentage.
    /// Pin against the contract default (95 %).
    #[test]
    fn pid_above_critical_threshold_is_breached() {
        let thresholds = thresholds_default(); // vram_critical_pct = 95.0
        let gpu = gpu_with_total(10_000); // 10 KB total (test scale)
        // 96% of 10 000 = 9 600 bytes.
        let annotated = vec![ann(100, Some(9_600))];

        let breaches = build_threshold_breaches(&annotated, &gpu, None, &thresholds);
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].pid, 100);
        assert!(breaches[0].vram_breached, "96% should breach 95% threshold");
        let pct = breaches[0].vram_pct.expect("vram_pct populated");
        assert!((pct - 96.0).abs() < 0.01, "expected ~96%, got {pct}");
    }

    /// A PID using below-critical VRAM fraction → not breached.
    /// Confirms the threshold boundary is correctly directional.
    #[test]
    fn pid_below_critical_threshold_is_not_breached() {
        let thresholds = thresholds_default();
        let gpu = gpu_with_total(10_000);
        // 50% of 10 000 = 5 000 — well below 95%.
        let annotated = vec![ann(101, Some(5_000))];

        let breaches = build_threshold_breaches(&annotated, &gpu, None, &thresholds);
        assert!(!breaches[0].vram_breached, "50% must not breach 95%");
        assert_eq!(breaches[0].vram_pct, Some(50.0));
    }

    /// STOP #5 / honesty discriminator — when VRAM is unmeasured
    /// (`vram_bytes = None`, which on the current dev host happens
    /// when the GPU driver is unloaded), `vram_breached` MUST stay
    /// false. Pin BOTH branches: no GPU (total_vram = None) AND
    /// missing per-PID reading.
    #[test]
    fn unmeasured_vram_never_breaches() {
        let thresholds = thresholds_default();

        // (a) GPU snapshot is empty / total_vram = 0.
        let gpu_empty = GpuSnapshot { devices: vec![] };
        let annotated = vec![ann(200, Some(9_999))]; // would breach if pct computable
        let breaches = build_threshold_breaches(&annotated, &gpu_empty, None, &thresholds);
        assert!(
            !breaches[0].vram_breached,
            "no GPU on host → vram_pct None → breach MUST stay false",
        );
        assert_eq!(breaches[0].vram_pct, None);

        // (b) GPU present, but per-PID VRAM not reported for this
        // workload. The PID stays unmeasured.
        let gpu = gpu_with_total(10_000);
        let annotated = vec![ann(201, None)];
        let breaches = build_threshold_breaches(&annotated, &gpu, None, &thresholds);
        assert!(
            !breaches[0].vram_breached,
            "per-PID VRAM unmeasured → breach MUST stay false (honesty)",
        );
        assert_eq!(breaches[0].vram_pct, None);
    }

    /// Multiple PIDs in a single projection: breach state is
    /// computed per-row, no cross-talk.
    #[test]
    fn per_pid_breach_independence() {
        let thresholds = thresholds_default();
        let gpu = gpu_with_total(10_000);
        let annotated = vec![
            ann(300, Some(9_600)), // breaches
            ann(301, Some(5_000)), // does not
            ann(302, None),        // unmeasured
        ];

        let breaches = build_threshold_breaches(&annotated, &gpu, None, &thresholds);
        assert_eq!(breaches.len(), 3);
        assert!(breaches[0].vram_breached);
        assert!(!breaches[1].vram_breached);
        assert!(!breaches[2].vram_breached);
        assert!(breaches[2].vram_pct.is_none());
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 84 / step-8 — RAM + thermal widening.
    // ─────────────────────────────────────────────────────────────

    fn ann_with_rss(pid: u32, rss_mb: u64) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: format!("p{pid}"),
            category: AICategory::Inference,
            workload_category: WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb,
            vram_bytes: None,
            first_observed_at: Instant::now(),
            probe_endpoint: None,
        }
    }

    /// RAM breach: a PID's RSS above `ram_critical_pct` of host
    /// total RAM produces `ram_breached = true` and a populated
    /// `ram_pct` reflecting the actual percentage. Pin against the
    /// contract default (95 %).
    #[test]
    fn pid_above_critical_ram_threshold_is_breached() {
        let thresholds = thresholds_default(); // ram_critical_pct = 95.0
        let gpu = gpu_with_total(10_000);
        // 1 GB total RAM, 980 MB rss → 95.7%
        let total_ram_bytes = 1024u64 * 1024 * 1024;
        let annotated = vec![ann_with_rss(400, 980)];

        let breaches =
            build_threshold_breaches(&annotated, &gpu, Some(total_ram_bytes), &thresholds);
        assert_eq!(breaches.len(), 1);
        assert!(
            breaches[0].ram_breached,
            "rss 980 MB / 1024 MB = 95.7% must breach 95% threshold"
        );
        let pct = breaches[0].ram_pct.expect("ram_pct populated");
        assert!(
            (pct - 95.7).abs() < 0.2,
            "expected ~95.7%, got {pct}"
        );
    }

    /// RAM unmeasured (None total) ⇒ ram_pct=None ⇒ ram_breached=false.
    /// THE HONESTY DISCRIMINATOR for RAM, mirroring
    /// `unmeasured_vram_never_breaches`. Even with an enormous rss_mb
    /// value, no denominator means no breach claim.
    #[test]
    fn unmeasured_ram_never_breaches() {
        let thresholds = thresholds_default();
        let gpu = gpu_with_total(10_000);
        // Big rss but no total_ram denominator → ram_pct=None.
        let annotated = vec![ann_with_rss(401, 999_999)];

        let breaches = build_threshold_breaches(&annotated, &gpu, None, &thresholds);
        assert!(
            !breaches[0].ram_breached,
            "no total RAM denominator → ram_pct None → breach MUST stay false"
        );
        assert_eq!(breaches[0].ram_pct, None);

        // Also pin: zero total RAM (degenerate read) ⇒ unmeasured.
        let breaches_zero =
            build_threshold_breaches(&annotated, &gpu, Some(0), &thresholds);
        assert!(
            !breaches_zero[0].ram_breached,
            "zero total RAM (degenerate sysinfo read) → ram_pct None → breach false"
        );
        assert_eq!(breaches_zero[0].ram_pct, None);
    }

    /// Thermal: a single zone above `thermal_red_c` produces a host
    /// breach with the zone's label surfaced as `hottest_zone`.
    /// Pin the contract default (`THERMAL_RED_C`).
    #[test]
    fn thermal_above_red_threshold_is_breached_with_zone_label() {
        use ux_contract::host_vitals::{HostVitals, ThermalZone};
        let thresholds = thresholds_default();
        let vitals = HostVitals {
            thermal_zones: vec![ThermalZone {
                label: "x86_pkg_temp".into(),
                temp_celsius: 95.0,
            }],
            power_rails: Vec::new(),
        };

        let host = build_host_breach(&vitals, &thresholds);
        assert!(host.thermal_breached, "95°C must breach thermal_red_c");
        assert_eq!(host.max_temp_c, Some(95.0));
        assert_eq!(host.hottest_zone.as_deref(), Some("x86_pkg_temp"));
    }

    /// Multi-zone (the OPERATOR'S LIVE CASE — 2 acpitz zones on x86).
    /// The HOTTEST zone wins; the breach flag reflects max-across-all.
    /// `hottest_zone` carries the winning label so the operator can
    /// correlate against the host-vitals panel.
    #[test]
    fn multi_zone_thermal_takes_max_and_surfaces_label() {
        use ux_contract::host_vitals::{HostVitals, ThermalZone};
        let thresholds = thresholds_default();
        // Operator's actual hardware: two acpitz zones.
        let vitals = HostVitals {
            thermal_zones: vec![
                ThermalZone {
                    label: "acpitz".into(),
                    temp_celsius: 55.0,
                },
                ThermalZone {
                    label: "acpitz".into(),
                    temp_celsius: 96.5, // hotter — drives the breach
                },
            ],
            power_rails: Vec::new(),
        };
        let host = build_host_breach(&vitals, &thresholds);
        assert!(
            host.thermal_breached,
            "max-across-zones (96.5°C) must breach thermal_red_c (95.0°C) even when \
             ONE zone is cool"
        );
        assert_eq!(
            host.max_temp_c,
            Some(96.5),
            "max_temp_c must report the hotter zone, not an average"
        );
        assert_eq!(host.hottest_zone.as_deref(), Some("acpitz"));

        // Sanity: when ALL zones are well below threshold, no breach.
        let vitals_cool = HostVitals {
            thermal_zones: vec![
                ThermalZone {
                    label: "acpitz".into(),
                    temp_celsius: 45.0,
                },
                ThermalZone {
                    label: "acpitz".into(),
                    temp_celsius: 55.0,
                },
            ],
            power_rails: Vec::new(),
        };
        let host_cool = build_host_breach(&vitals_cool, &thresholds);
        assert!(!host_cool.thermal_breached);
        assert_eq!(host_cool.max_temp_c, Some(55.0));
    }

    /// THE HONESTY DISCRIMINATOR for thermal: empty `thermal_zones`
    /// (no `/sys/class/thermal/` exposure — container, exotic kernel,
    /// stripped-down sandbox) ⇒ `max_temp_c = None` ⇒
    /// `thermal_breached = false`. We cannot claim a breach against
    /// a measurement we don't have. Mirrors `unmeasured_vram_never_breaches`
    /// and `unmeasured_ram_never_breaches`.
    #[test]
    fn unmeasured_thermal_never_breaches() {
        use ux_contract::host_vitals::HostVitals;
        let thresholds = thresholds_default();
        let vitals = HostVitals {
            thermal_zones: Vec::new(),
            power_rails: Vec::new(),
        };
        let host = build_host_breach(&vitals, &thresholds);
        assert!(
            !host.thermal_breached,
            "empty thermal_zones MUST yield thermal_breached=false. \
             Same discipline as unmeasured VRAM/RAM."
        );
        assert_eq!(host.max_temp_c, None);
        assert_eq!(host.hottest_zone, None);
    }
}
