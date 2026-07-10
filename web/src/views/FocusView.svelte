<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 104 / PHASE 5 display modes step 5 —
     * FOCUS view. Single-workload deep-dive with client-buffered
     * live sparklines.
     *
     * Per PHASE5_DISPLAY_MODES_DESIGN.md §1.2 + §5.1:
     *
     * * SINGLE WORKLOAD: `?mode=focus&pid=N` — the operator picks
     *   ONE live workload; the view shows its full detail + a
     *   rolling 60-second trajectory of cpu/rss/vram.
     *
     * * CLIENT-BUFFERED (§5.1, the load-bearing decision): the
     *   sparkline data lives entirely CLIENT-SIDE. Each 1 Hz
     *   `/api/snapshot` poll appends the focused PID's current
     *   metrics to a rolling 60-entry buffer. No new endpoint, no
     *   contract, no ux_contract touch. Buffer RESETS on reload
     *   — acceptable per §5.1's "watch this workload NOW" mental
     *   model. Cross-reload persistence is deferred to step 9
     *   (v-next, CAR-gated).
     *
     * * REUSES D95 TrajectoryChart: the sparkline is the SAME
     *   chart HistoryPage uses for dead-PID drill-in. Same M/L
     *   path with pen-lift for the VRAM gap primitive — the
     *   honesty invariant carries through automatically.
     *
     * * VRAM UNMEASURED = GAP (§C4). When the wire ships
     *   `vram_mb: null` for a tick (driver unloaded — common on
     *   this host), the buffer stores `undefined` at that
     *   position; the chart lifts the pen. NEVER coerced to 0 —
     *   a 0-line would falsely read as "used 0 VRAM."
     *
     * * PID-GONE / NO-PID: graceful states. Missing pid → picker.
     *   Nonexistent pid + no buffered history → "not found, may
     *   have exited". Workload dies mid-watch → the sparkline
     *   stops growing + an "exited" banner appears above the
     *   frozen trajectory.
     */
    import { onMount, onDestroy } from 'svelte';
    import { get, type Unsubscriber } from 'svelte/store';
    import { snapshot, focusPid } from '../lib/stores';
    import type {
        WireSample,
        WireTrajectory,
    } from '../lib/types';
    import TrajectoryChart from '../components/TrajectoryChart.svelte';

    // ── Rolling buffer ─────────────────────────────────────────
    //
    // Buffer holds up to MAX_BUFFER samples for exactly ONE pid at
    // a time. When the focused pid changes, the buffer resets —
    // holding samples for a stale pid would show a chart that
    // doesn't correspond to the workload the operator picked.
    //
    // `bufferPid` tracks whose data lives in `buffer`; guards the
    // reset-on-change semantic. `lastTick` de-dupes appends when
    // the snapshot store fires without a wire change (e.g. a
    // reactive glitch or an identical-payload re-set).

    const MAX_BUFFER = 60;

    let buffer: WireSample[] = [];
    let bufferPid: number | null = null;
    let lastTick = -1;
    let focusedExited = false;

    function updateBuffer(): void {
        const snap = get(snapshot);
        const pid = get(focusPid);
        if (pid === null) {
            if (bufferPid !== null || buffer.length > 0) {
                buffer = [];
                bufferPid = null;
                focusedExited = false;
                lastTick = -1;
            }
            return;
        }
        // Focused pid changed — start over.
        if (bufferPid !== pid) {
            buffer = [];
            bufferPid = pid;
            focusedExited = false;
            lastTick = -1;
        }
        // De-dupe: only append on tick advances. The snapshot
        // store fires on every poll (even payload-identical ones);
        // appending each fire would multi-count same-tick samples.
        if (snap.tick === lastTick) return;
        lastTick = snap.tick;

        const wl = snap.workloads.find((w) => w.pid === pid);
        if (!wl) {
            // Focused PID vanished from the live workloads list.
            // We DON'T reset the buffer — the operator was watching
            // this workload's history and it just died; the frozen
            // buffer is exactly what they need to see. The "exited"
            // banner overlays it. Only flip the flag if we had
            // something to freeze; before-first-sample is "never
            // seen it," not "just died."
            if (buffer.length > 0) focusedExited = true;
            return;
        }
        focusedExited = false;

        // VRAM honesty: null on the wire ⇒ UNMEASURED. Encode as
        // `undefined` on the sample so TrajectoryChart's isMeasured()
        // predicate lifts the pen (the M/L gap primitive). Do NOT
        // coerce to 0 — the whole D102/D95/D103 discriminator lives
        // in this one line.
        const sample: WireSample = {
            timestamp: snap.server_time,
            cpu_pct: wl.cpu_pct,
            rss_mb: wl.rss_mb,
            vram_mb: wl.vram_mb === null ? undefined : wl.vram_mb,
        };
        if (buffer.length >= MAX_BUFFER) {
            buffer = [...buffer.slice(1), sample];
        } else {
            buffer = [...buffer, sample];
        }
    }

    // Subscribe to snapshot + focusPid changes. Using an explicit
    // subscribe (rather than a `$:` reactive block that reads both
    // stores + writes to buffer) sidesteps Svelte's reactive-block
    // fire ordering — updateBuffer is a plain function called from
    // both subscribes and reads the freshest store values via get().
    let unsubSnapshot: Unsubscriber | null = null;
    let unsubFocusPid: Unsubscriber | null = null;

    onMount(() => {
        unsubSnapshot = snapshot.subscribe(() => updateBuffer());
        unsubFocusPid = focusPid.subscribe(() => updateBuffer());
    });
    onDestroy(() => {
        unsubSnapshot?.();
        unsubFocusPid?.();
    });

    // ── Derived state ──────────────────────────────────────────

    $: focusedWorkload = $focusPid !== null
        ? $snapshot.workloads.find((w) => w.pid === $focusPid) ?? null
        : null;

    $: allWorkloads = $snapshot.workloads;
    $: otherWorkloads = $focusPid !== null
        ? allWorkloads.filter((w) => w.pid !== $focusPid)
        : allWorkloads;

    // Package the rolling buffer as a WireTrajectory for
    // TrajectoryChart. The chart handles empty buffers gracefully
    // (renders its own "No samples in this trajectory" line).
    $: trajectory = buffer.length > 0
        ? ({
              samples: buffer,
              first_sample_at: buffer[0].timestamp,
              last_sample_at: buffer[buffer.length - 1].timestamp,
          } as WireTrajectory)
        : null;

    $: statusClass = focusedWorkload
        ? statusToClass(focusedWorkload.status)
        : 'text-fg-muted';

    function statusToClass(s: string): string {
        switch (s) {
            case 'critical':
                return 'text-critical';
            case 'attention':
                return 'text-attention';
            case 'healthy':
                return 'text-healthy';
            default:
                return 'text-fg-muted';
        }
    }

    function focusHref(pid: number): string {
        return `?mode=focus&pid=${pid}`;
    }

    /**
     * Handle picker clicks in-place: update the focusPid store
     * (the URL syncs via mode_url.ts's subscribe) instead of a
     * full page navigation. Keeps the SPA state — the buffer
     * resets on pid change per updateBuffer's own logic, but
     * the theme / other in-memory state survives. The `<a href>`
     * on the row is still valid for right-click / middle-click
     * open-in-new-tab flows; this only intercepts primary clicks.
     */
    function onPickerClick(event: MouseEvent, pid: number): void {
        // Let modifier-clicks (Cmd/Ctrl for new tab, Shift for
        // window) do the normal browser thing.
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
            return;
        event.preventDefault();
        focusPid.set(pid);
    }

    function bufferSpanSecs(): number {
        if (buffer.length < 2) return 0;
        const first = new Date(buffer[0].timestamp).getTime();
        const last = new Date(buffer[buffer.length - 1].timestamp).getTime();
        return Math.max(0, Math.round((last - first) / 1000));
    }
</script>

<div
    class="focus-view flex-1 flex flex-col overflow-hidden"
    data-testid="focus-view"
>
    <div
        class="focus-body flex-1 grid grid-cols-1 lg:grid-cols-[minmax(0,3fr)_minmax(0,1fr)] gap-4 px-6 py-4 overflow-y-auto"
    >
        <!-- ── Main column ───────────────────────────────────── -->
        <section
            class="focus-main space-y-4 min-w-0"
            data-testid="focus-main"
        >
            {#if $focusPid === null}
                <!--
                    No pid in the URL. Not an error state — a
                    graceful "pick a workload" prompt. The side rail
                    on the right IS the picker; here we just tell
                    the operator where to click.
                -->
                <div
                    class="rounded border border-fg-muted/30 p-8 bg-bg-raised/40 text-center"
                    data-testid="focus-nopid"
                >
                    <h2 class="text-fg text-lg font-bold mb-2">
                        Focus mode
                    </h2>
                    <p class="text-fg-muted text-sm">
                        Pick a workload from the side rail to watch its
                        live vitals and rolling trajectory.
                    </p>
                    <p class="text-fg-muted text-xs mt-4">
                        Or set <code
                            class="bg-bg/60 px-1 rounded"
                        >?mode=focus&pid=&lt;PID&gt;</code
                        > in the URL directly.
                    </p>
                </div>
            {:else if focusedWorkload === null && buffer.length === 0}
                <!--
                    Requested pid isn't in the live workloads list
                    AND we never buffered any of its samples. Two
                    common cases: (a) the URL was bookmarked from an
                    old session and the PID is long-dead; (b) the
                    URL was typo'd. Either way, point at History for
                    dead-PID drill-in.
                -->
                <div
                    class="rounded border border-attention/40 p-8 bg-bg-raised/40 text-center"
                    data-testid="focus-notfound"
                >
                    <h2 class="text-attention text-lg font-bold mb-2">
                        Workload PID {$focusPid} not found
                    </h2>
                    <p class="text-fg-muted text-sm">
                        No live workload with that PID. It may have
                        exited before this session opened.
                    </p>
                    <p class="text-fg-muted text-xs mt-4">
                        Try
                        <a
                            class="text-accent underline"
                            href="?mode=history"
                        >History mode</a
                        > to browse the dead-PID index, or pick a live
                        workload from the side rail.
                    </p>
                </div>
            {:else}
                <!-- Live workload OR mid-watch exit (buffered history). -->
                <div
                    class="focus-header rounded border border-fg-muted/30 p-4 bg-bg-raised/40"
                    data-testid="focus-header"
                >
                    <div class="flex items-baseline gap-3 flex-wrap">
                        <span class="{statusClass} text-lg font-bold" data-testid="focus-status">
                            {focusedWorkload?.status ?? '—'}
                        </span>
                        <span class="text-fg text-lg font-bold" data-testid="focus-name">
                            {focusedWorkload?.model_name
                                ?? focusedWorkload?.name
                                ?? `PID ${$focusPid}`}
                        </span>
                        <span class="text-fg-muted text-sm">
                            PID {$focusPid}
                        </span>
                        {#if focusedWorkload}
                            <span class="text-fg-muted text-sm">
                                · {focusedWorkload.workload_category}
                                · {focusedWorkload.activity_state ?? '—'}
                            </span>
                        {/if}
                    </div>
                    {#if focusedExited}
                        <div
                            class="mt-2 text-critical text-sm"
                            data-testid="focus-exited"
                        >
                            ⚠ Workload exited during watch. The trajectory
                            below shows what was captured before exit.
                            Try
                            <a
                                class="text-accent underline"
                                href="?mode=history"
                            >History</a
                            >
                            for the persistent post-mortem.
                        </div>
                    {/if}
                </div>

                <!-- Big-number current-value tiles. -->
                <div
                    class="focus-tiles grid grid-cols-2 sm:grid-cols-4 gap-3"
                    data-testid="focus-tiles"
                >
                    <div class="focus-tile" data-testid="focus-tile-cpu">
                        <div class="focus-tile-label text-fg-muted">CPU</div>
                        <div class="focus-tile-value text-fg">
                            {focusedWorkload
                                ? focusedWorkload.cpu_pct.toFixed(1) + '%'
                                : '—'}
                        </div>
                    </div>
                    <div class="focus-tile" data-testid="focus-tile-rss">
                        <div class="focus-tile-label text-fg-muted">RSS</div>
                        <div class="focus-tile-value text-fg">
                            {focusedWorkload
                                ? focusedWorkload.rss_mb + ' MB'
                                : '—'}
                        </div>
                    </div>
                    <div class="focus-tile" data-testid="focus-tile-vram">
                        <div class="focus-tile-label text-fg-muted">VRAM</div>
                        {#if focusedWorkload && focusedWorkload.vram_mb !== null && focusedWorkload.vram_mb !== undefined}
                            <div
                                class="focus-tile-value text-fg"
                                data-testid="focus-vram-value"
                            >
                                {focusedWorkload.vram_mb} MB
                            </div>
                        {:else}
                            <!-- VRAM UNMEASURED (§C4) at the big-tile level. -->
                            <div
                                class="focus-tile-value text-fg-muted"
                                data-testid="focus-vram-value"
                                data-testid-unmeasured="true"
                                title="No VRAM measurement this tick"
                            >
                                —
                            </div>
                        {/if}
                    </div>
                    <div class="focus-tile" data-testid="focus-tile-throughput">
                        <div class="focus-tile-label text-fg-muted">
                            {focusedWorkload?.tokens_per_sec != null
                                ? 'tok/s'
                                : focusedWorkload?.fps != null
                                    ? 'fps'
                                    : 'Throughput'}
                        </div>
                        <div class="focus-tile-value text-fg">
                            {focusedWorkload?.tokens_per_sec != null
                                ? focusedWorkload.tokens_per_sec.toFixed(1)
                                : focusedWorkload?.fps != null
                                    ? focusedWorkload.fps.toFixed(1)
                                    : '—'}
                        </div>
                    </div>
                </div>

                <!-- Rolling sparkline: reuses D95 TrajectoryChart. -->
                <div
                    class="focus-chart rounded border border-fg-muted/30 p-4 bg-bg-raised/40"
                    data-testid="focus-chart"
                >
                    <div class="text-fg-muted text-xs mb-2">
                        Rolling trajectory · client-buffered ·
                        {buffer.length} sample{buffer.length === 1 ? '' : 's'}
                        {#if buffer.length >= 2}
                            over {bufferSpanSecs()}s
                        {/if}
                        · buffer resets on reload
                    </div>
                    {#if trajectory}
                        <TrajectoryChart {trajectory} />
                    {:else}
                        <div class="text-fg-muted italic text-sm py-4">
                            Waiting for the first sample…
                        </div>
                    {/if}
                </div>
            {/if}
        </section>

        <!-- ── Picker side rail ─────────────────────────────── -->
        <aside
            class="focus-picker min-w-0"
            data-testid="focus-picker"
        >
            <h2 class="text-fg text-sm font-bold mb-2">
                Workloads
                <span class="text-xs font-normal text-fg-muted">
                    ({allWorkloads.length})
                </span>
            </h2>
            <div
                class="rounded border border-fg-muted/30 p-2 bg-bg-raised/40"
            >
                {#if allWorkloads.length === 0}
                    <div class="text-fg-muted italic text-xs px-2 py-1">
                        No live workloads.
                    </div>
                {:else}
                    <ul class="space-y-1 text-sm">
                        {#each allWorkloads as w (w.pid)}
                            {@const isFocused = w.pid === $focusPid}
                            <li>
                                <a
                                    href={focusHref(w.pid)}
                                    on:click={(e) => onPickerClick(e, w.pid)}
                                    class="picker-row"
                                    class:picker-row--active={isFocused}
                                    data-testid="focus-picker-row"
                                    data-testid-pid={w.pid}
                                >
                                    <span class="{statusToClass(w.status)}">
                                        ●
                                    </span>
                                    <span class="text-fg truncate">
                                        {w.model_name ?? w.name}
                                    </span>
                                    <span class="text-fg-muted text-xs">
                                        pid {w.pid}
                                    </span>
                                </a>
                            </li>
                        {/each}
                    </ul>
                {/if}
            </div>
        </aside>
    </div>
</div>

<style>
    .focus-tile {
        display: flex;
        flex-direction: column;
        padding: 0.75rem;
        border: 1px solid rgb(var(--em-muted) / 0.3);
        border-radius: 4px;
        background: rgb(var(--em-bg) / 0.4);
    }
    .focus-tile-label {
        font-size: 0.7rem;
        text-transform: uppercase;
        letter-spacing: 0.1em;
        margin-bottom: 0.25rem;
    }
    .focus-tile-value {
        font-size: 1.5rem;
        font-weight: 700;
        line-height: 1.1;
        font-variant-numeric: tabular-nums;
    }
    .picker-row {
        display: grid;
        grid-template-columns: 1rem 1fr auto;
        gap: 0.5rem;
        align-items: baseline;
        padding: 0.25rem 0.5rem;
        border-radius: 3px;
        text-decoration: none;
        color: inherit;
    }
    .picker-row:hover {
        background: rgb(var(--em-muted) / 0.1);
    }
    .picker-row--active {
        background: rgb(var(--em-accent) / 0.2);
    }
</style>
