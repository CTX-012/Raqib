<script lang="ts">
    import type { WireVitals } from '../lib/types';
    export let vitals: WireVitals;

    /** Tailwind class for a bar at the given percentage per §14
     * thresholds (85% → attention, 95% → critical). */
    function barColor(pct: number): string {
        if (pct >= 95) return 'bg-critical';
        if (pct >= 85) return 'bg-attention';
        return 'bg-accent';
    }
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

        <div class="text-xs text-fg-muted grid grid-cols-2 gap-2 pt-2">
            <div>load avg: {vitals.load_average.map((n) => n.toFixed(2)).join(' ')}</div>
            <div>cpus: {vitals.cpu_count}</div>
            <div>processes: {vitals.process_count}</div>
        </div>
    </div>
</div>
