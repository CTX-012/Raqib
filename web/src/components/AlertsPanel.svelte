<script lang="ts">
    /*
     * v1.1.13 / DISPATCH 42 — render the wire's currently-visible
     * alerts. Closes the v1.1.11 deferral (headless logs got alerts;
     * the web dashboard did not).
     *
     * v1.2.0 / DISPATCH 45 — also render the recommendation
     * projection. Recs sit under their associated alert; a single
     * disclaimer at the section top reminds the operator that
     * recommendations are SUGGESTIONS — the action is taken via
     * the TUI `k` keybinding flow.
     *
     * AUTHORITY LOCK: display only. The wire pre-classifies
     * severity (`'attention' | 'critical'` for alerts, `'warning' |
     * 'critical' | 'info'` for recs) and pre-renders the label
     * text. This component does NOT do template substitution; it
     * ONLY maps the severity literal to a tailwind class. There
     * is NO action button, NO click handler. Single source of
     * truth: ux_contract.
     *
     * Hidden when both `alerts` and `recommendations` are empty.
     */
    import type { WireAlertEntry, WireRecommendation } from '../lib/types';
    import RecommendationCard from './RecommendationCard.svelte';
    export let alerts: WireAlertEntry[] | undefined;
    export let recommendations: WireRecommendation[] | undefined;

    // v1.2.0 / DISPATCH 45 — the once-per-section disclaimer is
    // operator-locked at the contract level. The string lives at
    // `ux_contract::recommendation::display::RECOMMENDATION_NOT_ACTIONABLE`
    // (Rust) and is mirrored verbatim here so the TUI and web
    // render the IDENTICAL text. If the contract bumps this
    // string, this constant must follow.
    const RECOMMENDATION_NOT_ACTIONABLE =
        'Suggestion only — press k to act manually';

    function severityBg(severity: WireAlertEntry['severity']): string {
        switch (severity) {
            case 'critical':
                return 'bg-critical';
            case 'attention':
            default:
                return 'bg-attention';
        }
    }

    $: visibleAlerts = alerts ?? [];
    $: visibleRecs = recommendations ?? [];

    // Group recs by their underlying alert_id so each rec renders
    // under the alert it derives from. Some alerts (GovernorArmed,
    // WorkloadExited) have no rec (suppressed by the projection);
    // those alerts render alone.
    $: recsByAlertId = (() => {
        const m = new Map<string, WireRecommendation[]>();
        for (const r of visibleRecs) {
            const list = m.get(r.alert_id) ?? [];
            list.push(r);
            m.set(r.alert_id, list);
        }
        return m;
    })();
</script>

{#if visibleAlerts.length > 0 || visibleRecs.length > 0}
    <section
        class="rounded border border-fg-muted/30 p-4 bg-bg-raised/40"
        aria-label="Alerts"
    >
        <h2 class="text-fg-muted text-sm font-bold mb-3">Alerts</h2>

        <ul class="space-y-2">
            {#each visibleAlerts as alert (`${alert.alert_id}-${alert.pid ?? 'system'}`)}
                <li>
                    <div
                        class="px-3 py-1 rounded {severityBg(alert.severity)} text-bg font-bold text-sm"
                        title="alert id: {alert.alert_id}"
                    >
                        {alert.text}
                    </div>
                    {#if recsByAlertId.get(alert.alert_id)?.length}
                        <ul class="mt-1 ml-3 space-y-1">
                            {#each recsByAlertId.get(alert.alert_id) ?? [] as rec, idx (`${alert.alert_id}-rec-${idx}`)}
                                <li>
                                    <RecommendationCard {rec} />
                                </li>
                            {/each}
                        </ul>
                    {/if}
                </li>
            {/each}
        </ul>

        {#if visibleRecs.length > 0}
            <!-- v1.2.0 / DISPATCH 45 — operator-locked: render the
                 disclaimer ONCE at the bottom of the rec section,
                 not per-rec. -->
            <div class="mt-3 pt-2 border-t border-fg-muted/20 text-xs text-fg-muted italic">
                {RECOMMENDATION_NOT_ACTIONABLE}
            </div>
        {/if}
    </section>
{/if}
