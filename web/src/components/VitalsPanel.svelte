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

    // Match the TUI's cap so both surfaces show the same hottest
    // zones in the same order (operator glances at either and sees
    // the same signal). Kept in lockstep with
    // `TUI_TOP_THERMAL_ZONES` in `src/ui/panels/vitals.rs` — bump
    // both together. Original CAR-22 value was 3 and hid the 4th
    // zone on a common 4-zone x86 dev box; raised to 10 so every
    // realistic host (x86: 3-5, Jetson AGX: ~9) shows every zone,
    // with the "N of M zones shown" nag only appearing for exotic
    // hosts with more than 10 zones.
    const TOP_THERMAL_ZONES = 10;

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
                <!--
                    v1.3.2 / DISPATCH 109 — GPU temp+power inline
                    line beneath the VRAM bar. Same VRAM-honesty
                    rule: `temp_c` / `power_w` are OPTIONAL fields
                    on the wire (skip_serializing_if collapses
                    unmeasured `None` to absent). Renderer must
                    show `—` for absent, NEVER `0°C` / `0W`.
                    `data-testid-unmeasured` flags both branches
                    for the D98 gate.
                -->
                <div
                    class="flex justify-between text-xs mt-1 text-fg-muted"
                    data-testid="vitals-panel-gpu-line"
                >
                    <span>GPU</span>
                    <span>
                        {#if vitals.gpu.temp_c !== undefined && vitals.gpu.temp_c !== null}
                            <span data-testid="vitals-panel-gpu-temp"
                                >{vitals.gpu.temp_c.toFixed(0)}°C</span
                            >
                        {:else}
                            <span
                                data-testid="vitals-panel-gpu-temp"
                                data-testid-unmeasured="true"
                                title="No GPU temperature measurement this tick"
                                >—</span
                            >
                        {/if}
                        ·
                        {#if vitals.gpu.power_w !== undefined && vitals.gpu.power_w !== null}
                            <span data-testid="vitals-panel-gpu-power"
                                >{vitals.gpu.power_w.toFixed(0)}W</span
                            >
                        {:else}
                            <span
                                data-testid="vitals-panel-gpu-power"
                                data-testid-unmeasured="true"
                                title="No GPU power measurement this tick"
                                >—</span
                            >
                        {/if}
                    </span>
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
                    <!--
                        v1.3.2 / DISPATCH 65 — key on `${label}-${idx}`,
                        NOT label alone. Many Linux hosts expose multiple
                        zones with the same label (e.g. two `acpitz` rows
                        on this dev box; certain BIOS configs surface
                        per-package `x86_pkg_temp` rows under one label
                        too). Keying purely on `label` triggered the
                        Svelte `each_key_duplicate` error, which threw
                        inside the `<main>` slot and CASCADE-BLANKED the
                        sibling WorkloadsPanel + ActivityFeed (only the
                        out-of-`<main>` AlertsPanel survived — exactly
                        the operator-confirmed symptom). The composite
                        is positional so a re-key on list reorder is
                        cheap; a wire-level stable zone id would be
                        marginally better but requires a contract bump,
                        deliberately out of scope here.
                    -->
                    {#each thermalTop as zone, idx (`${zone.label}-${idx}`)}
                        <li class="flex justify-between {thermalColor(zone.severity)}" data-testid="thermal-row">
                            <span>
                                <span data-testid="thermal-friendly">{zone.friendly_label}</span>
                                <span class="font-mono text-fg-muted/70 text-[0.65rem] ml-1" data-testid="thermal-raw">({zone.label})</span>
                            </span>
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
