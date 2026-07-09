<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 101 / PHASE 5 display modes step 2 —
     * HISTORY view.
     *
     * Mounts the D95 HistoryPage full-viewport (`alwaysOpen`) —
     * event timeline, dead-PID index, on-demand TrajectoryChart
     * drill-in with the VRAM-gap honesty invariant intact. The
     * component's internals are unchanged (D101 HARD RULE 1); this
     * wrapper only supplies the full-viewport container the mode
     * expects and a heading region so the header dropdown selection
     * has a matching page title.
     *
     * Snapshot-on-open (§Q5 / D95) is preserved: `HistoryPage` with
     * `alwaysOpen` fires exactly one fetch on mount and one per
     * Reload click. No 1 Hz poll on this mode.
     */
    import HistoryPage from '../components/HistoryPage.svelte';
</script>

<div
    class="history-view flex-1 px-6 py-4 overflow-y-auto"
    data-testid="history-view"
>
    <div class="max-w-6xl mx-auto">
        <h1 class="text-fg text-lg font-bold mb-1">History</h1>
        <p class="text-fg-muted text-xs mb-4">
            Event archive · dead-workload index · per-PID trajectory drill-in.
            Snapshot-on-open — reload to re-fetch.
        </p>
        <HistoryPage alwaysOpen />
    </div>
</div>
