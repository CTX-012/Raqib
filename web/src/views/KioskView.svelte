<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 102 / PHASE 5 display modes step 3 —
     * KIOSK view.
     *
     * Glance-only wall monitor per PHASE5_DISPLAY_MODES_DESIGN.md
     * §1.2. Answers ONE operator question: "is anything on fire
     * right now?" — from across a room, no mouse expected.
     *
     * Design shape:
     *   * a single overall-severity headline (healthy / attention /
     *     critical) at the top — the biggest element on the page,
     *     colored per severity. This IS the traffic light.
     *   * three big-number tiles beneath — RAM % / VRAM % or "—" /
     *     Thermal max °C — with severity-colored values.
     *   * a small footer with alert count + degraded-workload count.
     *   * no clickable elements. Kiosk is read-only glance
     *     (§1.2 no-interaction).
     *
     * VRAM honesty at glance scale (D102 hard rule C5): when the
     * snapshot has no GPU (`vitals.gpu === null`), the VRAM tile
     * shows a giant "—" WITH the "unmeasured" label, NOT a giant
     * "0%". A "0%" on a wall reads "GPU idle" — a lie when the
     * driver is unloaded. Same discriminator D95 applies at chart
     * scale, now at kiosk scale.
     *
     * Data source: `/api/snapshot` at the existing 1 Hz cadence
     * (§C3 — no new endpoint, no contract). Kiosk is a different
     * RENDERING of the same live data the dashboard reads.
     * Live-updates automatically because it subscribes to
     * `$snapshot` (via `store` auto-subscribe).
     */
    import { snapshot } from '../lib/stores';
    import type { WireGpu, WireThermalZone } from '../lib/types';

    type OverallSeverity = 'healthy' | 'attention' | 'critical';

    // ── Severity aggregation ──────────────────────────────────
    //
    // The wire doesn't carry a pre-computed "system severity" (the
    // Rust side surfaces alerts + per-workload status separately).
    // Aggregate here at the view boundary: any critical anywhere
    // dominates; else any attention; else healthy. Mirrors the TUI
    // banner region's mental model.

    function aggregateSeverity(
        workloadCritical: boolean,
        workloadAttention: boolean,
        thermalRed: boolean,
        thermalAmber: boolean,
        alertCritical: boolean,
        alertAttention: boolean,
    ): OverallSeverity {
        if (workloadCritical || thermalRed || alertCritical) return 'critical';
        if (workloadAttention || thermalAmber || alertAttention) return 'attention';
        return 'healthy';
    }

    $: workloadCriticalCount = $snapshot.workloads.filter(
        (w) => w.status === 'critical',
    ).length;
    $: workloadAttentionCount = $snapshot.workloads.filter(
        (w) => w.status === 'attention',
    ).length;
    $: degradedCount = workloadCriticalCount + workloadAttentionCount;

    $: thermalZones = $snapshot.vitals.thermal_zones ?? [];
    $: thermalRedCount = thermalZones.filter((z) => z.severity === 'red').length;
    $: thermalAmberCount = thermalZones.filter(
        (z) => z.severity === 'amber',
    ).length;

    $: alerts = $snapshot.alerts ?? [];
    $: alertCriticalCount = alerts.filter((a) => a.severity === 'critical').length;
    $: alertAttentionCount = alerts.filter(
        (a) => a.severity === 'attention',
    ).length;

    $: overall = aggregateSeverity(
        workloadCriticalCount > 0,
        workloadAttentionCount > 0,
        thermalRedCount > 0,
        thermalAmberCount > 0,
        alertCriticalCount > 0,
        alertAttentionCount > 0,
    );

    // ── Per-tile values ───────────────────────────────────────

    $: ramPct = $snapshot.vitals.memory_pct;
    $: ramSeverity = ramPctSeverity(ramPct);

    // VRAM honesty: gpu === null ⇒ NO measurement (driver unloaded
    // or no NVIDIA hardware). Show "—", NOT "0%". The design's C5
    // hard rule at kiosk scale.
    $: vram = vramTile($snapshot.vitals.gpu);

    $: thermalMax = thermalZones.reduce(
        (max, z) => Math.max(max, z.temp_celsius),
        Number.NEGATIVE_INFINITY,
    );
    $: thermalSeverity = thermalRedCount > 0
        ? 'critical'
        : thermalAmberCount > 0
          ? 'attention'
          : 'healthy';
    $: hasThermal = thermalZones.length > 0;

    function ramPctSeverity(pct: number): OverallSeverity {
        // Contract-side thresholds live in ux_contract::thresholds
        // (RAM_ATTENTION_PCT = 80, RAM_CRITICAL_PCT = 90). Kiosk
        // mirrors them; if the operator retunes via /api/settings
        // the dashboard's severity dots follow, but kiosk stays
        // pinned to the ratified defaults — a wall monitor should
        // read from the contract, not a mutable tuning surface.
        if (pct >= 90) return 'critical';
        if (pct >= 80) return 'attention';
        return 'healthy';
    }

    function vramTile(gpu: WireGpu | null): {
        measured: boolean;
        pct: number;
        severity: OverallSeverity;
    } {
        if (gpu === null) {
            return { measured: false, pct: 0, severity: 'healthy' };
        }
        // Both used and total zero suggests a legacy "empty GPU
        // stub" pattern — treat as unmeasured too. A real GPU
        // reads > 0 total even when idle.
        if (gpu.vram_total_mb === 0) {
            return { measured: false, pct: 0, severity: 'healthy' };
        }
        const pct = gpu.vram_pct;
        // Mirrors ux_contract::thresholds VRAM 80/90 defaults.
        const severity: OverallSeverity =
            pct >= 90 ? 'critical' : pct >= 80 ? 'attention' : 'healthy';
        return { measured: true, pct, severity };
    }

    function severityClass(s: OverallSeverity): string {
        switch (s) {
            case 'critical':
                return 'text-critical';
            case 'attention':
                return 'text-attention';
            case 'healthy':
            default:
                return 'text-healthy';
        }
    }

    function severityLabel(s: OverallSeverity): string {
        switch (s) {
            case 'critical':
                return 'CRITICAL';
            case 'attention':
                return 'ATTENTION';
            case 'healthy':
            default:
                return 'HEALTHY';
        }
    }
</script>

<!--
    KIOSK view — no interaction. Every element below is a `<div>`
    or `<span>`; NO `<button>`, `<a href>`, or `<input>` inside the
    view (the app header still carries the mode dropdown, but that's
    the app frame, not the kiosk view). If a future edit adds a
    clickable child, the D98 gate's `no-interaction` pin fires.
-->
<div
    class="kiosk-view flex-1 flex flex-col items-center justify-center px-8 py-6 text-center"
    data-testid="kiosk-view"
    data-testid-severity={overall}
>
    <div class="kiosk-headline mb-2">
        <div
            class="kiosk-severity-label {severityClass(overall)}"
            data-testid="kiosk-severity"
        >
            SYSTEM: {severityLabel(overall)}
        </div>
        <div class="kiosk-mission text-fg-muted mt-2">
            {$snapshot.workloads.length} workloads · {degradedCount} degraded ·
            {alerts.length} alerts
        </div>
    </div>

    <div class="kiosk-tiles grid grid-cols-1 md:grid-cols-3 gap-6 w-full max-w-5xl mt-8">
        <!-- RAM tile -->
        <div class="kiosk-tile" data-testid="kiosk-tile-ram">
            <div class="kiosk-tile-label text-fg-muted">RAM</div>
            <div class="kiosk-tile-value {severityClass(ramSeverity)}">
                {ramPct.toFixed(0)}%
            </div>
            <div class="kiosk-tile-hint text-fg-muted">
                {$snapshot.vitals.memory_used_mb} /
                {$snapshot.vitals.memory_total_mb} MB
            </div>
        </div>

        <!-- VRAM tile — honesty discriminator at glance scale. -->
        <div class="kiosk-tile" data-testid="kiosk-tile-vram">
            <div class="kiosk-tile-label text-fg-muted">VRAM</div>
            {#if vram.measured}
                <div
                    class="kiosk-tile-value {severityClass(vram.severity)}"
                    data-testid="kiosk-vram-value"
                >
                    {vram.pct.toFixed(0)}%
                </div>
                <div class="kiosk-tile-hint text-fg-muted">
                    {$snapshot.vitals.gpu?.vram_used_mb} /
                    {$snapshot.vitals.gpu?.vram_total_mb} MB
                </div>
            {:else}
                <!--
                    VRAM UNMEASURED — the C5 discriminator. The giant
                    dash + "no measurement" label makes it visually
                    distinct from a real "0%" reading, which would
                    (dangerously) read as "GPU idle" from across a room.
                -->
                <div
                    class="kiosk-tile-value text-fg-muted"
                    data-testid="kiosk-vram-value"
                    data-testid-unmeasured="true"
                >
                    —
                </div>
                <div class="kiosk-tile-hint text-fg-muted">no measurement</div>
            {/if}
        </div>

        <!-- Thermal tile -->
        <div class="kiosk-tile" data-testid="kiosk-tile-thermal">
            <div class="kiosk-tile-label text-fg-muted">THERMAL</div>
            {#if hasThermal}
                <div class="kiosk-tile-value {severityClass(thermalSeverity)}">
                    {thermalMax.toFixed(0)}°C
                </div>
                <div class="kiosk-tile-hint text-fg-muted">
                    {thermalZones.length} zone{thermalZones.length === 1 ? '' : 's'}
                </div>
            {:else}
                <div class="kiosk-tile-value text-fg-muted">—</div>
                <div class="kiosk-tile-hint text-fg-muted">no zones</div>
            {/if}
        </div>
    </div>

    <div class="kiosk-footer text-fg-muted mt-10 text-sm">
        Kiosk · live · tick #{$snapshot.tick}
    </div>
</div>

<style>
    .kiosk-view {
        /* Full viewport height above the app footer. flex-1 in the
         * script inherits main-grid parent height. */
        min-height: 60vh;
    }
    .kiosk-severity-label {
        font-size: clamp(2.5rem, 8vw, 6rem);
        font-weight: 900;
        letter-spacing: 0.05em;
        line-height: 1;
    }
    .kiosk-mission {
        font-size: clamp(0.9rem, 1.8vw, 1.5rem);
    }
    .kiosk-tile {
        display: flex;
        flex-direction: column;
        align-items: center;
        padding: 1rem;
        border: 1px solid rgb(var(--em-muted) / 0.3);
        border-radius: 6px;
        background: rgb(var(--em-bg-raised) / 0.3);
    }
    .kiosk-tile-label {
        font-size: clamp(0.75rem, 1.5vw, 1.1rem);
        letter-spacing: 0.15em;
        text-transform: uppercase;
        margin-bottom: 0.25rem;
    }
    .kiosk-tile-value {
        font-size: clamp(2.5rem, 6vw, 5rem);
        font-weight: 900;
        line-height: 1.1;
        font-variant-numeric: tabular-nums;
    }
    .kiosk-tile-hint {
        font-size: clamp(0.7rem, 1.2vw, 1rem);
        margin-top: 0.25rem;
    }
</style>
