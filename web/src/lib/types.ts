// Sprint-6 — wire-protocol types mirroring src/web/wire.rs.
//
// Keep this file in sync with the Rust `WireSnapshot` family;
// breaking changes need the contract bump documented in
// `src/web/wire.rs`. The dashboard parses incoming WebSocket JSON
// frames against this shape with no validation step — a malformed
// payload from a mismatched binary will surface as a runtime error
// in the component layer.

export interface WireSnapshot {
    tick: number;
    server_time: string;
    mission: WireMission;
    vitals: WireVitals;
    workloads: WireWorkload[];
    activity: WireRunRecord[];
    // v1.1.13 / DISPATCH 42 — currently visible alerts (closes the
    // v1.1.11 web-wire deferral). Server pre-classifies severity
    // against `ux_contract::AlertId`'s tier mapping, so the
    // dashboard just maps the `severity` literal to a tailwind
    // class — no template substitution in TypeScript, the `text`
    // field is the same byte-for-byte rendering the TUI banner
    // shows. Optional for backward compat in case a pre-v1.1.13
    // binary is in the field (additive-default guarantee, same
    // shape as `thermal_zones?` and `activity_state?`).
    alerts?: WireAlertEntry[];
    // v1.2.0 / DISPATCH 45 — render-time recommendation projection.
    // Phase 3 capstone field; each entry derives from one of the
    // visible alerts above and carries a server-rendered label,
    // a snake-case action discriminator, and ranked targets.
    // Empty when no alerts project to recs (e.g. only
    // GovernorArmed / WorkloadExited visible — both suppressed by
    // the projection). Optional for backward compat with
    // pre-v1.2.0 server payloads.
    //
    // AUTHORITY LOCK: these are DISPLAY STRINGS the user reads.
    // The TS layer renders the `label` verbatim and maps the
    // severity literal to a tailwind class. There is no action
    // callback, no kill button — the user acts via the existing
    // manual TUI `k` keybinding flow (the disclaimer reminds
    // them: "Suggestion only — press k to act manually").
    recommendations?: WireRecommendation[];
}

export interface WireAlertEntry {
    // Snake-case identifier — 'vram_pressure' | 'ram_pressure' |
    // 'kv_pressure' | 'governor_armed' | 'oom_detected' |
    // 'workload_exited'. Mapped from ux_contract::AlertId at the
    // wire boundary. The dashboard pattern-matches on these
    // literals for any per-id styling beyond severity.
    alert_id: string;
    // PID the alert is scoped to. null for system-scope alerts
    // (currently only RamPressure).
    pid: number | null;
    // Workload display name. Empty string for system-scope alerts.
    workload_name: string;
    // Pre-classified severity. Mirrors the TUI's `AlertTier` →
    // tailwind class mapping: 'critical' → bg-critical / text-critical,
    // 'attention' → bg-attention / text-attention.
    severity: 'attention' | 'critical';
    // Fully-rendered banner text, e.g.
    // "VRAM at 92% — Llama-70B (PID 4523) — kill armed".
    // Rendered server-side via the SAME
    // `ux_contract::alerts::*` template + `substitute(...)` pipeline
    // the TUI uses; DO NOT do template substitution in TS.
    text: string;
}

export interface WireRecommendation {
    // Snake-case identifier of the underlying alert that drove
    // this rec (e.g. 'vram_pressure', 'thermal_pressure').
    alert_id: string;
    // 'workload' or 'system' — mirrors the contract's
    // RecommendationScope.
    scope: 'workload' | 'system';
    // Pre-classified severity. Renderer maps the literal to a
    // tailwind class; DO NOT introduce numeric thresholds here —
    // the contract is the single source of truth.
    severity: 'info' | 'warning' | 'critical';
    // Snake-case action discriminator. The Svelte dashboard
    // pattern-matches on this literal for any per-action styling
    // beyond severity. AUTHORITY LOCK: this is a string, NOT a
    // callable. There is no `executeAction()` in the TS layer.
    action: 'consider_kill' | 'consider_reduce_load' | 'consider_restart';
    // Ranked targets. Empty for system-scope recs without per-PID
    // attribution (thermal).
    targets: WireRecommendedTarget[];
    // Server-rendered label string. The Svelte renderer shows
    // this verbatim; NO template substitution in TS.
    label: string;
    // Producer-formatted rationale rendered as a one-line
    // sub-text under the label.
    reason: string;
}

export interface WireRecommendedTarget {
    pid: number;
    name: string;
    evidence: string | null;
}

export interface WireMission {
    workloads: number;
    degraded: number;
}

export interface WireVitals {
    memory_pct: number;
    memory_used_mb: number;
    memory_total_mb: number;
    load_average: [number, number, number];
    cpu_count: number;
    process_count: number;
    gpu: WireGpu | null;
    // v1.1.12 / CAR-22 — host-level thermal zones, pre-classified
    // server-side against ux_contract::thresholds::THERMAL_AMBER_C
    // (85 °C) and THERMAL_RED_C (95 °C). Empty when no zones were
    // discovered; the panel hides the section in that case. Optional
    // for backward compat: a pre-v1.1.12 server would not emit it.
    thermal_zones?: WireThermalZone[];
}

export interface WireThermalZone {
    // Canonical zone label, e.g. "x86_pkg_temp", "cpu-thermal".
    label: string;
    temp_celsius: number;
    // Pre-classified by the Rust wire layer against
    // ux_contract::thresholds::THERMAL_AMBER_C / THERMAL_RED_C.
    // Render the dot/row color by mapping these literal variants
    // to tailwind classes — DO NOT redo the threshold check in TS,
    // the contract is the single source of truth.
    severity: 'nominal' | 'amber' | 'red';
}

export interface WireGpu {
    vram_pct: number;
    vram_used_mb: number;
    vram_total_mb: number;
    device_count: number;
}

export interface WireWorkload {
    pid: number;
    name: string;
    model_name: string | null;
    category: string;
    workload_category: string;
    cpu_pct: number;
    rss_mb: number;
    ram_pct: number | null;
    vram_mb: number | null;
    tokens_per_sec: number | null;
    fps: number | null;
    kv_cache_peak_pct: number | null;
    status: WorkloadStatus;
    /**
     * Phase 2 / DISPATCH 1 — per-category activity state. `null` when
     * no Phase-2 sampler has surfaced a state for this PID yet, or
     * the workload's category has no Phase-2 sampler. Dashboard
     * hides the column when every visible row's `activity` is null,
     * mirroring the TUI's auto-hide rule (Inspector #8 V1).
     */
    activity: ActivityState | null;
}

export type WorkloadStatus = 'healthy' | 'attention' | 'critical' | 'loading';

export type ActivityState = 'active' | 'idle' | 'loading' | 'not_detected';

export interface WireRunRecord {
    pid: number;
    name: string;
    model_name: string | null;
    spawn_time: string;
    exit_time: string;
    uptime_secs: number;
    avg_cpu_pct: number;
    peak_cpu_pct: number;
    peak_rss_mb: number;
    peak_vram_mb: number;
    exit_kind: string;
    exit_detail: string | null;
}

/** Empty snapshot used as the initial store value. */
export const EMPTY_SNAPSHOT: WireSnapshot = {
    tick: 0,
    server_time: new Date(0).toISOString(),
    mission: { workloads: 0, degraded: 0 },
    vitals: {
        memory_pct: 0,
        memory_used_mb: 0,
        memory_total_mb: 0,
        load_average: [0, 0, 0],
        cpu_count: 0,
        process_count: 0,
        gpu: null,
    },
    workloads: [],
    activity: [],
};
