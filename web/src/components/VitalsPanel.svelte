<script lang="ts">
    import type { WireVitals, WireThermalZone } from '../lib/types';
    export let vitals: WireVitals;

    /** Tailwind class for a bar at the given percentage per §14
     * thresholds (85% → attention, 95% → critical). */
    function barColor(pct: number): string {
        if (pct >= 95) return 'bg-critical';
        if (pct >= 85) return 'bg-attention';
        return 'bg-accent';
    }

    /** v1.1.12 / CAR-22 — map the server-side classified severity
     * to a tailwind text color. The thresholds (85 °C amber / 95 °C
     * red) live in `ux_contract::thresholds` and are applied
     * server-side in `src/web/wire.rs::classify_thermal`; this
     * function ONLY translates the variant to a render hint. DO
     * NOT redo the >= 85 check here — the contract is the single
     * source of truth, and adding TS-side numeric thresholds would
     * be the v0.x drift mode the lift to ux_contract was supposed
     * to prevent. */
    function thermalColor(severity: WireThermalZone['severity']): string {
        switch (severity) {
            case 'red':
                return 'text-critical';
            case 'amber':
                return 'text-attention';
            case 'nominal':
            default:
                return 'text-fg';
        }
    }

    // v1.1.12 / CAR-22 — match the TUI's top-3 + count behaviour so
    // both surfaces show the same hottest zones in the same order
    // (the operator can glance at either and see the same signal).
    // Picking the same K=3 instead of "show all on web because more
    // screen space" because the operator decision in DISPATCH 39
    // leaned toward consistent presentation.
    const TOP_THERMAL_ZONES = 3;

    $: thermalAll = vitals.thermal_zones ?? [];
    $: thermalSorted = [...thermalAll].sort(
        (a, b) => b.temp_celsius - a.temp_celsius,
    );
    $: thermalTop = thermalSorted.slice(0, TOP_THERMAL_ZONES);
    $: thermalHasMore = thermalAll.length > TOP_THERMAL_ZONES;
</script>

<div class="rounded border border-fg-muted/30 p-4 bg-bg-raised/40">
    <h2 class="text-fg-muted text-sm font-bold mb-3">System</h2>

    <div class="space-y-3">
        <div>
            <div class="flex justify-between text-xs mb-1">
                <span>RAM</span>
                <span>{vitals.memory_used_mb} / {vitals.memory_total_mb} MB</span>
            </div>
            <div class="h-2 bg-fg-muted/20 rounded overflow-hidden">
                <div
                    class="h-full {barColor(vitals.memory_pct)}"
                    style="width: {Math.min(vitals.memory_pct, 100)}%"
                ></div>
            </div>
        </div>

        {#if vitals.gpu}
            <div>
                <div class="flex justify-between text-xs mb-1">
                    <span>VRAM</span>
                    <span>{vitals.gpu.vram_used_mb} / {vitals.gpu.vram_total_mb} MB
                        · {vitals.gpu.device_count} device{vitals.gpu.device_count === 1 ? '' : 's'}</span>
                </div>
                <div class="h-2 bg-fg-muted/20 rounded overflow-hidden">
                    <div
                        class="h-full {barColor(vitals.gpu.vram_pct)}"
                        style="width: {Math.min(vitals.gpu.vram_pct, 100)}%"
                    ></div>
                </div>
            </div>
        {:else}
            <div class="text-xs text-fg-muted italic">No GPU detected</div>
        {/if}

        <!--
            v1.1.12 / CAR-22 — thermal zones. Hidden when no zones
            were discovered (contract semantic: empty thermal_zones
            means "no sensors on this host"). Pre-classified
            server-side; this template only maps the severity
            variant to a tailwind color and renders the values.
        -->
        {#if thermalAll.length > 0}
            <div class="pt-2 border-t border-fg-muted/20">
                <div class="text-xs text-fg-muted mb-1">Thermal</div>
                <ul class="text-xs space-y-0.5">
                    {#each thermalTop as zone (zone.label)}
                        <li class="flex justify-between {thermalColor(zone.severity)}">
                            <span class="font-mono">{zone.label}</span>
                            <span>{zone.temp_celsius.toFixed(1)} °C</span>
                        </li>
                    {/each}
                </ul>
                {#if thermalHasMore}
                    <div class="text-xs text-fg-muted italic mt-1">
                        {thermalTop.length} of {thermalAll.length} zones shown
                    </div>
                {/if}
            </div>
        {/if}

        <div class="text-xs text-fg-muted grid grid-cols-2 gap-2 pt-2">
            <div>load avg: {vitals.load_average.map((n) => n.toFixed(2)).join(' ')}</div>
            <div>cpus: {vitals.cpu_count}</div>
            <div>processes: {vitals.process_count}</div>
        </div>
    </div>
</div>
