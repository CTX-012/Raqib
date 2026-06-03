<script lang="ts">
    /*
     * v1.1.13 / DISPATCH 42 — render the wire's currently-visible
     * alerts. Closes the v1.1.11 deferral (headless logs got alerts;
     * the web dashboard did not).
     *
     * Design: the wire pre-classifies severity (`'attention' |
     * 'critical'`) and pre-renders the alert text via the same
     * `ux_contract::alerts::*` template + `substitute(...)` pipeline
     * the TUI banner uses. This component does NOT do template
     * substitution; it ONLY maps the severity literal to a tailwind
     * class. Single source of truth: ux_contract.
     *
     * Hidden when `alerts` is empty or absent (a pre-v1.1.13 server
     * doesn't emit the field; the additive-default guarantee in
     * `WireSnapshot.alerts` handles that gracefully).
     */
    import type { WireAlertEntry } from '../lib/types';
    export let alerts: WireAlertEntry[] | undefined;

    /** Map the server-classified severity to a tailwind background.
     *  Matches the TUI's §14 banner colors: Attention → amber,
     *  Critical → red. DO NOT introduce numeric thresholds here —
     *  the contract is the single source of truth and the server
     *  has already done the classification. */
    function severityBg(severity: WireAlertEntry['severity']): string {
        switch (severity) {
            case 'critical':
                return 'bg-critical';
            case 'attention':
            default:
                return 'bg-attention';
        }
    }

    // `alerts` may be undefined on a pre-v1.1.13 server payload
    // (the optional field). Normalize to an empty list so the
    // template renders deterministically.
    $: visible = alerts ?? [];
</script>

{#if visible.length > 0}
    <section
        class="rounded border border-fg-muted/30 p-4 bg-bg-raised/40"
        aria-label="Alerts"
    >
        <h2 class="text-fg-muted text-sm font-bold mb-3">Alerts</h2>
        <ul class="space-y-2">
            {#each visible as alert (`${alert.alert_id}-${alert.pid ?? 'system'}`)}
                <li>
                    <div
                        class="px-3 py-1 rounded {severityBg(alert.severity)} text-bg font-bold text-sm"
                        title="alert id: {alert.alert_id}"
                    >
                        {alert.text}
                    </div>
                </li>
            {/each}
        </ul>
    </section>
{/if}
