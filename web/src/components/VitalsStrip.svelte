<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 103 / PHASE 5 display modes step 4 —
     * TIMELINE mode's compact vitals bar.
     *
     * PHASE5_DISPLAY_MODES_DESIGN.md §3.1 chose a dedicated
     * VitalsStrip over a `compact` prop on VitalsPanel: "forcing
     * VitalsPanel into two shapes via a prop grows the biggest
     * presentational panel further; a dedicated strip stays
     * presentational and small." This component is the "strip"
     * (~80 LoC target).
     *
     * Shape: a single horizontal row of readouts. RAM % ·
     * VRAM % (or "—") · Thermal max °C · load-avg 1m. Colored per
     * severity but compact — this is CONTEXT for the timeline
     * event stream, not the hero. The dashboard's VitalsPanel is
     * the hero shape for that data.
     *
     * VRAM honesty inherited from the D95/D102 discriminator:
     * `vitals.gpu === null` → "—" WITH "unmeasured" hint, not "0%".
     * Same reason as kiosk — a compact "0%" reads "GPU idle,"
     * a lie when it's unmeasured.
     */
    import type { WireVitals, WireThermalZone } from '../lib/types';
    export let vitals: WireVitals;

    // ── severity classifiers ─────────────────────────────────────
    // Same threshold constants VitalsPanel + KioskView use
    // (mirrored from ux_contract::thresholds). Duplicated here to
    // keep VitalsStrip self-contained; if these ever start to
    // drift we'd lift them to lib/severity.ts. Two callsites is
    // NOT yet the "rule of three" cost that justifies the lift.
    function pctSeverity(pct: number): 'healthy' | 'attention' | 'critical' {
        if (pct >= 90) return 'critical';
        if (pct >= 80) return 'attention';
        return 'healthy';
    }
    function severityClass(
        s: 'healthy' | 'attention' | 'critical',
    ): string {
        switch (s) {
            case 'critical':
                return 'text-critical';
            case 'attention':
                return 'text-attention';
            case 'healthy':
            default:
                return 'text-fg';
        }
    }
    function thermalSeverityClass(
        z: WireThermalZone['severity'],
    ): string {
        switch (z) {
            case 'red':
                return 'text-critical';
            case 'amber':
                return 'text-attention';
            case 'nominal':
            default:
                return 'text-fg';
        }
    }

    $: ramSev = pctSeverity(vitals.memory_pct);

    $: gpuAvailable = vitals.gpu !== null && vitals.gpu.vram_total_mb > 0;
    $: vramSev = gpuAvailable
        ? pctSeverity(vitals.gpu!.vram_pct)
        : 'healthy';

    $: thermalZones = vitals.thermal_zones ?? [];
    $: thermalMax = thermalZones.reduce(
        (max, z) => Math.max(max, z.temp_celsius),
        Number.NEGATIVE_INFINITY,
    );
    $: thermalTopSeverity =
        thermalZones.find((z) => z.severity === 'red')?.severity ??
        thermalZones.find((z) => z.severity === 'amber')?.severity ??
        'nominal';
    $: hasThermal = thermalZones.length > 0;

    $: load1m = vitals.load_average?.[0] ?? 0;
</script>

<!--
    Strip layout: single row, tight spacing, mid-dot separators.
    The `data-testid` hooks let the D103 gate confirm the strip
    rendered without asserting exact text (which changes with the
    fixture). Read-only.
-->
<div
    class="vitals-strip flex flex-wrap items-baseline gap-x-4 gap-y-1 px-4 py-2 border-b border-fg-muted/20 bg-bg-raised/40 text-sm"
    data-testid="vitals-strip"
    role="status"
    aria-label="System vitals"
>
    <span class="text-fg-muted text-xs uppercase tracking-wider">Vitals</span>

    <span data-testid="vitals-strip-ram">
        <span class="text-fg-muted mr-1">RAM</span>
        <span class="{severityClass(ramSev)} font-bold tabular-nums">
            {vitals.memory_pct.toFixed(0)}%
        </span>
    </span>

    <span class="text-fg-muted">·</span>

    <span data-testid="vitals-strip-vram">
        <span class="text-fg-muted mr-1">VRAM</span>
        {#if gpuAvailable}
            <span
                class="{severityClass(vramSev)} font-bold tabular-nums"
                data-testid="vitals-strip-vram-value"
            >
                {vitals.gpu!.vram_pct.toFixed(0)}%
            </span>
        {:else}
            <!--
                VRAM honesty (§C5 across kiosk / chart / strip): a
                bare "0%" here would read as "GPU idle" — a lie when
                the driver is unloaded. Show "—" + `unmeasured` hint;
                same discriminator D95 + D102 established.
            -->
            <span
                class="text-fg-muted font-bold"
                data-testid="vitals-strip-vram-value"
                data-testid-unmeasured="true"
                title="No GPU measurement (driver unloaded or no NVIDIA hardware)"
            >
                —
            </span>
        {/if}
    </span>

    <span class="text-fg-muted">·</span>

    <span data-testid="vitals-strip-thermal">
        <span class="text-fg-muted mr-1">Thermal</span>
        {#if hasThermal}
            <span
                class="{thermalSeverityClass(thermalTopSeverity)} font-bold tabular-nums"
            >
                {thermalMax.toFixed(0)}°C
            </span>
        {:else}
            <span class="text-fg-muted">—</span>
        {/if}
    </span>

    <span class="text-fg-muted">·</span>

    <span data-testid="vitals-strip-load">
        <span class="text-fg-muted mr-1">Load 1m</span>
        <span class="text-fg tabular-nums">{load1m.toFixed(2)}</span>
    </span>
</div>
