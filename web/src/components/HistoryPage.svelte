<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 95 / PHASE 5 step 7 — history view.
     *
     * SNAPSHOT-ON-OPEN (dispatch C1, PHASE 5 Q5). Fetches
     * /api/history when the panel opens; NOT a 1Hz poll. The event
     * archive + dead-PID index freeze at the fetch instant; a
     * "Reload" button re-fetches. The D76 lesson: a live-streamed
     * archive drops the operator's currently-selected PID under
     * them when a new tick shifts the list, and a mid-inspection
     * chart re-render loses context. Snapshot-on-open is the honest
     * shape for a "look at recent history" view.
     *
     * {#each} keys (dispatch C2, C3): both lists key on a UNIQUE
     * composite. Events use `${kind}-${pid}-${timestamp}` (mirrors
     * ActivityFeed's D65/D71 fix — a PID can appear in both an exit
     * and a kill row in the same archive, so bare pid would collide).
     * Dead PIDs key on `${pid}-${exit_time}` (a PID can be reused
     * across the ring window; the timestamp disambiguates).
     */

    import {
        fetchHistorySnapshot,
        fetchHistoryTrajectory,
    } from '../lib/rest';
    import type {
        WireHistorySnapshot,
        WireHistoryEvent,
        WireDeadPidEntry,
        WireTrajectory,
    } from '../lib/types';
    import TrajectoryChart from './TrajectoryChart.svelte';

    let expanded = false;

    // Snapshot state — set once per fetch (open or Reload).
    let snapshot: WireHistorySnapshot | null = null;
    let loadError: string | null = null;
    let loading = false;
    let loadedAt: string | null = null;

    // Trajectory state — an on-demand drill-in. `selectedKey`
    // matches the dead-PID {#each} key so the panel highlights
    // the row the operator clicked.
    let selectedKey: string | null = null;
    let selectedEntry: WireDeadPidEntry | null = null;
    let trajectory: WireTrajectory | null = null;
    let trajectoryError: string | null = null;
    let trajectoryLoading = false;

    async function refresh(): Promise<void> {
        if (loading) return;
        loading = true;
        loadError = null;
        try {
            snapshot = await fetchHistorySnapshot();
            loadedAt = new Date().toLocaleTimeString([], {
                hour: '2-digit',
                minute: '2-digit',
                second: '2-digit',
            });
            // Snapshot changed — the selected PID may no longer
            // be in the index; drop the selection so the operator
            // is never staring at a chart whose header row is
            // gone. (Selection-stability across a REFRESH is
            // intentionally not preserved — Reload is an explicit
            // "give me fresh data" action.)
            selectedKey = null;
            selectedEntry = null;
            trajectory = null;
            trajectoryError = null;
        } catch (err) {
            loadError = (err as Error).message ?? String(err);
        } finally {
            loading = false;
        }
    }

    /**
     * Toggle the panel expanded state. First-open triggers the
     * snapshot fetch (Q5); subsequent opens do NOT re-fetch — the
     * data was frozen on the first open, and the operator gets
     * fresh via the Reload button. This matches the "look at
     * recent history" mental model: opening is inspecting a
     * moment, not resuming a live stream.
     */
    async function toggle(): Promise<void> {
        expanded = !expanded;
        if (expanded && snapshot === null && !loading) {
            await refresh();
        }
    }

    function eventKey(ev: WireHistoryEvent): string {
        // D65/D71 composite — kind AND pid AND timestamp. A PID
        // can appear in both an exit AND a kill row in the same
        // archive; kind alone or pid alone would each_key_duplicate.
        return `${ev.kind}-${ev.pid}-${ev.timestamp}`;
    }

    function deadKey(entry: WireDeadPidEntry): string {
        // PID reuse across the ring window is rare on Linux but
        // theoretically possible; the exit_time timestamp
        // disambiguates. Bare pid would each_key_duplicate on
        // a reused PID.
        return `${entry.pid}-${entry.exit_time}`;
    }

    function severityForKind(kind: WireHistoryEvent['kind']): string {
        // Mirror the ActivityFeed convention: kills / regressions
        // read as attention, exits are neutral. The archive doesn't
        // carry a per-event severity (D91 kept it simple — server
        // did not synthesize one) so we map from kind.
        switch (kind) {
            case 'kill':
                return 'text-critical';
            case 'regression':
                return 'text-attention';
            case 'exit':
            default:
                return 'text-fg-muted';
        }
    }

    function shortTime(iso: string): string {
        try {
            const d = new Date(iso);
            return d.toLocaleTimeString([], {
                hour: '2-digit',
                minute: '2-digit',
                second: '2-digit',
            });
        } catch {
            return '--:--:--';
        }
    }

    async function selectDeadPid(entry: WireDeadPidEntry): Promise<void> {
        const k = deadKey(entry);
        if (selectedKey === k) {
            // Second click on the same row collapses the drill-in.
            selectedKey = null;
            selectedEntry = null;
            trajectory = null;
            trajectoryError = null;
            return;
        }
        selectedKey = k;
        selectedEntry = entry;
        trajectory = null;
        trajectoryError = null;
        trajectoryLoading = true;
        try {
            const t = await fetchHistoryTrajectory(entry.pid);
            // `fetchHistoryTrajectory` returns null on 404 — the
            // PID rolled out of the window between the snapshot
            // fetch and the drill-in click (rare but real). The
            // renderer shows a "no trajectory" line instead of
            // an error banner because it's not the operator's
            // problem to solve.
            trajectory = t;
        } catch (err) {
            trajectoryError = (err as Error).message ?? String(err);
        } finally {
            trajectoryLoading = false;
        }
    }
</script>

<section class="history-panel" aria-label="History">
    <button
        type="button"
        class="history-toggle"
        on:click={toggle}
        aria-expanded={expanded}
    >
        History {expanded ? '▾' : '▸'}
    </button>

    {#if expanded}
        <div class="history-body">
            <div class="controls">
                <button
                    type="button"
                    class="reload-btn"
                    on:click={refresh}
                    disabled={loading}
                    title="Snapshot-on-open — click to re-fetch"
                >
                    {loading ? 'Loading…' : 'Reload'}
                </button>
                {#if loadedAt}
                    <span class="text-xs text-fg-muted">
                        snapshot @ {loadedAt} · not live
                    </span>
                {/if}
            </div>

            {#if loadError}
                <div class="error">Failed to load history: {loadError}</div>
            {/if}

            {#if snapshot}
                <div class="grid grid-cols-1 lg:grid-cols-2 gap-4 mt-3">
                    <!-- ── Event timeline ─────────────────────────
                         Newest-first (server already sorts DESC in
                         the archive). Kind → color via
                         `severityForKind`; the summary prints
                         verbatim (server-rendered).
                    -->
                    <div>
                        <h3 class="text-fg-muted text-sm font-bold mb-2">
                            Event archive
                            <span class="text-xs font-normal">
                                ({snapshot.events.length})
                            </span>
                        </h3>
                        {#if snapshot.events.length === 0}
                            <div class="text-fg-muted italic text-sm">
                                No events in the archive.
                            </div>
                        {:else}
                            <ul class="events">
                                {#each snapshot.events as ev (eventKey(ev))}
                                    <li class="event-row">
                                        <span class="text-fg-muted text-xs">
                                            {shortTime(ev.timestamp)}
                                        </span>
                                        <span
                                            class="text-xs font-bold {severityForKind(ev.kind)}"
                                        >
                                            {ev.kind}
                                        </span>
                                        <span
                                            class="text-fg truncate"
                                            title={ev.summary}
                                        >
                                            {ev.summary}
                                        </span>
                                    </li>
                                {/each}
                            </ul>
                        {/if}
                    </div>

                    <!-- ── Dead-PID index ─────────────────────────
                         The click surface. Each row → trajectory
                         drill-in via `selectDeadPid`. Key is
                         `${pid}-${exit_time}` — PID reuse across the
                         ring is theoretically possible; the wire's
                         exit_time disambiguates.
                    -->
                    <div>
                        <h3 class="text-fg-muted text-sm font-bold mb-2">
                            Dead workloads
                            <span class="text-xs font-normal">
                                ({snapshot.dead_pids.length})
                            </span>
                        </h3>
                        {#if snapshot.dead_pids.length === 0}
                            <div class="text-fg-muted italic text-sm">
                                No dead workloads in the window.
                            </div>
                        {:else}
                            <ul class="dead-pids">
                                {#each snapshot.dead_pids as entry (deadKey(entry))}
                                    {@const key = deadKey(entry)}
                                    {@const isSelected = selectedKey === key}
                                    <li>
                                        <button
                                            type="button"
                                            class="dead-row"
                                            class:selected={isSelected}
                                            on:click={() => selectDeadPid(entry)}
                                            aria-expanded={isSelected}
                                        >
                                            <span class="text-fg-muted text-xs">
                                                {shortTime(entry.exit_time)}
                                            </span>
                                            <span class="text-fg">
                                                {entry.name}
                                            </span>
                                            <span class="text-fg-muted text-xs">
                                                pid {entry.pid}
                                            </span>
                                            {#if entry.model_name}
                                                <span class="text-accent text-xs">
                                                    {entry.model_name}
                                                </span>
                                            {/if}
                                        </button>
                                    </li>
                                {/each}
                            </ul>
                        {/if}
                    </div>
                </div>

                <!-- ── Trajectory drill-in ────────────────────────
                     Rendered outside the two-column layout so it can
                     span the full width when open. Trajectory fetch
                     is on-demand (only when a dead PID is clicked).
                -->
                {#if selectedEntry}
                    <div class="trajectory-block mt-4">
                        <div class="text-sm mb-2">
                            <span class="text-fg-muted">Trajectory for</span>
                            <span class="text-fg font-bold">{selectedEntry.name}</span>
                            <span class="text-fg-muted">
                                (pid {selectedEntry.pid})
                            </span>
                            {#if selectedEntry.model_name}
                                <span class="text-accent">
                                    · {selectedEntry.model_name}
                                </span>
                            {/if}
                            <button
                                type="button"
                                class="close-traj"
                                on:click={() => selectDeadPid(selectedEntry)}
                                aria-label="Close trajectory"
                            >
                                ×
                            </button>
                        </div>
                        {#if trajectoryLoading}
                            <div class="text-fg-muted italic text-sm">Loading trajectory…</div>
                        {:else if trajectoryError}
                            <div class="error">Failed to load: {trajectoryError}</div>
                        {:else if trajectory === null}
                            <div class="text-fg-muted italic text-sm">
                                No trajectory captured for this workload
                                (or it rolled out of the window).
                            </div>
                        {:else}
                            <TrajectoryChart {trajectory} />
                        {/if}
                    </div>
                {/if}
            {/if}
        </div>
    {/if}
</section>

<style>
    .history-panel {
        margin: 0.5rem 0;
        font-size: 0.875rem;
    }
    .history-toggle {
        background: transparent;
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        padding: 0.2rem 0.6rem;
        color: rgb(var(--em-fg));
        cursor: pointer;
    }
    .history-toggle:hover {
        background: rgb(var(--em-muted) / 0.1);
    }
    .history-body {
        margin-top: 0.6rem;
        padding: 0.8rem;
        border: 1px solid rgb(var(--em-muted) / 0.3);
        border-radius: 4px;
    }
    .controls {
        display: flex;
        align-items: baseline;
        gap: 0.7rem;
    }
    .reload-btn {
        padding: 0.2rem 0.7rem;
        background: rgb(var(--em-muted) / 0.15);
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        color: rgb(var(--em-fg));
        cursor: pointer;
        font-size: 0.8rem;
    }
    .reload-btn:hover {
        background: rgb(var(--em-muted) / 0.25);
    }
    .reload-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    .events {
        list-style: none;
        padding: 0;
        margin: 0;
        max-height: 18rem;
        overflow-y: auto;
    }
    .event-row {
        display: grid;
        grid-template-columns: 5rem 5rem 1fr;
        gap: 0.5rem;
        align-items: baseline;
        padding: 0.1rem 0;
    }
    .dead-pids {
        list-style: none;
        padding: 0;
        margin: 0;
        max-height: 18rem;
        overflow-y: auto;
    }
    .dead-row {
        width: 100%;
        text-align: left;
        display: grid;
        grid-template-columns: 5rem 1fr 4rem 8rem;
        gap: 0.5rem;
        align-items: baseline;
        padding: 0.15rem 0.3rem;
        background: transparent;
        border: 1px solid transparent;
        border-radius: 3px;
        color: inherit;
        cursor: pointer;
        font-family: inherit;
        font-size: inherit;
    }
    .dead-row:hover {
        background: rgb(var(--em-muted) / 0.1);
    }
    .dead-row.selected {
        background: rgb(var(--em-accent) / 0.15);
        border-color: rgb(var(--em-accent) / 0.5);
    }
    .trajectory-block {
        padding: 0.6rem;
        border: 1px solid rgb(var(--em-muted) / 0.3);
        border-radius: 4px;
        background: rgb(var(--em-bg-raised) / 0.4);
    }
    .close-traj {
        margin-left: 0.5rem;
        background: transparent;
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        padding: 0 0.4rem;
        color: rgb(var(--em-muted));
        cursor: pointer;
    }
    .close-traj:hover {
        color: rgb(var(--em-fg));
    }
    .error {
        color: rgb(var(--em-critical));
        margin-top: 0.4rem;
    }
</style>
