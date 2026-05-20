<script lang="ts">
    import type { WireRunRecord } from '../lib/types';
    export let activity: WireRunRecord[];

    function exitClass(kind: string): string {
        switch (kind) {
            case 'clean':
                return 'text-healthy';
            case 'governor':
            case 'signal':
                return 'text-attention';
            default:
                return 'text-critical';
        }
    }

    function shortTime(iso: string): string {
        try {
            const d = new Date(iso);
            return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
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
            {#each activity.slice(0, 12) as ev (ev.pid + '-' + ev.exit_time)}
                <li class="grid grid-cols-[5rem_1fr_auto_auto] gap-x-3">
                    <span class="text-fg-muted text-xs">{shortTime(ev.exit_time)}</span>
                    <span class="text-fg truncate">{ev.model_name ?? ev.name}</span>
                    <span class="text-fg-muted text-xs">{ev.uptime_secs}s</span>
                    <span class="text-xs {exitClass(ev.exit_kind)}">
                        {ev.exit_kind}{#if ev.exit_detail} · {ev.exit_detail}{/if}
                    </span>
                </li>
            {/each}
        </ul>
    {/if}
</div>
