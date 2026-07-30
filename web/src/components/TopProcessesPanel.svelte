<script lang="ts">
    /*
     * DISPATCH 3-panel — Top processes side-by-side panel.
     *
     * Parity with the TUI `render_three_panels` at
     * src/ui/panels/top_processes.rs. Three sub-panels (RAM / VRAM /
     * CPU), each showing top-5 processes, all reading the same
     * server-side sort projection (`WireTopProcesses` populated by
     * `WireSnapshot::build_top_processes`). Ordering + tiebreak
     * (PID-ascending) match the TUI exactly — same rank order on
     * both surfaces.
     *
     * ## Responsive layout
     *
     * Wide viewports: 3-column grid side-by-side. Narrow viewports:
     * stack vertically. Matches the TUI's horizontal-vs-stacked
     * fallback (area.width < 3 × 28 chars → vertical stack).
     *
     * ## VRAM honesty
     *
     * `by_vram` is pre-filtered on the Rust side via
     * `top_n_by_vram_honest` — entries without measured VRAM are
     * DROPPED before truncation, so the list may be SHORTER than 5
     * (or empty) when few processes hold GPU allocations. This
     * component:
     *   * NEVER pads the VRAM list to 5 with fake `0 MB` rows.
     *   * On any single entry, if `vram_mb` is absent (undefined),
     *     renders "—" instead of `0` (defensive; the honest filter
     *     should have dropped it already).
     *   * When the list is empty, shows an italic "no GPU users"
     *     empty state — matches the TUI's `render_sub_panel` empty
     *     branch text.
     */
    import type { WireTopProcess, WireTopProcesses } from '../lib/types';
    export let top_processes: WireTopProcesses | undefined = undefined;

    // Coerce undefined (pre-bump payload) to empty lists so the
    // component still renders three panel frames without crashing.
    // The empty-state branch then handles each column's "no data"
    // display.
    $: safe = top_processes ?? { by_ram: [], by_vram: [], by_cpu: [] };

    /**
     * Format bytes-as-MB the same way `format_rss` does in the TUI:
     * < 1024 MB shows as "NNN MB", >= 1024 MB shows as "N.N GB".
     * Keeps the two surfaces number-for-number aligned.
     */
    function formatMb(mb: number): string {
        if (mb >= 1024) {
            const gb = mb / 1024;
            return `${gb.toFixed(1)} GB`;
        }
        return `${mb} MB`;
    }
</script>

<div class="rounded border border-accent/40 p-4 bg-bg-raised/40">
    <h2 class="text-accent text-sm font-bold mb-3">Top Processes</h2>

    <!--
        Three sub-panels in a responsive grid. Wide viewports
        (md and up) show 3 columns side-by-side; narrow stacks
        vertically — same shape as the TUI's Direction::Horizontal
        vs Direction::Vertical fallback.
    -->
    <div class="grid grid-cols-1 md:grid-cols-3 gap-4">
        <!-- RAM sub-panel -->
        <div data-testid="top-processes-ram" class="border border-fg-muted/20 rounded p-3 bg-bg-raised/30">
            <h3 class="text-fg-muted text-xs uppercase tracking-wide mb-2 border-b border-fg-muted/20 pb-1">
                Top processes (by RAM)
            </h3>
            {#if safe.by_ram.length === 0}
                <div class="text-fg-muted italic text-xs">—</div>
            {:else}
                <ul class="space-y-0.5 text-sm">
                    {#each safe.by_ram as p (`ram-${p.pid}`)}
                        <li class="flex justify-between items-baseline" data-testid="top-row-ram">
                            <span class="text-fg truncate mr-2" title={p.name}>{p.name}</span>
                            <span class="text-fg-muted text-xs tabular-nums whitespace-nowrap" data-testid="top-row-ram-value">
                                {formatMb(p.rss_mb)}
                            </span>
                        </li>
                    {/each}
                </ul>
            {/if}
        </div>

        <!-- VRAM sub-panel — honest short list, no fake zeros -->
        <div data-testid="top-processes-vram" class="border border-fg-muted/20 rounded p-3 bg-bg-raised/30">
            <h3 class="text-fg-muted text-xs uppercase tracking-wide mb-2 border-b border-fg-muted/20 pb-1">
                Top processes (by VRAM)
            </h3>
            {#if safe.by_vram.length === 0}
                <!--
                    VRAM honesty: when no process on the host holds
                    GPU allocations, show "no GPU users" (italic
                    muted) — mirror the TUI `render_sub_panel`
                    empty state, and do NOT fabricate zero-VRAM
                    rows to pad to 5.
                -->
                <div class="text-fg-muted italic text-xs" data-testid="top-processes-vram-empty">
                    no GPU users
                </div>
            {:else}
                <ul class="space-y-0.5 text-sm">
                    {#each safe.by_vram as p (`vram-${p.pid}`)}
                        <li class="flex justify-between items-baseline" data-testid="top-row-vram">
                            <span class="text-fg truncate mr-2" title={p.name}>{p.name}</span>
                            {#if p.vram_mb != null}
                                <span class="text-fg text-xs tabular-nums whitespace-nowrap" data-testid="top-row-vram-value">
                                    {formatMb(p.vram_mb)}
                                </span>
                            {:else}
                                <!--
                                    Defensive: the honest filter on
                                    the server side drops
                                    vram_mb-absent entries before
                                    they reach the wire. If one
                                    slips through, render "—" —
                                    never coerce to 0.
                                -->
                                <span class="text-fg-muted text-xs whitespace-nowrap" data-testid="top-row-vram-value" data-testid-unmeasured="true">
                                    —
                                </span>
                            {/if}
                        </li>
                    {/each}
                </ul>
            {/if}
        </div>

        <!--
            CPU sub-panel. Header carries the `per-core` clarification:
            the value is raw per-core (htop / top / ps convention —
            N cores = N × 100 % max), so a `210 %` reading on a 4-core
            box is honest, not a bug. Display-only pin; the arithmetic
            is unchanged. TUI mirror lives at
            `src/ui/panels/top_processes.rs::panel_title(Cpu)`.
        -->
        <div data-testid="top-processes-cpu" class="border border-fg-muted/20 rounded p-3 bg-bg-raised/30">
            <h3 class="text-fg-muted text-xs uppercase tracking-wide mb-2 border-b border-fg-muted/20 pb-1">
                Top processes (by CPU %, per-core)
            </h3>
            {#if safe.by_cpu.length === 0}
                <div class="text-fg-muted italic text-xs">—</div>
            {:else}
                <ul class="space-y-0.5 text-sm">
                    {#each safe.by_cpu as p (`cpu-${p.pid}`)}
                        <li class="flex justify-between items-baseline" data-testid="top-row-cpu">
                            <span class="text-fg truncate mr-2" title={p.name}>{p.name}</span>
                            <span class="text-fg-muted text-xs tabular-nums whitespace-nowrap" data-testid="top-row-cpu-value">
                                {p.cpu_pct.toFixed(1)}%
                            </span>
                        </li>
                    {/each}
                </ul>
            {/if}
        </div>
    </div>
</div>
