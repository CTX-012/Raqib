//! Sprint-6 — wire-protocol types for the web companion.
//!
//! These structs are the serialization-friendly mirror of the runtime
//! state surface the web UI consumes. They live on a dedicated layer
//! so the in-memory `RuntimeState` can carry non-`Serialize` fields
//! (`Instant`, internal handles) without forcing serde derives on
//! deep internals.
//!
//! ## Wire schema v0.1 (LOCKED for Sprint-6)
//!
//! Future schema changes need contract consideration — the v2 / Altara
//! companion consumes this same JSON, so a breaking field rename
//! requires either an `v=…` versioned envelope or a coordinated
//! update across both consumers. New OPTIONAL fields are safe;
//! removed or renamed fields are not.
//!
//! ## Why a hand-written shim layer
//!
//! `RuntimeState` carries `Instant`, `Vec<ProcessSample>` (with raw
//! cmdlines + env vars), and per-tick scratch state. None of that
//! belongs on the wire. The shim types pick exactly the fields the
//! dashboard needs, no more, and convert internal types into
//! transport-friendly equivalents (e.g., `Instant` → "ms since first
//! observed" instead of an opaque handle).

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::Instant;

use crate::lifecycle::LifecycleSummary;
use crate::model::{AICategory, WorkloadCategory};
use crate::runtime::RuntimeState;
use crate::storage::RunRecord;
use crate::storage::run_store::ExitReason;

/// Top-level snapshot delivered on every WebSocket tick AND returned
/// by `GET /api/snapshot`. Sized for "smallish JSON" — ~5–10 KB at
/// typical workload counts. Bundles everything the dashboard needs
/// in one document so a single subscription drives the whole UI.
#[derive(Debug, Clone, Serialize)]
pub struct WireSnapshot {
    /// Monotonic tick counter. Increments by 1 per
    /// `runtime.tick_interval_ms`. Client uses this to detect skipped
    /// ticks (rare on localhost but defensible) and to anchor
    /// per-row animations to the tick cadence.
    pub tick: u64,
    /// Server-side wall clock at the moment this snapshot was
    /// composed. Surfaced to the dashboard's mission line so the
    /// browser doesn't have to trust its own clock for the timestamp
    /// shown to the operator.
    pub server_time: DateTime<Utc>,
    /// Mission-line counts.
    pub mission: WireMission,
    /// Vitals panel — host-level CPU/RAM/GPU readings.
    pub vitals: WireVitals,
    /// AI Workloads panel — one entry per AI-classified live PID,
    /// pre-grouped by workload category for the renderer's convenience.
    pub workloads: Vec<WireWorkload>,
    /// Activity feed — recent run exits / governor decisions
    /// (chronological, newest first). Capped at 50 to bound the
    /// JSON payload; the operator opens the full history view for
    /// older entries.
    pub activity: Vec<WireRunRecord>,
}

/// Mission-line counts shown at the top of the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct WireMission {
    pub workloads: usize,
    /// Subset of `workloads` whose live status is Attention or
    /// Critical. The dashboard uses this for the "N degraded" cue —
    /// matches the TUI's mission line exactly.
    pub degraded: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WireVitals {
    /// Memory percent (0..=100). Computed via
    /// `system.memory_usage_percent()` so the web matches the TUI's
    /// vitals bar number-for-number.
    pub memory_pct: f64,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
    pub load_average: [f64; 3],
    pub cpu_count: usize,
    pub process_count: usize,
    /// `Some(_)` when NVML reported at least one device, `None`
    /// otherwise. Mirrors the TUI's "No GPU detected" fallback
    /// without leaking the raw NVML state.
    pub gpu: Option<WireGpu>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WireGpu {
    pub vram_pct: f64,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub device_count: usize,
}

/// One AI workload row.
#[derive(Debug, Clone, Serialize)]
pub struct WireWorkload {
    pub pid: u32,
    pub name: String,
    /// Resolved model name when the classifier extracted one — e.g.
    /// `phi3-mini-q8_0` — otherwise `None`. The dashboard renders
    /// the resolved name when present, falling back to `name` so a
    /// just-spawned process without a fingerprint doesn't render as
    /// blank.
    pub model_name: Option<String>,
    /// `AICategory` as a wire-stable string. Using strings rather
    /// than the bare enum repr means the JSON survives an enum-variant
    /// reorder on the Rust side without bumping the wire schema.
    pub category: String,
    pub workload_category: String,
    pub cpu_pct: f32,
    pub rss_mb: u64,
    /// VRAM in MB. `None` when the process has no GPU allocation
    /// (vision/LLM workloads that haven't loaded yet, ROS2 nodes
    /// that don't touch GPU).
    pub vram_mb: Option<u64>,
    /// Live tokens/sec for LLM workloads sampled passively via vLLM /
    /// llama.cpp Prometheus. `None` for non-LLM categories OR LLM
    /// processes whose sampler hasn't reported a value yet.
    pub tokens_per_sec: Option<f32>,
    /// Live frames/sec for Vision workloads (B6 conditional).
    pub fps: Option<f32>,
    /// Live KV-cache occupancy percent for LLM workloads.
    pub kv_cache_peak_pct: Option<f32>,
    /// Status enum mirrored as a wire-stable string —
    /// `healthy`/`attention`/`critical`/`loading`. The dashboard
    /// renders the status dot color from this.
    pub status: String,
}

/// One completed-run record, suitable for both the history view and
/// the activity feed. Built from `RunRecord` (which is itself built
/// from `LifecycleSummary`) so every record represents an exited
/// workload — see B13 invariant in `tests/history_refactor.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct WireRunRecord {
    pub pid: u32,
    pub name: String,
    pub model_name: Option<String>,
    pub spawn_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub uptime_secs: u64,
    pub avg_cpu_pct: f32,
    pub peak_cpu_pct: f32,
    pub peak_rss_mb: u64,
    pub peak_vram_mb: u64,
    /// Human-readable exit kind — `clean` / `governor` / `oom` /
    /// `crash` / `cuda` / `segfault` / `signal` / `unknown`.
    /// Stays a string for schema stability; the variants follow
    /// `ExitReason` but the names are pinned here.
    pub exit_kind: String,
    /// Detail string when the exit kind carries one (signal number,
    /// exit code, CUDA error message). `None` for plain `clean`.
    pub exit_detail: Option<String>,
}

impl WireSnapshot {
    /// Compose a wire snapshot from the runtime's authoritative state
    /// plus a slice of recent completed RunRecords. The recent slice
    /// is the caller's choice — Sprint-6 main.rs hands in
    /// `runtime.recent_completed(50)` to keep JSON bounded.
    pub fn from_runtime_state(state: &RuntimeState, recent: &[RunRecord]) -> Self {
        let mission = WireMission::from_runtime(state);
        let vitals = WireVitals::from_runtime(state);
        let workloads = state
            .ai_processes()
            .map(|p| WireWorkload::from_annotated(p, state))
            .collect::<Vec<_>>();
        let activity = recent.iter().map(WireRunRecord::from_record).collect();
        Self {
            tick: state.tick_count,
            server_time: Utc::now(),
            mission,
            vitals,
            workloads,
            activity,
        }
    }

    /// Empty snapshot for the watch channel's initial value. Lets
    /// the first WS client get something coherent before the first
    /// tick runs through `from_runtime_state`.
    pub fn empty() -> Self {
        Self {
            tick: 0,
            server_time: Utc::now(),
            mission: WireMission {
                workloads: 0,
                degraded: 0,
            },
            vitals: WireVitals {
                memory_pct: 0.0,
                memory_used_mb: 0,
                memory_total_mb: 0,
                load_average: [0.0, 0.0, 0.0],
                cpu_count: 0,
                process_count: 0,
                gpu: None,
            },
            workloads: Vec::new(),
            activity: Vec::new(),
        }
    }
}

impl WireMission {
    fn from_runtime(state: &RuntimeState) -> Self {
        // We compute degraded by walking workloads twice (count vs
        // filter) rather than re-running `compute_workload_status`
        // here because the TUI uses `panels::workloads::ordered_rows`
        // which already produces the canonical status for each row.
        // For the web wire path we replicate the simpler check:
        // degraded = matches Attention/Critical.
        let workloads: Vec<&crate::runtime::AnnotatedProcess> = state.ai_processes().collect();
        let total = workloads.len();
        let degraded = workloads
            .iter()
            .filter(|p| {
                let inputs = build_status_inputs(p, state);
                let status = crate::runtime::compute_workload_status(&inputs);
                matches!(
                    status,
                    ux_contract::WorkloadStatus::Attention | ux_contract::WorkloadStatus::Critical
                )
            })
            .count();
        Self {
            workloads: total,
            degraded,
        }
    }
}

impl WireVitals {
    fn from_runtime(state: &RuntimeState) -> Self {
        let Some(snap) = state.last_snapshot.as_ref() else {
            return Self {
                memory_pct: 0.0,
                memory_used_mb: 0,
                memory_total_mb: 0,
                load_average: [0.0, 0.0, 0.0],
                cpu_count: 0,
                process_count: 0,
                gpu: None,
            };
        };
        let memory_total_mb = snap.system.total_memory / (1024 * 1024);
        let memory_used_mb = snap.system.used_memory / (1024 * 1024);
        let memory_pct = snap.system.memory_usage_percent();
        let gpu = if snap.gpu.has_gpu() {
            let total = snap.gpu.total_vram_all_devices();
            let used = snap.gpu.used_vram_all_devices();
            let pct = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            Some(WireGpu {
                vram_pct: pct,
                vram_used_mb: used / (1024 * 1024),
                vram_total_mb: total / (1024 * 1024),
                device_count: snap.gpu.devices.len(),
            })
        } else {
            None
        };
        Self {
            memory_pct,
            memory_used_mb,
            memory_total_mb,
            load_average: snap.system.load_average,
            cpu_count: snap.system.cpu_count,
            process_count: snap.processes.len(),
            gpu,
        }
    }
}

impl WireWorkload {
    fn from_annotated(p: &crate::runtime::AnnotatedProcess, state: &RuntimeState) -> Self {
        let vram_mb = p.vram_bytes.map(|b| b / (1024 * 1024)).filter(|&v| v > 0);
        let lt = state.live_telemetry.get(&p.pid);
        let tokens_per_sec = lt.and_then(|t| t.tokens_per_sec_avg);
        let fps = lt.and_then(|t| t.fps_avg);
        let kv_cache_peak_pct = lt.and_then(|t| t.kv_cache_peak_pct);
        let inputs = build_status_inputs(p, state);
        let status = crate::runtime::compute_workload_status(&inputs);
        Self {
            pid: p.pid,
            name: p.name.clone(),
            model_name: p.model_name.clone(),
            category: category_to_str(p.category).to_string(),
            workload_category: workload_category_to_str(p.workload_category).to_string(),
            cpu_pct: p.cpu_pct,
            rss_mb: p.rss_mb,
            vram_mb,
            tokens_per_sec,
            fps,
            kv_cache_peak_pct,
            status: workload_status_to_str(status).to_string(),
        }
    }
}

impl WireRunRecord {
    /// Build from a persisted `RunRecord`. The `exit_kind` /
    /// `exit_detail` projection collapses the rich `ExitReason` enum
    /// to a stable two-string pair so the wire schema doesn't track
    /// every variant the reason taxonomy might add.
    pub fn from_record(rec: &RunRecord) -> Self {
        let (kind, detail) = match &rec.exit_reason {
            ExitReason::CleanExit => ("clean", None),
            ExitReason::UserSignal { signal } => ("signal", Some(format!("signal {signal}"))),
            ExitReason::GovernorKill { reason } => ("governor", Some(reason.clone())),
            ExitReason::Segfault => ("segfault", None),
            ExitReason::OutOfMemory { ram, vram } => {
                let detail = match (ram, vram) {
                    (true, true) => "RAM and GPU memory",
                    (true, false) => "RAM",
                    (false, true) => "GPU memory",
                    (false, false) => "unknown",
                };
                ("oom", Some(detail.to_string()))
            }
            ExitReason::CudaError { last_msg } => {
                ("cuda", last_msg.clone())
            }
            ExitReason::Crash { exit_code } => ("crash", Some(format!("exit {exit_code}"))),
            ExitReason::Unknown => ("unknown", None),
        };
        Self::from_summary_with_exit(&rec.summary, kind.to_string(), detail)
    }

    fn from_summary_with_exit(
        summary: &LifecycleSummary,
        kind: String,
        detail: Option<String>,
    ) -> Self {
        Self {
            pid: summary.pid,
            name: summary.name.clone(),
            model_name: summary.model_name.clone(),
            spawn_time: summary.spawn_time,
            exit_time: summary.exit_time,
            uptime_secs: summary.uptime_secs.max(0) as u64,
            avg_cpu_pct: summary.avg_cpu_pct,
            peak_cpu_pct: summary.peak_cpu_pct,
            peak_rss_mb: summary.peak_rss_mb,
            peak_vram_mb: summary.peak_vram_mb,
            exit_kind: kind,
            exit_detail: detail,
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────

fn build_status_inputs(
    proc: &crate::runtime::AnnotatedProcess,
    state: &RuntimeState,
) -> crate::runtime::WorkloadStatusInputs {
    let total_vram = state
        .last_snapshot
        .as_ref()
        .map(|s| s.gpu.total_vram_all_devices())
        .filter(|&v| v > 0);
    let vram_pct = match (total_vram, proc.vram_bytes) {
        (Some(total), Some(used)) => Some((used as f64 / total as f64) * 100.0),
        _ => None,
    };
    let ram_pct = state
        .last_snapshot
        .as_ref()
        .map(|s| s.system.memory_usage_percent());
    let kv_cache_pct = state
        .live_telemetry
        .get(&proc.pid)
        .and_then(|lt| lt.kv_cache_peak_pct.map(|v| v as f64));
    let telemetry_age = Instant::now().saturating_duration_since(proc.first_observed_at);
    crate::runtime::WorkloadStatusInputs {
        vram_pct,
        ram_pct,
        kv_cache_pct,
        throughput_vs_baseline: None,
        governor_armed: false,
        oom_detected: false,
        telemetry_age,
    }
}

fn category_to_str(c: AICategory) -> &'static str {
    match c {
        AICategory::Inference => "inference",
        AICategory::Training => "training",
        AICategory::ModelDownload => "model_download",
        AICategory::Framework => "framework",
        AICategory::NotAi => "not_ai",
    }
}

fn workload_category_to_str(c: WorkloadCategory) -> &'static str {
    match c {
        WorkloadCategory::LLM => "llm",
        WorkloadCategory::Vision => "vision",
        WorkloadCategory::ROS2 => "ros2",
        WorkloadCategory::Embeddings => "embeddings",
        WorkloadCategory::Unknown => "unknown",
    }
}

fn workload_status_to_str(s: ux_contract::WorkloadStatus) -> &'static str {
    match s {
        ux_contract::WorkloadStatus::Healthy => "healthy",
        ux_contract::WorkloadStatus::Attention => "attention",
        ux_contract::WorkloadStatus::Critical => "critical",
        ux_contract::WorkloadStatus::Loading => "loading",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::Runtime;

    #[test]
    fn empty_snapshot_serializes_to_valid_json() {
        let snap = WireSnapshot::empty();
        let json = serde_json::to_string(&snap).expect("serialize");
        // Round-trip back via Value so we can poke at the shape.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["tick"].as_u64(), Some(0));
        assert_eq!(v["mission"]["workloads"].as_u64(), Some(0));
        assert_eq!(v["mission"]["degraded"].as_u64(), Some(0));
        assert!(v["workloads"].is_array());
        assert!(v["activity"].is_array());
        assert!(v["vitals"]["gpu"].is_null());
    }

    #[test]
    fn snapshot_from_default_runtime_has_zero_workloads() {
        let runtime = Runtime::new(Config::default());
        let snap = WireSnapshot::from_runtime_state(runtime.state(), &[]);
        assert_eq!(snap.mission.workloads, 0);
        assert_eq!(snap.mission.degraded, 0);
        assert!(snap.workloads.is_empty());
    }

    #[test]
    fn snapshot_serialization_contains_locked_top_level_keys() {
        // Wire-schema v0.1 invariant — top-level keys must not be
        // renamed without coordinating with the web client (and the
        // v2 / Altara consumer once that lands). This regression-
        // guard breaks loudly if anyone accidentally renames a key.
        let snap = WireSnapshot::empty();
        let v: serde_json::Value = serde_json::to_value(&snap).expect("serialize");
        for key in [
            "tick",
            "server_time",
            "mission",
            "vitals",
            "workloads",
            "activity",
        ] {
            assert!(
                v.get(key).is_some(),
                "wire schema v0.1 missing top-level key {key:?}: {v}"
            );
        }
    }

    #[test]
    fn workload_status_strings_cover_all_four_variants() {
        // The dashboard reads `status` as one of four strings to
        // pick a dot color. Pin the mapping so adding a contract
        // variant on the Rust side doesn't silently render as
        // "unknown" on the web side.
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Healthy),
            "healthy"
        );
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Attention),
            "attention"
        );
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Critical),
            "critical"
        );
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Loading),
            "loading"
        );
    }

    #[test]
    fn exit_kind_mapping_pins_wire_strings() {
        // Wire-stability check: the strings on the right are part of
        // the locked schema. Don't rename without bumping the
        // protocol version.
        use crate::lifecycle::LifecycleSummary;
        use chrono::Utc;
        fn lc(signal: Option<i32>, exit_code: Option<i32>) -> LifecycleSummary {
            LifecycleSummary {
                pid: 1,
                name: "x".into(),
                category: None,
                model_name: None,
                spawn_time: Utc::now(),
                exit_time: Utc::now(),
                uptime_secs: 1,
                exit_code,
                signal,
                avg_cpu_pct: 0.0,
                peak_cpu_pct: 0.0,
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                samples: 1,
            }
        }
        let clean = WireRunRecord::from_record(&RunRecord::from_summary(lc(None, Some(0))));
        assert_eq!(clean.exit_kind, "clean");
        let signal = WireRunRecord::from_record(&RunRecord::from_summary(lc(Some(15), None)));
        assert_eq!(signal.exit_kind, "signal");
        let segfault = WireRunRecord::from_record(&RunRecord::from_summary(lc(Some(11), None)));
        assert_eq!(segfault.exit_kind, "segfault");
        let crash = WireRunRecord::from_record(&RunRecord::from_summary(lc(None, Some(139))));
        assert_eq!(crash.exit_kind, "crash");
    }
}
