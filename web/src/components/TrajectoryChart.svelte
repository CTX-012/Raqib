<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 95 / PHASE 5 step 7 — trajectory chart.
     *
     * Simple SVG time-series for one dead workload's cpu / rss / vram
     * over its life. NO chart library (dispatch C5 — a heavy dep for
     * one screen would be scope creep).
     *
     * VRAM HONESTY (dispatch C4, the load-bearing invariant on this
     * component): a sample where `vram_mb` is `null`/`undefined`
     * renders as a GAP in the vram polyline — the line is broken
     * across the missing segment, not interpolated over it. A
     * measured 0 renders as a point ON the baseline. Given the
     * operator's GPU is unloaded in the common case, unmeasured is
     * the frequent case; it must be VISUALLY distinct from
     * measured-zero. A single unbroken line at 0 would falsely read
     * "used 0 VRAM the whole time." The gap + "no VRAM samples"
     * legend row is the honest render.
     */
    import type { WireTrajectory, WireSample } from '../lib/types';

    export let trajectory: WireTrajectory;

    // Layout constants — pinned so the caller doesn't have to
    // measure. The chart is responsive via viewBox; the container
    // just sets width.
    const VB_WIDTH = 640;
    const VB_HEIGHT = 200;
    const PAD_L = 44;
    const PAD_R = 12;
    const PAD_T = 16;
    const PAD_B = 24;
    const PLOT_W = VB_WIDTH - PAD_L - PAD_R;
    const PLOT_H = VB_HEIGHT - PAD_T - PAD_B;

    // Which series is visible. Independent toggles let the operator
    // isolate one axis when the ranges overlap awkwardly.
    let showCpu = true;
    let showRss = true;
    let showVram = true;

    $: samples = trajectory.samples;

    /**
     * Domain min/max on the time axis. The wire ships
     * `first_sample_at` / `last_sample_at` — trust them (the server
     * built them from the same iterator) but fall back to the sample
     * span so an empty-list edge case doesn't NaN-divide.
     */
    $: tMin = samples.length > 0
        ? new Date(samples[0].timestamp).getTime()
        : new Date(trajectory.first_sample_at).getTime();
    $: tMax = samples.length > 0
        ? new Date(samples[samples.length - 1].timestamp).getTime()
        : new Date(trajectory.last_sample_at).getTime();
    $: tSpan = Math.max(1, tMax - tMin);

    // Per-series y-domains. We plot each series in its OWN axis
    // (CPU %, RSS MB, VRAM MB) so an RSS spike doesn't crush the
    // CPU line to zero. Each polyline uses PLOT_H fully.
    $: cpuMax = Math.max(1, ...samples.map((s) => s.cpu_pct));
    $: rssMax = Math.max(1, ...samples.map((s) => s.rss_mb));
    $: vramMax = Math.max(
        1,
        ...samples
            .map((s) => (isMeasured(s) ? (s.vram_mb as number) : 0)),
    );

    /**
     * VRAM measurement discriminator — the honesty predicate.
     * Absent/null/undefined ⇒ NOT measured (gap). A number,
     * including `0`, ⇒ measured.
     */
    function isMeasured(s: WireSample): boolean {
        return s.vram_mb !== undefined && s.vram_mb !== null;
    }

    function xFor(iso: string): number {
        const t = new Date(iso).getTime();
        return PAD_L + ((t - tMin) / tSpan) * PLOT_W;
    }
    function yFor(value: number, domainMax: number): number {
        return PAD_T + PLOT_H - (value / domainMax) * PLOT_H;
    }

    /**
     * Build a polyline `points` string. Straight join — this
     * variant is safe for cpu/rss which have no gap semantics.
     */
    function polyline(
        vals: number[],
        xs: number[],
        domainMax: number,
    ): string {
        return vals
            .map((v, i) => `${xs[i].toFixed(2)},${yFor(v, domainMax).toFixed(2)}`)
            .join(' ');
    }

    /**
     * Build an SVG path with M/L commands, breaking the line at
     * UNMEASURED samples (`M` restart). This is the load-bearing
     * primitive for the VRAM-gap-not-zero invariant: an unmeasured
     * sample is NOT emitted as a segment; the path lifts the pen,
     * and the next measured sample starts a fresh subpath. Result:
     * a visible gap, not a line-across-zero and not an
     * interpolate-across-nulls.
     */
    function vramPath(): string {
        let d = '';
        let penDown = false;
        for (const s of samples) {
            const x = xFor(s.timestamp);
            if (!isMeasured(s)) {
                penDown = false;
                continue;
            }
            const y = yFor(s.vram_mb as number, vramMax);
            d += penDown
                ? ` L ${x.toFixed(2)} ${y.toFixed(2)}`
                : `M ${x.toFixed(2)} ${y.toFixed(2)}`;
            penDown = true;
        }
        return d;
    }

    /**
     * All-unmeasured detection. When the driver was unloaded the
     * whole time this workload ran, the VRAM row shows NOTHING —
     * we surface an explicit legend line instead of a silent empty
     * plot so the operator understands why.
     */
    $: vramHasAnyMeasurement = samples.some(isMeasured);

    $: xs = samples.map((s) => xFor(s.timestamp));

    // Formatters — human-friendly axis end-caps.
    function fmtTime(iso: string): string {
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
    function fmtDuration(msFrom: number, msTo: number): string {
        const s = Math.max(0, Math.round((msTo - msFrom) / 1000));
        const m = Math.floor(s / 60);
        const rem = s % 60;
        return m > 0 ? `${m}m ${rem}s` : `${s}s`;
    }
</script>

<div class="text-sm">
    <div class="flex items-baseline gap-3 mb-2 text-xs">
        <span class="text-fg-muted">
            {samples.length} sample{samples.length === 1 ? '' : 's'} over
            {fmtDuration(tMin, tMax)}
        </span>
        <label class="flex items-center gap-1 cursor-pointer">
            <input type="checkbox" bind:checked={showCpu} />
            <span class="text-healthy">CPU %</span>
        </label>
        <label class="flex items-center gap-1 cursor-pointer">
            <input type="checkbox" bind:checked={showRss} />
            <span class="text-accent">RSS MB</span>
        </label>
        <label class="flex items-center gap-1 cursor-pointer">
            <input type="checkbox" bind:checked={showVram} />
            <span class="text-attention">VRAM MB</span>
        </label>
    </div>

    {#if samples.length === 0}
        <div class="text-fg-muted italic py-4">No samples in this trajectory.</div>
    {:else}
        <svg
            viewBox="0 0 {VB_WIDTH} {VB_HEIGHT}"
            class="w-full h-auto"
            role="img"
            aria-label="Trajectory chart: cpu/rss/vram over the workload's life"
        >
            <!-- Plot frame: baseline + top ceiling. Kept minimal so
                 the polylines stand out against the theme's bg. -->
            <line
                x1={PAD_L} y1={PAD_T + PLOT_H}
                x2={PAD_L + PLOT_W} y2={PAD_T + PLOT_H}
                stroke="rgb(var(--em-muted))" stroke-opacity="0.4" stroke-width="1"
            />
            <line
                x1={PAD_L} y1={PAD_T}
                x2={PAD_L} y2={PAD_T + PLOT_H}
                stroke="rgb(var(--em-muted))" stroke-opacity="0.4" stroke-width="1"
            />

            <!-- X-axis end caps (time labels). Center pieces would
                 need irregular-grid math; end caps are enough for a
                 debugging view. -->
            <text
                x={PAD_L} y={VB_HEIGHT - 6}
                text-anchor="start"
                font-size="10"
                fill="rgb(var(--em-muted))"
            >
                {samples.length > 0 ? fmtTime(samples[0].timestamp) : ''}
            </text>
            <text
                x={PAD_L + PLOT_W} y={VB_HEIGHT - 6}
                text-anchor="end"
                font-size="10"
                fill="rgb(var(--em-muted))"
            >
                {samples.length > 0 ? fmtTime(samples[samples.length - 1].timestamp) : ''}
            </text>

            <!-- Y-axis max labels — one per series. The units are
                 different per series so we can't share; a small "max"
                 tag per line keeps it honest without a legend layer. -->
            {#if showCpu}
                <text
                    x={PAD_L - 4} y={PAD_T + 4}
                    text-anchor="end"
                    font-size="10"
                    fill="rgb(var(--em-healthy))"
                >
                    CPU {cpuMax.toFixed(0)}%
                </text>
            {/if}
            {#if showRss}
                <text
                    x={PAD_L - 4} y={PAD_T + 16}
                    text-anchor="end"
                    font-size="10"
                    fill="rgb(var(--em-accent))"
                >
                    RSS {rssMax}
                </text>
            {/if}
            {#if showVram && vramHasAnyMeasurement}
                <text
                    x={PAD_L - 4} y={PAD_T + 28}
                    text-anchor="end"
                    font-size="10"
                    fill="rgb(var(--em-attention))"
                >
                    VRAM {vramMax}
                </text>
            {/if}

            <!-- Series: CPU %. -->
            {#if showCpu}
                <polyline
                    points={polyline(samples.map((s) => s.cpu_pct), xs, cpuMax)}
                    fill="none"
                    stroke="rgb(var(--em-healthy))"
                    stroke-width="1.5"
                />
            {/if}

            <!-- Series: RSS MB. -->
            {#if showRss}
                <polyline
                    points={polyline(samples.map((s) => s.rss_mb), xs, rssMax)}
                    fill="none"
                    stroke="rgb(var(--em-accent))"
                    stroke-width="1.5"
                />
            {/if}

            <!-- Series: VRAM MB — the honesty line. `vramPath()`
                 breaks the subpath at unmeasured samples so gaps
                 render as VISIBLE breaks, never as an interpolated
                 line-across-null. C4 hard rule. -->
            {#if showVram && vramHasAnyMeasurement}
                <path
                    d={vramPath()}
                    fill="none"
                    stroke="rgb(var(--em-attention))"
                    stroke-width="1.5"
                />
            {/if}
        </svg>

        {#if showVram && !vramHasAnyMeasurement}
            <!-- The all-unmeasured case: SAY IT. An empty vram row
                 with no legend explanation reads as "0 the whole
                 time" — the exact confusion CAR-D93 Q3 exists to
                 prevent. -->
            <div class="text-xs text-fg-muted italic mt-1">
                VRAM: — no measurements for this workload
                (driver unloaded or NVML unavailable this session)
            </div>
        {/if}
    {/if}
</div>
