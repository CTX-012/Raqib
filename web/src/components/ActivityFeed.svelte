<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 71 — uniform activity feed.
     *
     * Renders the merged 3-source list the Rust wire builder
     * produces (`src/web/wire.rs::build_activity`): exits + governor
     * kills + Tier-1.3 regressions, time-descending, sliced to
     * ACTIVITY_FEED_WEB_MAX rows.
     *
     * Shape B: the server pre-classifies severity and pre-renders
     * `summary`. The TypeScript side ONLY maps severity to a
     * tailwind class and lays out the row — no template
     * substitution, no per-kind classification logic.
     *
     * {#each} keying: `${kind}-${pid}-${timestamp}` is unique
     * across all 3 sources, including the legitimate
     * "same PID has both an exit and a kill" case. Pid alone is
     * insufficient (the D65 each_key_duplicate failure class);
     * timestamp at sub-second resolution may also collide on a
     * busy tick. The composite is what stays unique.
     */
    import type { WireActivityEntry } from '../lib/types';
    import { ACTIVITY_FEED_WEB_MAX } from '../lib/limits';
    export let activity: WireActivityEntry[];

    // Pre-classified severity → tailwind class. Mirrors the same
    // single-source-of-truth pattern as AlertsPanel.svelte and
    // VitalsPanel.svelte: the contract pre-classified the
    // severity; we render.
    function severityClass(severity: WireActivityEntry['severity']): string {
        switch (severity) {
            case 'critical':
                return 'text-critical';
            case 'attention':
                return 'text-attention';
            case 'healthy':
            default:
                return 'text-healthy';
        }
    }

    // Per-kind label for the small badge in the right column. The
    // server-rendered `summary` is the primary content; this badge
    // gives the operator a one-glance hint of the event class.
    function kindLabel(kind: WireActivityEntry['kind']): string {
        switch (kind) {
            case 'exit':
                return 'exit';
            case 'kill':
                return 'kill';
            case 'regression':
                return 'regression';
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
</script>

<div class="rounded border border-fg-muted/30 p-4 bg-bg-raised/40">
    <h2 class="text-fg-muted text-sm font-bold mb-3">Activity</h2>

    {#if activity.length === 0}
        <div class="text-fg-muted italic text-sm py-2">No recent activity.</div>
    {:else}
        <ul class="space-y-1 text-sm">
            {#each activity.slice(0, ACTIVITY_FEED_WEB_MAX) as ev (`${ev.kind}-${ev.pid}-${ev.timestamp}`)}
                <li class="grid grid-cols-[5rem_1fr_auto] gap-x-3">
                    <span class="text-fg-muted text-xs">{shortTime(ev.timestamp)}</span>
                    <span class="text-fg truncate" title={ev.summary}>{ev.summary}</span>
                    <span class="text-xs {severityClass(ev.severity)}">
                        {kindLabel(ev.kind)}
                    </span>
                </li>
            {/each}
        </ul>
    {/if}
</div>
