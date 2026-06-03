<script lang="ts">
    /*
     * v1.2.0 / DISPATCH 45 — render one recommendation under its
     * associated alert. Reads the server-pre-classified severity
     * and the server-rendered label string verbatim; the only
     * thing this component does is map the severity literal to a
     * tailwind class and lay the label + reason out.
     *
     * AUTHORITY LOCK: DISPLAY ONLY. This component renders text
     * the user reads. There is NO click handler, NO action button,
     * NO kill flow. The contract enforces "discriminator, not
     * callable" at the type level (SuggestedAction is Copy); this
     * component honours it by simply not having an `on:click` or
     * any onSubmit. The user acts via the existing TUI `k` flow
     * — the disclaimer at the section top reminds them.
     */
    import type { WireRecommendation } from '../lib/types';
    export let rec: WireRecommendation;

    function severityFg(severity: WireRecommendation['severity']): string {
        switch (severity) {
            case 'critical':
                return 'text-critical';
            case 'warning':
                return 'text-attention';
            case 'info':
            default:
                return 'text-fg';
        }
    }
</script>

<div
    class="px-3 py-1 rounded border border-fg-muted/30 bg-bg-raised/40 text-sm"
    title="alert id: {rec.alert_id}, action: {rec.action}"
>
    <div class="font-semibold {severityFg(rec.severity)}">
        {rec.label}
    </div>
    {#if rec.reason}
        <div class="text-xs text-fg-muted mt-0.5">
            {rec.reason}
        </div>
    {/if}
</div>
