<script lang="ts">
    import type { WireWorkload, WorkloadStatus } from '../lib/types';
    export let workload: WireWorkload;

    const STATUS_GLYPH: Record<WorkloadStatus, string> = {
        healthy: '●',
        attention: '⚠',
        critical: '✕',
        loading: '○',
    };
    const STATUS_CLASS: Record<WorkloadStatus, string> = {
        healthy: 'text-healthy',
        attention: 'text-attention',
        critical: 'text-critical',
        loading: 'text-fg-muted',
    };

    // v1.0.1 B-NEW-6 + B-NEW-4 — branch on workload_category so an
    // Agent row with no metric reads "alive" (honest minimum signal:
    // the process exists; no activity claim), while LLM keeps
    // tokens/sec → KV → "running actively", and Vision keeps fps.
    // Pre-v1.0.1 the fallback was always "running actively" — every
    // Agent claude-code row claimed activity that wasn't measured.
    $: primary = (() => {
        if (workload.tokens_per_sec != null) {
            return `${workload.tokens_per_sec.toFixed(1)} tok/s`;
        }
        if (workload.fps != null) {
            return `${workload.fps.toFixed(1)} fps`;
        }
        if (workload.workload_category === 'agent') {
            return 'alive';
        }
        return 'running actively';
    })();
</script>

<div class="grid grid-cols-[auto_1fr_auto_auto_auto] gap-x-3 py-0.5 items-baseline text-sm">
    <span class={STATUS_CLASS[workload.status]} aria-label={workload.status}>
        {STATUS_GLYPH[workload.status]}
    </span>
    <span class="text-fg truncate">
        {workload.model_name ?? workload.name}
    </span>
    <span class="text-fg-muted text-xs">{primary}</span>
    <span class="text-fg-muted text-xs">{workload.cpu_pct.toFixed(1)}% CPU</span>
    <span class="text-fg-muted text-xs tabular-nums">
        {workload.rss_mb}M{#if workload.ram_pct != null} ({workload.ram_pct.toFixed(1)}%){/if}{#if workload.vram_mb} · {workload.vram_mb}M GPU{/if}
    </span>
</div>
