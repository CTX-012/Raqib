<script lang="ts">
    import type { WireWorkload } from '../lib/types';
    import WorkloadRow from './WorkloadRow.svelte';
    export let workloads: WireWorkload[];

    // §1 region 4 grouping order — locked at the contract level.
    const ORDER: { key: string; label: string }[] = [
        { key: 'llm', label: 'LLM' },
        { key: 'vision', label: 'Vision' },
        { key: 'ros2', label: 'ROS2' },
        { key: 'embeddings', label: 'Embeddings' },
        { key: 'unknown', label: 'Unknown' },
    ];

    $: grouped = ORDER.map((cat) => ({
        ...cat,
        rows: workloads.filter((w) => w.workload_category === cat.key),
    })).filter((g) => g.rows.length > 0);
</script>

<div class="rounded border border-accent/40 p-4 bg-bg-raised/40">
    <h2 class="text-accent text-sm font-bold mb-3">AI Workloads</h2>

    {#if workloads.length === 0}
        <div class="text-fg-muted italic py-4">
            No AI workloads detected. Start one to begin monitoring.
        </div>
    {:else}
        {#each grouped as group}
            <div class="text-fg-muted text-xs uppercase mt-3 mb-1 tracking-wide">
                ── {group.label} ──
            </div>
            {#each group.rows as w (w.pid)}
                <WorkloadRow workload={w} />
            {/each}
        {/each}
    {/if}
</div>
