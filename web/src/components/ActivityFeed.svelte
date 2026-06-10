<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 74 — shape-A click-to-expand activity feed.
     *
     * Builds on D71 (uniform 3-source feed: exits + governor kills
     * + Tier-1.3 regressions). EXIT and KILL rows are now
     * click-to-expand: a second click on the same row collapses;
     * the {#each} key is unchanged so the each_key_duplicate
     * failure class (D65) stays closed.
     *
     * REGRESSION rows expand to nothing per dispatch hard rule #4
     * ("no fabricated exit fields") — the renderer suppresses the
     * expand chevron for that kind so operators don't get a
     * disappointing empty drop-down.
     *
     * Shape B detail (D71) carries everything as flat optional
     * fields; we render the populated set per `kind`. STOP #3:
     * `peak_vram_mb = 0` with `vram_unmeasured = true` reads
     * "no measurements," NOT "0 MB" — a tick-window-short exit
     * shouldn't lie about GPU usage.
     */
    import type {
        WireActivityEntry,
        WireActivityDetail,
    } from '../lib/types';
    import { ACTIVITY_FEED_WEB_MAX } from '../lib/limits';
    export let activity: WireActivityEntry[];

    // Track which entry is expanded. Key matches the {#each} key
    // exactly: `${kind}-${pid}-${timestamp}` (D65 composite).
    let expandedKey: string | null = null;

    function entryKey(ev: WireActivityEntry): string {
        return `${ev.kind}-${ev.pid}-${ev.timestamp}`;
    }

    function toggleExpand(ev: WireActivityEntry): void {
        // Regression rows have no detail; don't toggle.
        if (ev.kind === 'regression' || !ev.detail) {
            return;
        }
        const k = entryKey(ev);
        expandedKey = expandedKey === k ? null : k;
    }

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

    function kindLabel(kind: WireActivityEntry['kind']): string {
        return kind;
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

    // Render `peak_vram_mb` honestly per the STOP #3 contract:
    // a `vram_unmeasured = true` row must not show "0 MB" — the
    // short-lived process never measured VRAM at all. Same shape
    // for the renderer; the truth depends on `vram_unmeasured`.
    function vramLabel(detail: WireActivityDetail): string {
        if (detail.peak_vram_mb === undefined) {
            return '—';
        }
        if (detail.vram_unmeasured) {
            return 'no measurements';
        }
        return `${detail.peak_vram_mb} MB`;
    }

    function expandable(ev: WireActivityEntry): boolean {
        return ev.kind !== 'regression' && !!ev.detail;
    }
</script>

<div class="rounded border border-fg-muted/30 p-4 bg-bg-raised/40">
    <h2 class="text-fg-muted text-sm font-bold mb-3">Activity</h2>

    {#if activity.length === 0}
        <div class="text-fg-muted italic text-sm py-2">No recent activity.</div>
    {:else}
        <ul class="space-y-1 text-sm">
            {#each activity.slice(0, ACTIVITY_FEED_WEB_MAX) as ev (`${ev.kind}-${ev.pid}-${ev.timestamp}`)}
                {@const key = entryKey(ev)}
                {@const isOpen = expandedKey === key}
                {@const canExpand = expandable(ev)}
                <li>
                    <button
                        type="button"
                        class="w-full text-left grid grid-cols-[5rem_1rem_1fr_auto] gap-x-3 items-center {canExpand ? 'cursor-pointer hover:bg-fg-muted/10 rounded' : 'cursor-default'} py-0.5"
                        on:click={() => toggleExpand(ev)}
                        aria-expanded={isOpen}
                        disabled={!canExpand}
                    >
                        <span class="text-fg-muted text-xs">{shortTime(ev.timestamp)}</span>
                        <span class="text-fg-muted text-xs">
                            {#if canExpand}
                                {isOpen ? '▾' : '▸'}
                            {:else}
                                &nbsp;
                            {/if}
                        </span>
                        <span class="text-fg truncate" title={ev.summary}>{ev.summary}</span>
                        <span class="text-xs {severityClass(ev.severity)}">
                            {kindLabel(ev.kind)}
                        </span>
                    </button>
                    {#if isOpen && ev.detail}
                        <div class="ml-[6rem] mt-1 mb-2 pl-3 border-l border-fg-muted/30 text-xs space-y-0.5">
                            {#if ev.kind === 'exit'}
                                {#if ev.detail.exit_kind}
                                    <div>
                                        <span class="text-fg-muted">cause</span>:
                                        <span class="text-fg">{ev.detail.exit_kind}</span>
                                        {#if ev.detail.exit_detail}
                                            <span class="text-fg-muted">— {ev.detail.exit_detail}</span>
                                        {/if}
                                    </div>
                                {/if}
                                {#if ev.detail.uptime_secs !== undefined}
                                    <div>
                                        <span class="text-fg-muted">uptime</span>:
                                        <span class="text-fg">{ev.detail.uptime_secs}s</span>
                                    </div>
                                {/if}
                                {#if ev.detail.peak_rss_mb !== undefined}
                                    <div>
                                        <span class="text-fg-muted">peak RSS</span>:
                                        <span class="text-fg">{ev.detail.peak_rss_mb} MB</span>
                                    </div>
                                {/if}
                                <div>
                                    <span class="text-fg-muted">peak GPU memory</span>:
                                    <span class="text-fg">{vramLabel(ev.detail)}</span>
                                </div>
                                {#if ev.detail.peak_cpu_pct !== undefined}
                                    <div>
                                        <span class="text-fg-muted">CPU</span>:
                                        <span class="text-fg">avg {ev.detail.avg_cpu_pct?.toFixed(0) ?? '—'}% / peak {ev.detail.peak_cpu_pct.toFixed(0)}%</span>
                                    </div>
                                {/if}
                            {:else if ev.kind === 'kill'}
                                {#if ev.detail.action}
                                    <div>
                                        <span class="text-fg-muted">signal</span>:
                                        <span class="text-fg">{ev.detail.action}</span>
                                    </div>
                                {/if}
                                {#if ev.detail.success !== undefined}
                                    <div>
                                        <span class="text-fg-muted">delivered</span>:
                                        <span class="text-fg">{ev.detail.success ? 'yes' : 'no'}</span>
                                    </div>
                                {/if}
                                {#if ev.detail.error_msg}
                                    <div>
                                        <span class="text-fg-muted">error</span>:
                                        <span class="text-critical">{ev.detail.error_msg}</span>
                                    </div>
                                {/if}
                            {/if}
                        </div>
                    {/if}
                </li>
            {/each}
        </ul>
    {/if}
</div>
