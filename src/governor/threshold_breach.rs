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

/// Per-PID threshold-breach summary projected from the latest
/// platform snapshot. Built by [`build_threshold_breaches`] at the
/// runtime tick layer; consumed by `evaluate_process` as the kill
/// decision's metric input.
///
/// VRAM-only for step-3 (Q6 — VRAM%-first). RAM / thermal fields
/// land in the step-8 dispatch.
#[derive(Debug, Clone, PartialEq)]
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
    let critical = thresholds.vram_critical_pct;

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
            let vram_breached = vram_pct.is_some_and(|p| f64::from(p) >= critical);
            ThresholdBreach {
                pid: p.pid,
                vram_pct,
                vram_breached,
            }
        })
        .collect()
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

        let breaches = build_threshold_breaches(&annotated, &gpu, &thresholds);
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

        let breaches = build_threshold_breaches(&annotated, &gpu, &thresholds);
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
        let breaches = build_threshold_breaches(&annotated, &gpu_empty, &thresholds);
        assert!(
            !breaches[0].vram_breached,
            "no GPU on host → vram_pct None → breach MUST stay false",
        );
        assert_eq!(breaches[0].vram_pct, None);

        // (b) GPU present, but per-PID VRAM not reported for this
        // workload. The PID stays unmeasured.
        let gpu = gpu_with_total(10_000);
        let annotated = vec![ann(201, None)];
        let breaches = build_threshold_breaches(&annotated, &gpu, &thresholds);
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

        let breaches = build_threshold_breaches(&annotated, &gpu, &thresholds);
        assert_eq!(breaches.len(), 3);
        assert!(breaches[0].vram_breached);
        assert!(!breaches[1].vram_breached);
        assert!(!breaches[2].vram_breached);
        assert!(breaches[2].vram_pct.is_none());
    }
}
