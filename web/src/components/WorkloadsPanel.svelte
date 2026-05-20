<script lang="ts">
    import type { WireWorkload } from '../lib/types';
    import WorkloadRow from './WorkloadRow.svelte';
    export let workloads: WireWorkload[];

    // §1 region 4 grouping order — locked at the contract level.
    // Sprint-7.5 / CAR-18 — Agent sits between LLM and Vision to
    // mirror the Rust-side `WorkloadCategory::display_order`. The
    // Agent label / heading text matches `ux_contract::workload_
    // category::GROUP_HEADER_AGENT` minus the `── ── ` framing
    // (the panel renders the ── ── separators itself below).
    const ORDER: { key: string; label: string }[] = [
        { key: 'llm', label: 'LLM' },
        { key: 'agent', label: 'Agent' },
        { key: 'vision', label: 'Vision' },
        { key: 'ros2', label: 'ROS2' },
        { key: 'embeddings', label: 'Embeddings' },
        { key: 'unknown', label: 'Unknown' },
    ];

    // Sprint-7.5 Fix 3 — `.filter((g) => g.rows.length > 0)` is the
    // collapse rule. An empty Vision / ROS / Agent / Embeddings
    // subsection renders nothing — neither a header line nor a
    // placeholder — so the panel never wastes vertical space on
    // categories the operator doesn't currently have workloads in.
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
