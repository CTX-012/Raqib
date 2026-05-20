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
    vram_mb: number | null;
    tokens_per_sec: number | null;
    fps: number | null;
    kv_cache_peak_pct: number | null;
    status: WorkloadStatus;
}

export type WorkloadStatus = 'healthy' | 'attention' | 'critical' | 'loading';

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
