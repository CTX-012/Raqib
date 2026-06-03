//! v1.2.0 / DISPATCH 45 / CAR-23 — recommendation projection
//! from `RuntimeState` (derived view).
//!
//! ## AUTHORITY LOCK (binding, operator sign-off)
//!
//! Recommendations are **display strings the user reads**. This
//! module:
//!
//! - Reads `&RuntimeState` ONLY (read-only borrow).
//! - Returns `Vec<ux_contract::recommendation::Recommendation>`
//!   (pure data values built from the contract's bare types).
//! - Has NO `Executor`, no `Box<dyn Fn>`, no signal handle, no
//!   file descriptor, no callback. There is no method on any
//!   value returned from here that can send a signal.
//!
//! The contract enforces "discriminator, not callable" at the
//! type level via `SuggestedAction: Copy` and the
//! `suggested_action_is_copy` test. This module enforces
//! "consumer wiring stays observe-only" via the function signature
//! above and the `recommendation_path_has_no_actuation_handle`
//! test in C5 of DISPATCH 45 (the consumer-side lock-as-test
//! guard). Together they pin the firewall at both boundaries:
//! the type (contract) and the wiring (consumer).
//!
//! The user acts via the existing manual `k` → kill_confirm card
//! → SIGTERM path. v1.2.0 changes nothing about that path.
//!
//! ## Derived view, not stored state
//!
//! Recommendations are derived at render time from the alerts the
//! AlertState owns. There is no new state machine, no new
//! ack/clear lifecycle. When the underlying alert clears, the
//! recommendation disappears with it.
//!
//! ## Signal table (Inspector DISPATCH 41 §3)
//!
//! | AlertId            | Action               | Scope    | Targets       |
//! |---                 | ---                  | ---      | ---           |
//! | `VramPressure`     | `ConsiderKill`       | Workload | single (PID)  |
//! | `KvPressure`       | `ConsiderKill`       | Workload | single (PID)  |
//! | `RamPressure`      | `ConsiderKill`       | System   | top-3 by rss  |
//! | `ThermalPressure`  | `ConsiderReduceLoad` | System   | EMPTY         |
//! | `OomDetected`      | `ConsiderRestart`    | Workload | single (PID)  |
//! | `WorkloadExited`   | (suppressed)         | -        | -             |
//! | `GovernorArmed`    | (suppressed)         | -        | -             |
//!
//! `WorkloadExited` is suppressed because the workload is gone —
//! a "consider restart" rec here would be ambiguous (restart what
//! pid? the dead one's pid is reused), and a "consider kill" rec
//! is nonsensical for a dead PID.
//!
//! `GovernorArmed` is suppressed because the operator has already
//! manually opened the kill_confirm card targeting that PID — the
//! action is already being taken; recommending it would be
//! tautological.
//!
//! ## Severity classification (Inspector DISPATCH 41 §5)
//!
//! - `OomDetected` always → `Critical`.
//! - `ThermalPressure` → `Critical` when the offending zone is at
//!   or above `THERMAL_RED_C` (95 °C), else `Warning`.
//! - All other pressure recs → `Warning`.
//!
//! Severity drives ordering at the renderer (Critical first) and
//! the color choice in both TUI and Svelte.

use ux_contract::AlertId;
use ux_contract::recommendation::{
    Recommendation, RecommendationScope, RecommendationSeverity,
    RecommendedTarget, SuggestedAction,
};

use crate::runtime::RuntimeState;
use crate::ui::alerts::AlertEntry;

/// v1.2.0 / DISPATCH 45 — project the currently-visible alerts
/// into recommendations. Pure derived view: no stored state, no
/// side effects, no actuation paths.
///
/// The output is capped at
/// [`ux_contract::limits::REC_MAX_VISIBLE`] (3); recs above the
/// cap are dropped at the producer because the renderer surface
/// is fixed-height (matches the alert region's auto-hide budget).
/// Per-target list is independently capped at
/// [`ux_contract::limits::REC_TARGETS_MAX`] (3).
///
/// **AUTHORITY LOCK** (re-asserted at this entry point): the
/// signature carries only `&RuntimeState` (a read-only borrow)
/// and returns a `Vec` of pure-data structs. A future edit that
/// adds an actuator parameter (kill signal, executor, governor
/// handle) would break the C5 lock-as-test guard and would also
/// need to delete the `suggested_action_is_copy` contract test —
/// both being deletion-gated review surfaces.
pub fn project_recommendations(state: &RuntimeState) -> Vec<Recommendation> {
    let visible = state.alerts.visible();
    let mut out: Vec<Recommendation> = visible
        .iter()
        .filter_map(|entry| project_one(entry, state))
        .collect();
    // Sort by severity (Critical first), preserving stable order
    // within a tier so the existing `AlertState::visible()`
    // priority ordering carries through for ties.
    out.sort_by_key(|r| severity_sort_key(r.severity));
    // Cap at REC_MAX_VISIBLE. The contract pins
    // `REC_MAX_VISIBLE <= ALERT_MAX_VISIBLE` so this cap can never
    // exceed the alert section's own budget.
    out.truncate(ux_contract::limits::REC_MAX_VISIBLE);
    out
}

/// Project ONE alert entry into a recommendation, per the signal
/// table in the module docs. Returns `None` for alerts that are
/// suppressed (`WorkloadExited`, `GovernorArmed`).
///
/// `pub(crate)` so the per-alert test cases can drive each branch
/// without round-tripping through a full RuntimeState. Production
/// callers go through `project_recommendations`.
pub(crate) fn project_one(
    entry: &AlertEntry,
    state: &RuntimeState,
) -> Option<Recommendation> {
    match entry.alert_id {
        AlertId::VramPressure | AlertId::KvPressure => {
            // Single-target workload-scope. Drop if no PID
            // (defensive — these alerts are always workload-scope
            // in practice).
            let pid = entry.pid?;
            Some(Recommendation {
                alert_id: entry.alert_id,
                scope: RecommendationScope::Workload,
                targets: vec![RecommendedTarget {
                    pid,
                    name: entry.workload_name.clone(),
                    evidence: vram_or_kv_evidence(entry.alert_id, pid, state),
                }],
                action: SuggestedAction::ConsiderKill,
                severity: RecommendationSeverity::Warning,
                reason: vram_or_kv_reason(entry.alert_id, pid, state),
            })
        }
        AlertId::RamPressure => {
            // System-scope multi-target: top-3 AI processes by
            // rss_mb. The recommendation enumerates contributors
            // so the operator can pick one; the existing manual
            // `k` flow is the action surface for the pick.
            let mut by_rss: Vec<(u32, String, u64)> = state
                .ai_processes()
                .map(|p| (p.pid, p.name.clone(), p.rss_mb))
                .collect();
            // Largest-rss-first.
            by_rss.sort_by_key(|t| std::cmp::Reverse(t.2));
            let cap = ux_contract::limits::REC_TARGETS_MAX;
            let targets: Vec<RecommendedTarget> = by_rss
                .into_iter()
                .take(cap)
                .map(|(pid, name, rss_mb)| RecommendedTarget {
                    pid,
                    name,
                    evidence: Some(format!("rss_mb={rss_mb}")),
                })
                .collect();
            // System-scope rec with NO AI workloads in the
            // snapshot still makes sense ("consider reducing
            // load" implied by the alert), but per the signal
            // table the action is ConsiderKill which expects
            // named contributors. If there are no AI processes,
            // suppress the rec rather than emit an empty
            // multi-target.
            if targets.is_empty() {
                return None;
            }
            Some(Recommendation {
                alert_id: entry.alert_id,
                scope: RecommendationScope::System,
                targets,
                action: SuggestedAction::ConsiderKill,
                severity: RecommendationSeverity::Warning,
                reason: ram_pressure_reason(state),
            })
        }
        AlertId::ThermalPressure => {
            // System-scope, EMPTY targets (honest: thermal is
            // not per-PID attributable on Linux — zones are
            // whole-die / chip-level).
            //
            // v1.3.1 — severity bump uses the resolved
            // `thermal_red_c` from RuntimeState so an operator's
            // [thresholds] override (typical Jetson Tj_max tuning)
            // reaches the Critical/Warning split.
            let hottest = hottest_zone_temp(state);
            let crossed_red = hottest
                .is_some_and(|t| f64::from(t) >= state.thresholds.thermal_red_c);
            let severity = if crossed_red {
                RecommendationSeverity::Critical
            } else {
                RecommendationSeverity::Warning
            };
            Some(Recommendation {
                alert_id: entry.alert_id,
                scope: RecommendationScope::System,
                targets: Vec::new(),
                action: SuggestedAction::ConsiderReduceLoad,
                severity,
                reason: thermal_pressure_reason(hottest),
            })
        }
        AlertId::OomDetected => {
            // Post-exit single-target rec. The PID stamped on
            // the alert IS the exited workload's PID; the
            // operator can use it to find the model in their
            // launcher / orchestrator.
            let pid = entry.pid?;
            Some(Recommendation {
                alert_id: entry.alert_id,
                scope: RecommendationScope::Workload,
                targets: vec![RecommendedTarget {
                    pid,
                    name: entry.workload_name.clone(),
                    evidence: None,
                }],
                action: SuggestedAction::ConsiderRestart,
                severity: RecommendationSeverity::Critical,
                reason: format!(
                    "{} (PID {pid}) was OOM-killed by the kernel",
                    entry.workload_name,
                ),
            })
        }
        // Suppressed: see module docs. Both are alerts that
        // exist for operator awareness but don't have a
        // sensible "consider …" recommendation (WorkloadExited
        // has no live PID; GovernorArmed already has the
        // operator's manual flow open).
        AlertId::WorkloadExited | AlertId::GovernorArmed => None,
    }
}

// NOTE: the per-variant matching above is intentionally
// exhaustive (no `_ => None` wildcard) so a new `AlertId`
// variant added to ux_contract surfaces as a compile error
// here, forcing an explicit projection (or explicit suppression)
// decision rather than silently dropping the new variant.

/// Severity sort key. Critical sorts first (lowest u8), Warning
/// second, Info last. Stable sort within tier preserves the
/// upstream `AlertState::visible()` priority order.
fn severity_sort_key(severity: RecommendationSeverity) -> u8 {
    match severity {
        RecommendationSeverity::Critical => 0,
        RecommendationSeverity::Warning => 1,
        RecommendationSeverity::Info => 2,
    }
}

/// VRAM / KV evidence for a per-PID rec.
fn vram_or_kv_evidence(
    id: AlertId,
    pid: u32,
    state: &RuntimeState,
) -> Option<String> {
    match id {
        AlertId::VramPressure => state
            .annotated
            .iter()
            .find(|a| a.pid == pid)
            .and_then(|a| a.vram_bytes)
            .map(|b| format!("vram_mb={}", b / (1024 * 1024))),
        AlertId::KvPressure => state
            .live_telemetry
            .get(&pid)
            .and_then(|lt| lt.kv_cache_peak_pct)
            .map(|pct| format!("kv_cache_pct={pct:.0}")),
        _ => None,
    }
}

/// Producer-formatted rationale rendered as the sub-text under
/// the action label. Keeps the wording compact and consistent
/// between TUI and web — both consume this string verbatim.
fn vram_or_kv_reason(id: AlertId, pid: u32, state: &RuntimeState) -> String {
    let name = state
        .annotated
        .iter()
        .find(|a| a.pid == pid)
        .map(|a| a.name.as_str())
        .unwrap_or("workload");
    match id {
        AlertId::VramPressure => format!("{name} (PID {pid}) is the top VRAM consumer"),
        AlertId::KvPressure => format!("{name} (PID {pid}) is the top KV-cache consumer"),
        _ => format!("{name} (PID {pid})"),
    }
}

fn ram_pressure_reason(state: &RuntimeState) -> String {
    let pct = state
        .last_snapshot
        .as_ref()
        .map(|s| s.system.memory_usage_percent());
    match pct {
        Some(p) => format!("System RAM at {p:.0}% — top contributors"),
        None => "System RAM pressure — top contributors".to_string(),
    }
}

fn hottest_zone_temp(state: &RuntimeState) -> Option<f32> {
    state.last_snapshot.as_ref().and_then(|s| {
        s.vitals
            .thermal_zones
            .iter()
            .map(|z| z.temp_celsius)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    })
}

fn thermal_pressure_reason(hottest: Option<f32>) -> String {
    match hottest {
        Some(t) => format!("Hottest zone at {t:.1} °C — reduce sustained load"),
        None => "Thermal pressure detected — reduce sustained load".to_string(),
    }
}

/// Resolve the appropriate `display::*` template for a
/// recommendation's action + target shape. Mirrors the
/// `reference_label_for_action_covers_all_variants` contract test
/// at `ux_contract::recommendation::tests`. Public so the TUI
/// renderer (C4) and the web wire builder (C3) can both go
/// through one substitution pipeline.
pub(crate) fn label_template_for(rec: &Recommendation) -> &'static str {
    use ux_contract::recommendation::display;
    match rec.action {
        SuggestedAction::ConsiderKill => {
            if rec.targets.len() > 1 {
                display::CONSIDER_KILL_MULTI
            } else {
                display::CONSIDER_KILL_SINGLE
            }
        }
        SuggestedAction::ConsiderReduceLoad => display::CONSIDER_REDUCE_LOAD,
        SuggestedAction::ConsiderRestart => display::CONSIDER_RESTART,
    }
}

/// Apply `{pid}` / `{name}` / `{targets}` substitutions to a
/// label template. Mirrors the alert-template `substitute()`
/// pipeline. Unknown tokens are left in place (defensive — same
/// shape as the alerts substitution).
pub(crate) fn render_label(rec: &Recommendation) -> String {
    let template = label_template_for(rec);
    let mut out = template.to_string();
    // For single-target shapes, substitute `{pid}` / `{name}`
    // from the lone target. For multi-target shapes, substitute
    // `{targets}` from the formatted list.
    if rec.targets.len() == 1 {
        let t = &rec.targets[0];
        out = out.replace("{pid}", &t.pid.to_string());
        out = out.replace("{name}", &t.name);
    }
    let joined: String = rec
        .targets
        .iter()
        .map(|t| format!("{} (PID {})", t.name, t.pid))
        .collect::<Vec<_>>()
        .join(", ");
    out = out.replace("{targets}", &joined);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::{AICategory, WorkloadCategory};
    use crate::runtime::{AnnotatedProcess, Runtime};
    use crate::ui::alerts::WorkloadRef;
    use std::time::Instant;

    fn ann(pid: u32, name: &str, rss_mb: u64) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: AICategory::Inference,
            workload_category: WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        }
    }

    fn empty_gpu() -> crate::platform::GpuSnapshot {
        crate::platform::GpuSnapshot { devices: vec![] }
    }

    /// v1.2.0 / DISPATCH 45 — VRAM-pressure alert projects to a
    /// single-target ConsiderKill recommendation with Workload
    /// scope. The target's PID matches the alert's PID.
    #[test]
    fn vram_pressure_projects_to_single_target_consider_kill() {
        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let state = runtime.state_mut();
        let now = Instant::now();
        state.alerts.observe(
            now,
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::VramPressure,
            true,
        );
        state.alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::VramPressure,
            true,
        );
        let recs = project_recommendations(runtime.state());
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.alert_id, AlertId::VramPressure);
        assert_eq!(r.scope, RecommendationScope::Workload);
        assert_eq!(r.action, SuggestedAction::ConsiderKill);
        assert_eq!(r.severity, RecommendationSeverity::Warning);
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].pid, 4523);
        assert_eq!(r.targets[0].name, "Llama-70B");
    }

    /// v1.2.0 / DISPATCH 45 — RAM-pressure alert projects to a
    /// System-scope multi-target rec ranked by rss_mb desc,
    /// capped at REC_TARGETS_MAX.
    #[test]
    fn ram_pressure_projects_to_top_n_by_rss() {
        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        // Seed AI processes with descending rss values.
        let state_mut = runtime.state_mut();
        state_mut.annotated = vec![
            ann(1, "p1", 100),
            ann(2, "p2", 500),
            ann(3, "p3", 300),
            ann(4, "p4", 800),
            ann(5, "p5", 200),
        ];
        let now = Instant::now();
        // RAM pressure is instant-fire system-scope; observe
        // once-twice across the sustain window.
        state_mut.alerts.observe(
            now,
            WorkloadRef::system(),
            AlertId::RamPressure,
            true,
        );
        state_mut.alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::system(),
            AlertId::RamPressure,
            true,
        );

        let recs = project_recommendations(runtime.state());
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.scope, RecommendationScope::System);
        assert_eq!(r.action, SuggestedAction::ConsiderKill);
        // top-3 by rss: p4 (800), p2 (500), p3 (300).
        let cap = ux_contract::limits::REC_TARGETS_MAX;
        assert_eq!(r.targets.len(), cap);
        let pids: Vec<u32> = r.targets.iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![4, 2, 3]);
        // Evidence is rss-shaped.
        assert!(r.targets[0]
            .evidence
            .as_deref()
            .is_some_and(|e| e.contains("rss_mb=800")));
    }

    /// v1.2.0 / DISPATCH 45 — ThermalPressure alert projects to a
    /// System-scope, EMPTY-targets ConsiderReduceLoad rec. The
    /// severity bumps to Critical when the hottest zone is at or
    /// above THERMAL_RED_C.
    #[test]
    fn thermal_pressure_projects_to_empty_target_reduce_load() {
        use crate::platform::PlatformSnapshot;
        use ux_contract::host_vitals::{HostVitals, ThermalZone};

        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        // Seed a snapshot with a moderately-hot zone (between
        // amber and red).
        runtime.state_mut().last_snapshot = Some(PlatformSnapshot {
            timestamp: chrono::Utc::now(),
            system: crate::platform::SystemMetrics {
                timestamp: chrono::Utc::now(),
                total_memory: 16 * 1024 * 1024 * 1024,
                used_memory: 8 * 1024 * 1024 * 1024,
                available_memory: 8 * 1024 * 1024 * 1024,
                cpu_count: 8,
                load_average: [0.0, 0.0, 0.0],
            },
            processes: vec![],
            gpu: empty_gpu(),
            vitals: HostVitals {
                thermal_zones: vec![ThermalZone {
                    label: "x86_pkg_temp".into(),
                    temp_celsius: 90.0, // amber but not red
                }],
                power_rails: Vec::new(),
            },
        });
        let now = Instant::now();
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::system(),
            AlertId::ThermalPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::system(),
            AlertId::ThermalPressure,
            true,
        );

        let recs = project_recommendations(runtime.state());
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.alert_id, AlertId::ThermalPressure);
        assert_eq!(r.scope, RecommendationScope::System);
        assert_eq!(r.action, SuggestedAction::ConsiderReduceLoad);
        assert!(
            r.targets.is_empty(),
            "thermal recs MUST have empty targets — thermal is \
             not per-PID attributable",
        );
        // Below red → Warning.
        assert_eq!(r.severity, RecommendationSeverity::Warning);
    }

    /// v1.2.0 / DISPATCH 45 — ThermalPressure with a zone at or
    /// above THERMAL_RED_C bumps severity to Critical.
    #[test]
    fn thermal_pressure_critical_when_red_threshold_crossed() {
        use crate::platform::PlatformSnapshot;
        use ux_contract::host_vitals::{HostVitals, ThermalZone};

        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        runtime.state_mut().last_snapshot = Some(PlatformSnapshot {
            timestamp: chrono::Utc::now(),
            system: crate::platform::SystemMetrics {
                timestamp: chrono::Utc::now(),
                total_memory: 0,
                used_memory: 0,
                available_memory: 0,
                cpu_count: 0,
                load_average: [0.0, 0.0, 0.0],
            },
            processes: vec![],
            gpu: empty_gpu(),
            vitals: HostVitals {
                thermal_zones: vec![ThermalZone {
                    label: "x86_pkg_temp".into(),
                    temp_celsius: 96.5, // above red (95.0)
                }],
                power_rails: Vec::new(),
            },
        });
        let now = Instant::now();
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::system(),
            AlertId::ThermalPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::system(),
            AlertId::ThermalPressure,
            true,
        );

        let recs = project_recommendations(runtime.state());
        let r = &recs[0];
        assert_eq!(r.severity, RecommendationSeverity::Critical);
    }

    /// v1.2.0 / DISPATCH 45 — OomDetected projects to a
    /// Critical-severity ConsiderRestart rec on the exited PID.
    #[test]
    fn oom_detected_projects_to_consider_restart_critical() {
        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let now = Instant::now();
        runtime.state_mut().alerts.observe_exit(
            now,
            WorkloadRef::workload(206, "phi3"),
            AlertId::OomDetected,
            None,
        );
        let recs = project_recommendations(runtime.state());
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.action, SuggestedAction::ConsiderRestart);
        assert_eq!(r.severity, RecommendationSeverity::Critical);
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].pid, 206);
    }

    /// v1.2.0 / DISPATCH 45 — `WorkloadExited` and `GovernorArmed`
    /// are SUPPRESSED (no recommendation). See module docs for
    /// the rationale.
    #[test]
    fn workload_exited_and_governor_armed_are_suppressed() {
        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let now = Instant::now();
        runtime.state_mut().alerts.observe_exit(
            now,
            WorkloadRef::workload(206, "phi3"),
            AlertId::WorkloadExited,
            Some("exit code 139".into()),
        );
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(207, "vllm"),
            AlertId::GovernorArmed,
            true,
        );
        let recs = project_recommendations(runtime.state());
        // Both alerts visible but neither projects to a rec.
        assert_eq!(runtime.state().alerts.visible().len(), 2);
        assert!(
            recs.is_empty(),
            "WorkloadExited and GovernorArmed must NOT produce \
             recommendations — see module docs for the rationale. \
             Got: {recs:?}",
        );
    }

    /// v1.2.0 / DISPATCH 45 — recs are sorted by severity, Critical
    /// first; then capped at REC_MAX_VISIBLE so the renderer
    /// always sees a bounded list.
    #[test]
    fn recommendations_sorted_critical_first_and_capped() {
        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let now = Instant::now();
        // 4 alerts → 4 recs candidate, but capped at
        // REC_MAX_VISIBLE = 3. Critical (Oom) must sort first.
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(1, "a"),
            AlertId::VramPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::workload(1, "a"),
            AlertId::VramPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(2, "b"),
            AlertId::KvPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::workload(2, "b"),
            AlertId::KvPressure,
            true,
        );
        runtime.state_mut().alerts.observe_exit(
            now,
            WorkloadRef::workload(3, "c"),
            AlertId::OomDetected,
            None,
        );
        // 4th alert candidate (also Warning).
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(4, "d"),
            AlertId::KvPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now + std::time::Duration::from_secs(5),
            WorkloadRef::workload(4, "d"),
            AlertId::KvPressure,
            true,
        );

        let recs = project_recommendations(runtime.state());
        let cap = ux_contract::limits::REC_MAX_VISIBLE;
        assert!(
            recs.len() <= cap,
            "rec count must not exceed REC_MAX_VISIBLE ({cap}): got {}",
            recs.len(),
        );
        // First rec is Critical (the OOM).
        assert_eq!(recs[0].severity, RecommendationSeverity::Critical);
    }

    /// v1.2.0 / DISPATCH 45 — the label-template lookup mirrors
    /// the contract reference impl. Pinned so a future refactor
    /// that swaps the lookup for a function call (breaking the
    /// observe-only firewall) trips here.
    #[test]
    fn label_template_matches_contract_lookup() {
        use ux_contract::recommendation::display;

        let single_kill = Recommendation {
            alert_id: AlertId::VramPressure,
            scope: RecommendationScope::Workload,
            targets: vec![RecommendedTarget {
                pid: 1,
                name: "x".into(),
                evidence: None,
            }],
            action: SuggestedAction::ConsiderKill,
            severity: RecommendationSeverity::Warning,
            reason: "x".into(),
        };
        assert_eq!(label_template_for(&single_kill), display::CONSIDER_KILL_SINGLE);

        let multi_kill = Recommendation {
            targets: vec![
                RecommendedTarget {
                    pid: 1,
                    name: "x".into(),
                    evidence: None,
                },
                RecommendedTarget {
                    pid: 2,
                    name: "y".into(),
                    evidence: None,
                },
            ],
            ..single_kill.clone()
        };
        assert_eq!(label_template_for(&multi_kill), display::CONSIDER_KILL_MULTI);

        let reduce = Recommendation {
            alert_id: AlertId::ThermalPressure,
            scope: RecommendationScope::System,
            targets: Vec::new(),
            action: SuggestedAction::ConsiderReduceLoad,
            severity: RecommendationSeverity::Warning,
            reason: "x".into(),
        };
        assert_eq!(label_template_for(&reduce), display::CONSIDER_REDUCE_LOAD);

        let restart = Recommendation {
            alert_id: AlertId::OomDetected,
            scope: RecommendationScope::Workload,
            targets: vec![RecommendedTarget {
                pid: 3,
                name: "z".into(),
                evidence: None,
            }],
            action: SuggestedAction::ConsiderRestart,
            severity: RecommendationSeverity::Critical,
            reason: "x".into(),
        };
        assert_eq!(label_template_for(&restart), display::CONSIDER_RESTART);
    }

    /// v1.2.0 / DISPATCH 45 — `render_label` substitutes `{pid}`,
    /// `{name}`, and `{targets}` into the template. Pinned shape
    /// per the contract's `reference_label_for_action_covers_all_variants`.
    #[test]
    fn render_label_substitutes_tokens() {
        let single = Recommendation {
            alert_id: AlertId::VramPressure,
            scope: RecommendationScope::Workload,
            targets: vec![RecommendedTarget {
                pid: 1234,
                name: "Llama-70B".into(),
                evidence: Some("vram_mb=11400".into()),
            }],
            action: SuggestedAction::ConsiderKill,
            severity: RecommendationSeverity::Warning,
            reason: "x".into(),
        };
        let s = render_label(&single);
        assert!(s.contains("PID 1234"), "got: {s}");
        assert!(s.contains("Llama-70B"), "got: {s}");

        let multi = Recommendation {
            targets: vec![
                RecommendedTarget {
                    pid: 1,
                    name: "a".into(),
                    evidence: None,
                },
                RecommendedTarget {
                    pid: 2,
                    name: "b".into(),
                    evidence: None,
                },
            ],
            ..single.clone()
        };
        let s = render_label(&multi);
        assert!(s.contains("a (PID 1)"));
        assert!(s.contains("b (PID 2)"));
    }
}
