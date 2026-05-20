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

    $: primary = workload.tokens_per_sec != null
        ? `${workload.tokens_per_sec.toFixed(1)} tok/s`
        : workload.fps != null
        ? `${workload.fps.toFixed(1)} fps`
        : 'running actively';
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
        {workload.rss_mb}M{#if workload.vram_mb} · {workload.vram_mb}M GPU{/if}
    </span>
</div>
