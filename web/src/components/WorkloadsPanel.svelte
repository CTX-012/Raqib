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
        <!--
            DISPATCH web-column-headers — one shared grid across the
            header + every group divider + every WorkloadRow. Each
            WorkloadRow uses `display: contents` (Tailwind `contents`)
            so its 8 <span>s become direct children of THIS grid,
            forcing header labels + data cells to share the SAME
            column tracks. Auto-widths resolve once across all rows,
            so a wide value in one row widens the column for all
            (including the header) — pixel alignment, not approximate.
        -->
        <div class="grid grid-cols-[auto_1fr_auto_auto_auto_auto_auto_auto] gap-x-3 items-baseline text-sm">
            <!--
                Header row. Labels sit above the columns they
                describe — NAME / PRIMARY / STATE / CPU % / RSS MB /
                VRAM — matching the TUI's `column_header_line` at
                src/ui/panels/workloads.rs:98 (D107 FIX 2). Cells 1
                (status dot) and 8 (connectivity chip) get no label
                per TUI convention (dot columns are visual, not
                textual). `data-testid="workloads-header"` on the
                first labeled cell so the gate can find it; the
                aria-hidden empties keep the grid alignment stable.
            -->
            <!--
                The header cells live inside a `display: contents`
                wrapper so they're grid children of the shared grid
                above, but the wrapper itself carries the
                `data-testid="workloads-header"` handle — one single
                element the browser gate can find to prove the
                header row exists. Every cell shares the
                header-styling classes (muted, uppercase, tracking,
                border-b for the visual separator).
            -->
            <div data-testid="workloads-header" class="contents">
                <span aria-hidden="true" class="border-b border-fg-muted/20 pb-1"></span>
                <span data-testid="workloads-header-name" class="text-fg-muted text-[0.65rem] uppercase tracking-wide border-b border-fg-muted/20 pb-1">NAME</span>
                <span data-testid="workloads-header-primary" class="text-fg-muted text-[0.65rem] uppercase tracking-wide border-b border-fg-muted/20 pb-1">PRIMARY</span>
                <span data-testid="workloads-header-state" class="text-fg-muted text-[0.65rem] uppercase tracking-wide border-b border-fg-muted/20 pb-1">STATE</span>
                <span data-testid="workloads-header-cpu" class="text-fg-muted text-[0.65rem] uppercase tracking-wide border-b border-fg-muted/20 pb-1">CPU %</span>
                <span data-testid="workloads-header-rss" class="text-fg-muted text-[0.65rem] uppercase tracking-wide border-b border-fg-muted/20 pb-1">RSS MB</span>
                <span data-testid="workloads-header-vram" class="text-fg-muted text-[0.65rem] uppercase tracking-wide border-b border-fg-muted/20 pb-1">VRAM</span>
                <span aria-hidden="true" class="border-b border-fg-muted/20 pb-1"></span>
            </div>

            {#each grouped as group}
                <!-- Group divider spans all 8 columns via col-start/end. -->
                <div class="col-start-1 col-end-[-1] text-fg-muted text-xs uppercase mt-3 mb-1 tracking-wide">
                    ── {group.label} ──
                </div>
                {#each group.rows as w (w.pid)}
                    <WorkloadRow workload={w} />
                {/each}
            {/each}
        </div>
    {/if}
</div>
