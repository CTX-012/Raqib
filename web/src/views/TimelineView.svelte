<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 103 / PHASE 5 display modes step 4 —
     * TIMELINE view. Chronological incident-review mode.
     *
     * PHASE5_DISPLAY_MODES_DESIGN.md §1.2 shape (paraphrased):
     * "alerts + activity feed dominate the viewport (~2/3 of
     * screen). Vitals shrinks to a single-row strip along the
     * top. Workloads shrink to a compact side rail. Chronology-
     * first: events sorted newest-first."
     *
     * INTERACTION-FIRST (§1.2, and the D103 hard rule C5):
     * distinct from kiosk's glance-only. This view PRESERVES
     * everything AlertsPanel + ActivityFeed already do —
     * including D74's click-to-expand post-mortem on
     * exit/kill entries. `<AlertsPanel>` and `<ActivityFeed>`
     * are mounted verbatim; the wrapper JUST arranges them.
     *
     * SEPARATE SECTIONS, not one merged stream (design STOP #1):
     * alerts don't carry timestamps on the wire (WireAlertEntry
     * has no `timestamp` field — see lib/types.ts:55-77), so a
     * merged time-ordered stream isn't possible from the current
     * data. Two stacked chronological sections is the honest
     * shape — Alerts (state-of-now, severity-ordered by AlertsPanel)
     * above Activity (event log, time-ordered by ActivityFeed). If
     * a future contract amendment adds `WireAlertEntry.timestamp`
     * we can merge; today we render honestly separate.
     *
     * SAME DATA, live 1 Hz (§C4): reads `$snapshot`, the same store
     * the dashboard uses. No new endpoint, no contract.
     */
    import { snapshot } from '../lib/stores';
    import AlertsPanel from '../components/AlertsPanel.svelte';
    import ActivityFeed from '../components/ActivityFeed.svelte';
    import WorkloadRow from '../components/WorkloadRow.svelte';
    import VitalsStrip from '../components/VitalsStrip.svelte';
</script>

<div
    class="timeline-view flex-1 flex flex-col overflow-hidden"
    data-testid="timeline-view"
>
    <!-- Compact vitals context along the top (§1.2 sketch). -->
    <VitalsStrip vitals={$snapshot.vitals} />

    <div
        class="timeline-body flex-1 grid grid-cols-1 lg:grid-cols-[minmax(0,3fr)_minmax(0,1fr)] gap-4 px-6 py-4 overflow-y-auto"
    >
        <!-- Chronology-first main column: alerts (state) + activity
             (event log). Alerts render above because they're
             actionable NOW; activity is the historical log below. -->
        <section
            class="timeline-main space-y-4 min-w-0"
            data-testid="timeline-main"
        >
            <div>
                <h2 class="text-fg text-sm font-bold mb-2">
                    Alerts
                    <span class="text-xs font-normal text-fg-muted">
                        ({($snapshot.alerts ?? []).length})
                    </span>
                </h2>
                <AlertsPanel
                    alerts={$snapshot.alerts}
                    recommendations={$snapshot.recommendations}
                />
            </div>
            <div>
                <!--
                    ActivityFeed carries its own <h2>Activity</h2>
                    heading + severity coloring + D74 click-to-expand.
                    Timeline mounts it verbatim to preserve every
                    interaction the operator expects.
                -->
                <ActivityFeed activity={$snapshot.activity} />
            </div>
        </section>

        <!--
            Workloads side rail — compact. Uses WorkloadRow (not
            WorkloadsPanel) per §3 reuse map, so the timeline picks
            up per-row rendering without the panel chrome (heading,
            grouping). Each row keys on w.pid — the same composite
            WorkloadsPanel uses, so the each_key discipline follows.
        -->
        <aside
            class="timeline-workloads min-w-0"
            data-testid="timeline-workloads"
        >
            <h2 class="text-fg text-sm font-bold mb-2">
                Workloads
                <span class="text-xs font-normal text-fg-muted">
                    ({$snapshot.workloads.length})
                </span>
            </h2>
            <div
                class="rounded border border-fg-muted/30 p-3 bg-bg-raised/40 space-y-1"
            >
                {#if $snapshot.workloads.length === 0}
                    <div class="text-fg-muted italic text-xs">
                        No workloads.
                    </div>
                {:else}
                    {#each $snapshot.workloads as w (w.pid)}
                        <WorkloadRow workload={w} />
                    {/each}
                {/if}
            </div>
        </aside>
    </div>
</div>
