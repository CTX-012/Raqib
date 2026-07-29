<script lang="ts">
    import type { ActivityState, WireWorkload, WorkloadStatus } from '../lib/types';
    export let workload: WireWorkload;

    const STATUS_GLYPH: Record<WorkloadStatus, string> = {
        healthy: '●',
        attention: '⚠',
        critical: '✕',
        loading: '○',
    };
    const STATUS_CLASS: Record<WorkloadStatus, string> = {
        healthy: 'text-healthy',
        attention: 'text-attention',
        critical: 'text-critical',
        loading: 'text-fg-muted',
    };

    // Phase 2 / DISPATCH 1 — activity-state label. Mirrors the TUI's
    // foreground-only Inspector #8 V1 treatment: no per-state color
    // (L21 §14 invariant — only status dots are colored on workload
    // rows). State distinction comes from the text label itself.
    const ACTIVITY_LABEL: Record<ActivityState, string> = {
        active: 'active',
        idle: 'idle',
        loading: 'loading',
        not_detected: '—',
    };

    // v1.0.1 B-NEW-6 + B-NEW-4 — branch on workload_category so an
    // Agent row with no metric reads "alive" (honest minimum signal:
    // the process exists; no activity claim), while LLM keeps
    // tokens/sec → KV → "running actively", and Vision keeps fps.
    // Pre-v1.0.1 the fallback was always "running actively" — every
    // Agent claude-code row claimed activity that wasn't measured.
    // CAR-20 (v0.3.11) lifted the "alive" literal to
    // `ux_contract::status::AGENT_ALIVE`; web stack doesn't import
    // contract types yet, so the literal lives here pending future
    // contract-derived-types wiring.
    $: primary = (() => {
        if (workload.tokens_per_sec != null) {
            return `${workload.tokens_per_sec.toFixed(1)} tok/s`;
        }
        if (workload.fps != null) {
            return `${workload.fps.toFixed(1)} fps`;
        }
        if (workload.workload_category === 'agent') {
            return 'alive';
        }
        return 'running actively';
    })();
</script>

<!--
    v1.3.2 / DISPATCH 98 — `data-testid="workload-row"` is the
    stable structural hook the browser render gate
    (`web/tests/browser_render_gate.mjs`) uses to count rendered
    rows. Durable across the CSS refactors the upcoming 5-web-modes
    work will bring — an inert attribute, no behavior, no bytes in
    the styled output. If you remove or rename it, update the
    harness selector in lockstep.
-->
<!--
    v1.3.2 / DISPATCH thermal+VRAM — the grid grew from 6 to 7 cells.
    Layout: status · name · primary · activity · CPU · RSS · VRAM.
    The VRAM cell was previously crammed into the RSS cell as a
    trailing `· NNNM GPU` suffix that operators reported as invisible
    next to the TUI's aligned column. Promoting to its own cell
    matches the TUI's shape (`vram_label` at src/ui/panels/workloads.rs:558)
    and gives the VRAM-honesty discriminator a stable render slot:
    unmeasured / zero → `—` (muted), measured → `NNNM` (foreground).
-->
<div data-testid="workload-row" class="grid grid-cols-[auto_1fr_auto_auto_auto_auto_auto] gap-x-3 py-0.5 items-baseline text-sm">
    <span class={STATUS_CLASS[workload.status]} aria-label={workload.status}>
        {STATUS_GLYPH[workload.status]}
    </span>
    <span class="text-fg truncate">
        {workload.model_name ?? workload.name}
    </span>
    <span class="text-fg-muted text-xs">{primary}</span>
    {#if workload.activity != null}
        <span class="text-fg-muted text-xs" aria-label={`activity: ${workload.activity}`}>
            {ACTIVITY_LABEL[workload.activity]}
        </span>
    {:else}
        <span></span>
    {/if}
    <span class="text-fg-muted text-xs">{workload.cpu_pct.toFixed(1)}% CPU</span>
    <span class="text-fg-muted text-xs tabular-nums">
        {workload.rss_mb}M{#if workload.ram_pct != null} ({workload.ram_pct.toFixed(1)}%){/if} RSS
    </span>
    <!--
        VRAM cell. VRAM honesty rule (CLAUDE.md): null / zero / absent
        → `—` (unmeasured or CPU-only workload), NEVER `0M`. Positive
        integer → `NNNM VRAM`. Zero-suffixed testid discriminates the
        two states for the browser gate — `data-testid-unmeasured` on
        the "—" branch matches the pattern used for VRAM everywhere
        else (D95 / D98 / D102 / D109).
    -->
    {#if workload.vram_mb != null && workload.vram_mb > 0}
        <span class="text-fg text-xs tabular-nums" data-testid="workload-vram">
            {workload.vram_mb}M VRAM
        </span>
    {:else}
        <span class="text-fg-muted text-xs tabular-nums" data-testid="workload-vram" data-testid-unmeasured="true">
            — VRAM
        </span>
    {/if}
</div>
