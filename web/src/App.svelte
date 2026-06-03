<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { snapshot, connectionStatus, theme, type ThemeName } from './lib/stores';
    import { connect, disconnect } from './lib/ws';
    import MissionLine from './components/MissionLine.svelte';
    import VitalsPanel from './components/VitalsPanel.svelte';
    import WorkloadsPanel from './components/WorkloadsPanel.svelte';
    import ActivityFeed from './components/ActivityFeed.svelte';
    import ConnectionPill from './components/ConnectionPill.svelte';
    import AlertsPanel from './components/AlertsPanel.svelte';

    onMount(() => {
        connect();
    });
    onDestroy(() => {
        disconnect();
    });

    function setTheme(t: ThemeName): void {
        theme.set(t);
    }
</script>

<div class="min-h-screen flex flex-col">
    <header class="px-6 pt-4 pb-2 flex items-center gap-4 border-b border-fg-muted/20">
        <MissionLine mission={$snapshot.mission} server_time={$snapshot.server_time} />
        <div class="ml-auto flex items-center gap-3 text-sm">
            <ConnectionPill status={$connectionStatus} />
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

    <!-- v1.1.13 / DISPATCH 42 — alert region. Renders above the
         main grid so visible alerts catch the operator's eye
         FIRST. Self-hides when no alerts are visible (matches the
         TUI's banner region behaviour). -->
    <div class="px-6 pt-4">
        <AlertsPanel alerts={$snapshot.alerts} />
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

    <footer class="px-6 py-2 text-sm text-fg-muted border-t border-fg-muted/20">
        Tick #{$snapshot.tick} · {$snapshot.workloads.length} workloads ·
        edge_monitor web companion (read-only) · use the TUI for control
    </footer>
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
</style>
