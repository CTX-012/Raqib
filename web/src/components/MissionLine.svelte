<script lang="ts">
    import type { WireMission } from '../lib/types';
    export let mission: WireMission;
    export let server_time: string;

    $: clock = (() => {
        try {
            const d = new Date(server_time);
            const hh = String(d.getHours()).padStart(2, '0');
            const mm = String(d.getMinutes()).padStart(2, '0');
            const ss = String(d.getSeconds()).padStart(2, '0');
            return `${hh}:${mm}:${ss}`;
        } catch {
            return '--:--:--';
        }
    })();
</script>

<h1 class="text-fg font-bold text-base flex items-center gap-2 m-0">
    <span class="text-accent">edge_monitor</span>
    <span class="text-fg-muted">·</span>
    <span>{mission.workloads} workloads</span>
    <span class="text-fg-muted">·</span>
    <span class={mission.degraded > 0 ? 'text-attention' : 'text-fg-muted'}>
        {mission.degraded} degraded
    </span>
    <span class="text-fg-muted">·</span>
    <span class="text-fg-muted">{clock}</span>
</h1>
