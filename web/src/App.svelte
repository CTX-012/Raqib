<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import {
        snapshot,
        connectionStatus,
        theme,
        mode,
        MODES,
        type ThemeName,
        type ModeName,
    } from './lib/stores';
    // v1.3.2 / DISPATCH 68 — REST polling replaces the v1.0.x
    // WebSocket push transport. Same `connect()`/`disconnect()`
    // surface so the onMount/onDestroy plumbing is unchanged.
    import { connect, disconnect } from './lib/rest';
    // v1.3.2 / DISPATCH 100 / PHASE 5 display modes step 1 — URL
    // ⇄ mode-store sync. Installed at mount, cleaned up on unmount.
    import { installModeUrlSync } from './lib/mode_url';
    import MissionLine from './components/MissionLine.svelte';
    import VitalsPanel from './components/VitalsPanel.svelte';
    import WorkloadsPanel from './components/WorkloadsPanel.svelte';
    import ActivityFeed from './components/ActivityFeed.svelte';
    import ConnectionPill from './components/ConnectionPill.svelte';
    import AlertsPanel from './components/AlertsPanel.svelte';
    import SettingsPanel from './components/SettingsPanel.svelte';
    // DISPATCH 3-panel — Top processes side-by-side (RAM/VRAM/CPU).
    // Parity with the TUI's render_three_panels — same three
    // sub-panels, same ranking (server-side sorted via
    // WireSnapshot::build_top_processes).
    import TopProcessesPanel from './components/TopProcessesPanel.svelte';
    // v1.3.2 / DISPATCH 101 / PHASE 5 display modes step 2 —
    // HistoryPage no longer lives inside the dashboard grid as a
    // collapsible panel; it's now the exclusive content of the
    // HISTORY mode via HistoryView (full-viewport). Removed from
    // the dashboard branch to keep the dashboard lean — a
    // deliberate, single-home-per-component change, not a
    // regression. See docs/PHASE5_DISPLAY_MODES_DESIGN.md §1.2.
    import HistoryView from './views/HistoryView.svelte';
    // v1.3.2 / DISPATCH 102 / PHASE 5 display modes step 3 —
    // KIOSK view. Glance-only wall monitor: overall severity +
    // big-number tiles, no interaction. Auto-hc default when
    // `?mode=kiosk` is loaded from URL (see lib/mode_url.ts).
    import KioskView from './views/KioskView.svelte';
    // v1.3.2 / DISPATCH 103 / PHASE 5 display modes step 4 —
    // TIMELINE view. Chronological incident-review: VitalsStrip
    // + alerts + activity + workloads side rail. Interaction-
    // first (distinct from kiosk's glance-only).
    import TimelineView from './views/TimelineView.svelte';
    // v1.3.2 / DISPATCH 104 / PHASE 5 display modes step 5 —
    // FOCUS view. Single-workload deep-dive with client-buffered
    // live sparklines. Reuses the D95 TrajectoryChart for the
    // rolling trajectory; no new endpoint (§5.1 / §8).
    import FocusView from './views/FocusView.svelte';

    // v1.3.2 / DISPATCH 107 FIX 5 — `PLACEHOLDER_STEP` and its
    // `ModePlaceholder.svelte` companion were retired here; every
    // §7 mode (dashboard/history/kiosk/timeline/focus) has its
    // real view landed. The scaffold from D100 has fully served
    // its purpose.

    let modeUrlCleanup: (() => void) | null = null;

    onMount(() => {
        connect();
        modeUrlCleanup = installModeUrlSync();
    });
    onDestroy(() => {
        disconnect();
        if (modeUrlCleanup) modeUrlCleanup();
    });

    function setTheme(t: ThemeName): void {
        theme.set(t);
    }

    function onModeChange(e: Event): void {
        const target = e.currentTarget as HTMLSelectElement;
        // `mode.set` triggers the mode_url subscribe → pushState.
        // The store's own coerceMode() at read-time keeps invalid
        // values from ever landing; the dropdown only offers the
        // valid MODES tuple, so this write is always safe.
        mode.set(target.value as ModeName);
    }
</script>

<div class="min-h-screen flex flex-col">
    <header class="px-6 pt-4 pb-2 flex items-center gap-4 border-b border-fg-muted/20">
        <MissionLine mission={$snapshot.mission} server_time={$snapshot.server_time} />
        <div class="ml-auto flex items-center gap-3 text-sm">
            <ConnectionPill status={$connectionStatus} />
            <!--
                v1.3.2 / DISPATCH 100 / PHASE 5 display modes step 1 —
                the mode <select> sits beside the theme buttons per
                design §2.1 ("mirror theme's visual weight"). Selecting
                a mode writes to the store; `installModeUrlSync` picks
                that up and calls history.pushState (no reload). Iterates
                the MODES tuple so the order matches the store's
                canonical list.
            -->
            <label class="flex items-center gap-1" data-testid="mode-select-label">
                <span class="text-fg-muted">mode</span>
                <select
                    class="mode-select"
                    data-testid="mode-select"
                    aria-label="Display mode"
                    value={$mode}
                    on:change={onModeChange}
                >
                    {#each MODES as m (m)}
                        <option value={m}>{m}</option>
                    {/each}
                </select>
            </label>
            <div class="flex gap-1" role="group" aria-label="Theme">
                <button
                    class:active={$theme === 'dark'}
                    class="theme-btn"
                    on:click={() => setTheme('dark')}>dark</button>
                <button
                    class:active={$theme === 'light'}
                    class="theme-btn"
                    on:click={() => setTheme('light')}>light</button>
                <button
                    class:active={$theme === 'hc'}
                    class="theme-btn"
                    on:click={() => setTheme('hc')}>hc</button>
            </div>
        </div>
    </header>

    <!--
        v1.3.2 / DISPATCH 100 / PHASE 5 display modes step 1 — reactive
        routing. The other 4 modes point at their real views
        (D101-D104); dashboard renders its main-grid subtree here.

        Rendered inside {#if} branches (NOT swapped via a store /
        dynamic component) so Svelte's compile-time analysis produces
        an unchanged sub-DOM for the dashboard path — no wrapper
        element around the alerts region / main grid / history /
        settings / footer that could shift layout or class inheritance.
    -->
    {#if $mode === 'dashboard'}
        <!-- v1.1.13 / DISPATCH 42 — alert region. Renders above the
             main grid so visible alerts catch the operator's eye
             FIRST. Self-hides when no alerts are visible (matches the
             TUI's banner region behaviour). -->
        <div class="px-6 pt-4">
            <AlertsPanel
                alerts={$snapshot.alerts}
                recommendations={$snapshot.recommendations}
            />
        </div>

        <main class="grid grid-cols-1 lg:grid-cols-3 gap-6 px-6 py-4 flex-1">
            <section class="lg:col-span-1">
                <VitalsPanel vitals={$snapshot.vitals} />
            </section>
            <section class="lg:col-span-2 space-y-6">
                <WorkloadsPanel workloads={$snapshot.workloads} />
                <ActivityFeed activity={$snapshot.activity} />
            </section>
        </main>

        <!--
            v1.3.2 / DISPATCH 101 — the D95 collapsible HistoryPage
            was removed from the dashboard grid here. History has
            its own mode now (`?mode=history` → HistoryView).
            Intentional dashboard change — the only diff between
            D100 and D101 in the dashboard branch. Everything else
            (VitalsPanel / WorkloadsPanel / ActivityFeed /
            SettingsPanel / footer) is byte-identical to D100.
        -->

        <!--
            DISPATCH 3-panel — Top processes side-by-side (RAM /
            VRAM / CPU). Full-width row below the main grid, above
            the settings toggle. TUI + web parity: this is the web
            side of the TUI's `render_three_panels`.
        -->
        <div class="px-6 pb-4">
            <TopProcessesPanel top_processes={$snapshot.top_processes} />
        </div>

        <!-- v1.3.2 / DISPATCH 86 — settings panel. Collapsed by default.
             Tunes thresholds + sustain windows; auto_actuate stays
             TOML+restart only and is shown as a read-only status badge
             inside the panel. -->
        <div class="px-6 pb-4">
            <SettingsPanel />
        </div>

        <footer class="px-6 py-2 text-sm text-fg-muted border-t border-fg-muted/20">
            Tick #{$snapshot.tick} · {$snapshot.workloads.length} workloads ·
            edge_monitor web companion (read-only) · use the TUI for control
        </footer>
    {:else if $mode === 'history'}
        <HistoryView />
    {:else if $mode === 'kiosk'}
        <KioskView />
    {:else if $mode === 'timeline'}
        <TimelineView />
    {:else if $mode === 'focus'}
        <FocusView />
    {/if}
</div>

<style>
    .theme-btn {
        padding: 0.15rem 0.5rem;
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        background: transparent;
        color: rgb(var(--em-muted));
        cursor: pointer;
        font-family: inherit;
        font-size: 0.85rem;
    }
    .theme-btn:hover {
        color: rgb(var(--em-fg));
        border-color: rgb(var(--em-fg) / 0.6);
    }
    .theme-btn.active {
        color: rgb(var(--em-bg));
        background: rgb(var(--em-accent));
        border-color: rgb(var(--em-accent));
    }
    /*
     * v1.3.2 / DISPATCH 100 — mode dropdown. Matches the theme
     * buttons' visual weight (per §2.1) so the header reads as one
     * control group. Font family / size mirror .theme-btn.
     */
    .mode-select {
        padding: 0.15rem 0.5rem;
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        background: transparent;
        color: rgb(var(--em-fg));
        cursor: pointer;
        font-family: inherit;
        font-size: 0.85rem;
    }
    .mode-select:hover {
        border-color: rgb(var(--em-fg) / 0.6);
    }
</style>
