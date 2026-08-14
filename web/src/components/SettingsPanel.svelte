<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 86 — settings panel.
     *
     * THE BOUNDARY (the most important property of this component):
     * the editable controls bind ONLY to the tunable fields on
     * `SettingsView.thresholds` and `kill_sustain_secs`. The
     * `auto_actuate_readonly` and `default_ai_action_readonly`
     * fields render as STATIC TEXT — there is no `<input bind:value>`
     * or toggle on them. This component DISPLAYS state; it does NOT
     * offer the control.
     *
     * Server-side enforcement is the actual guard (the Rust handler
     * uses `serde(deny_unknown_fields)` so any crafted POST with
     * `auto_actuate` is rejected at deserialization). This component
     * is the operator-side honesty surface: it shows what's armed,
     * but only the TOML + restart can change it.
     */

    import { onMount } from 'svelte';
    import {
        fetchSettings,
        postSettings,
        type SettingsView,
        type ThresholdValues,
    } from '../lib/rest';

    let view: SettingsView | null = null;
    let loadError: string | null = null;
    let saving = false;
    let saveError: string | null = null;
    let saveOk: string | null = null;
    let expanded = false;

    // A4 SAFETY-UX — when the governor is ARMED, threshold changes
    // take effect on the next tick (SharedTunables is hot-updated),
    // meaning a slider dragged 95%→30% can immediately start firing
    // kills against real processes. The Save button is disabled
    // until the operator explicitly ticks `armedAck` — this is a
    // speed-bump against accidental live threshold edits, not a
    // security control (a scripted client can POST directly). The
    // backend also logs every POST with an `armed=true|false` field
    // so the audit trail exists regardless of UI path.
    let armedAck = false;

    // Local form state — bound to inputs. The values mirror
    // SettingsView.thresholds + kill_sustain_secs ONLY; no other
    // field is exposed (boundary).
    let form: {
        thresholds: ThresholdValues;
        kill_sustain_secs: number;
    } = {
        thresholds: {},
        kill_sustain_secs: 10,
    };

    // Save is blocked when the governor is armed and the operator
    // hasn't acknowledged the live-edit hazard. Disarmed → no gate.
    $: saveBlocked =
        saving ||
        (!!view && view.auto_actuate_readonly && !armedAck);

    onMount(async () => {
        await refresh();
    });

    async function refresh(): Promise<void> {
        loadError = null;
        try {
            view = await fetchSettings();
            form = {
                thresholds: { ...view.thresholds },
                kill_sustain_secs: view.kill_sustain_secs,
            };
        } catch (err) {
            loadError = (err as Error).message ?? String(err);
        }
    }

    async function save(): Promise<void> {
        if (saving) return;
        saving = true;
        saveError = null;
        saveOk = null;
        try {
            const resp = await postSettings({
                thresholds: form.thresholds,
                kill_sustain_secs: form.kill_sustain_secs,
            });
            view = resp.settings;
            saveOk = resp.persisted
                ? 'Saved (persisted to config file).'
                : 'Saved (running config only — no config file path; restart will lose changes).';
        } catch (err) {
            saveError = (err as Error).message ?? String(err);
        } finally {
            saving = false;
        }
    }
</script>

<section class="settings-panel" aria-label="Settings">
    <button
        type="button"
        class="settings-toggle"
        on:click={() => (expanded = !expanded)}
        aria-expanded={expanded}
    >
        Settings {expanded ? '▾' : '▸'}
    </button>

    {#if expanded}
        <div class="settings-body">
            {#if loadError}
                <div class="error" data-testid="settings-load-error">
                    Failed to load settings: {loadError}
                </div>
            {/if}

            {#if view}
                <!--
                    BOUNDARY DISPLAY — read-only status of auto-kill +
                    policy action. No input control here, on purpose.
                    The text explicitly tells the operator where to
                    change these.
                -->
                <div class="readonly-block" aria-label="Read-only status" data-testid="settings-loaded">
                    <div class="readonly-row">
                        <span class="label">Auto-actuate (autonomous kills):</span>
                        <span
                            class="value"
                            class:on={view.auto_actuate_readonly}
                            class:off={!view.auto_actuate_readonly}
                        >
                            {view.auto_actuate_readonly ? 'ON' : 'OFF'}
                        </span>
                    </div>
                    <div class="readonly-row">
                        <span class="label">Default AI policy action:</span>
                        <span class="value">{view.default_ai_action_readonly}</span>
                    </div>
                    <p class="hint">
                        These are read-only here. Edit
                        <code>[governor].auto_actuate</code> and
                        <code>[policy].default_ai_action</code> in
                        <code>{view.config_path ?? 'your config file'}</code>
                        then restart.
                    </p>
                </div>

                <!--
                    A4 SAFETY-UX — armed-state warning banner. Only
                    renders when the governor is armed (i.e. the
                    operator has already flipped auto_actuate=true
                    via the TOML + restart path). The banner + the
                    checkbox gate on Save are the operator-side
                    speed-bump against accidentally editing kill-
                    trigger thresholds live. Backend logging still
                    catches every POST regardless of what the UI
                    surfaced — this is UX, not the security control.
                -->
                {#if view.auto_actuate_readonly}
                    <div
                        class="armed-banner"
                        role="alert"
                        data-testid="settings-armed-banner"
                    >
                        <div class="armed-title">
                            <span aria-hidden="true">⚠</span>
                            Governor is ARMED
                        </div>
                        <p class="armed-body">
                            Threshold changes take effect on the next
                            tick and may immediately trigger kills against
                            live processes.
                        </p>
                        <label class="armed-ack">
                            <input
                                type="checkbox"
                                bind:checked={armedAck}
                                data-testid="settings-armed-ack"
                            />
                            I understand — apply this change to a
                            live, armed governor.
                        </label>
                    </div>
                {/if}

                <fieldset class="tunables" disabled={saving}>
                    <legend>Editable thresholds &amp; sustain windows</legend>

                    <label>
                        <span>VRAM critical %</span>
                        <input
                            type="number"
                            step="0.1"
                            min="0"
                            max="100"
                            bind:value={form.thresholds.vram_critical_pct}
                        />
                    </label>
                    <label>
                        <span>VRAM attention %</span>
                        <input
                            type="number"
                            step="0.1"
                            min="0"
                            max="100"
                            bind:value={form.thresholds.vram_attention_pct}
                        />
                    </label>
                    <label>
                        <span>RAM critical %</span>
                        <input
                            type="number"
                            step="0.1"
                            min="0"
                            max="100"
                            bind:value={form.thresholds.ram_critical_pct}
                        />
                    </label>
                    <label>
                        <span>Thermal red °C</span>
                        <input
                            type="number"
                            step="0.1"
                            bind:value={form.thresholds.thermal_red_c}
                        />
                    </label>
                    <label>
                        <span>Alert sustain (s)</span>
                        <input
                            type="number"
                            step="1"
                            min="1"
                            max="600"
                            bind:value={form.thresholds.alert_sustain_secs}
                        />
                    </label>
                    <label>
                        <span>Kill sustain (s)</span>
                        <input
                            type="number"
                            step="1"
                            min="1"
                            bind:value={form.kill_sustain_secs}
                        />
                    </label>

                    <div class="actions">
                        <button
                            type="button"
                            on:click={save}
                            disabled={saveBlocked}
                            data-testid="settings-save"
                            title={view.auto_actuate_readonly && !armedAck
                                ? 'Governor is armed — tick the acknowledgement above to enable Save.'
                                : ''}
                        >
                            {saving ? 'Saving…' : 'Save'}
                        </button>
                        <button type="button" on:click={refresh} disabled={saving}>
                            Reload
                        </button>
                    </div>
                    {#if saveOk}
                        <div class="ok">{saveOk}</div>
                    {/if}
                    {#if saveError}
                        <div class="error">{saveError}</div>
                    {/if}
                </fieldset>
            {:else if !loadError}
                <!--
                    Loading state — before the D-hardening fix this
                    branch didn't exist, so a `view === null &&
                    loadError === null` state (fetch pending, or a
                    silently-swallowed rejection) rendered the body
                    as VISUALLY BLANK. Operators reported it as
                    "settings is empty," which was ambiguous between
                    "loading" and "broken." Now the panel is always
                    in a legible state: loading / error / loaded.
                -->
                <div class="loading" data-testid="settings-loading">
                    Loading settings…
                </div>
            {/if}
        </div>
    {/if}
</section>

<style>
    .settings-panel {
        margin: 0.5rem 0;
        font-size: 0.875rem;
    }
    .settings-toggle {
        background: transparent;
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        padding: 0.2rem 0.6rem;
        color: rgb(var(--em-fg));
        cursor: pointer;
    }
    .settings-toggle:hover {
        background: rgb(var(--em-muted) / 0.1);
    }
    .settings-body {
        margin-top: 0.6rem;
        padding: 0.8rem;
        border: 1px solid rgb(var(--em-muted) / 0.3);
        border-radius: 4px;
    }
    .readonly-block {
        margin-bottom: 0.8rem;
        padding-bottom: 0.6rem;
        border-bottom: 1px dashed rgb(var(--em-muted) / 0.3);
    }
    .readonly-row {
        display: flex;
        gap: 0.6rem;
        margin: 0.15rem 0;
    }
    .readonly-row .label {
        color: rgb(var(--em-muted));
    }
    .readonly-row .value {
        font-weight: bold;
    }
    .readonly-row .value.on {
        color: rgb(var(--em-critical));
    }
    .readonly-row .value.off {
        color: rgb(var(--em-fg-muted, var(--em-muted)));
    }
    .hint {
        margin: 0.4rem 0 0;
        color: rgb(var(--em-muted));
        font-size: 0.8rem;
    }
    .hint code {
        background: rgb(var(--em-muted) / 0.15);
        padding: 0 0.25rem;
        border-radius: 2px;
    }
    /* A4 armed-state warning banner. Loud but not screaming —
       distinguishable from the everyday `.error` red so operators
       don't ignore it as "another form error." */
    .armed-banner {
        margin: 0 0 0.8rem;
        padding: 0.6rem 0.75rem;
        border: 2px solid rgb(var(--em-critical));
        border-radius: 4px;
        background: rgb(var(--em-critical) / 0.12);
        color: rgb(var(--em-fg));
    }
    .armed-title {
        font-weight: bold;
        color: rgb(var(--em-critical));
        display: flex;
        gap: 0.4rem;
        align-items: baseline;
        font-size: 0.9rem;
    }
    .armed-body {
        margin: 0.35rem 0 0.5rem;
        font-size: 0.85rem;
    }
    .armed-ack {
        display: flex;
        gap: 0.4rem;
        align-items: baseline;
        font-size: 0.85rem;
        cursor: pointer;
    }
    .armed-ack input {
        margin: 0;
    }

    .tunables {
        border: none;
        padding: 0;
        margin: 0;
        display: grid;
        gap: 0.4rem 1rem;
        grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .tunables legend {
        grid-column: 1 / -1;
        padding: 0;
        color: rgb(var(--em-muted));
        font-size: 0.8rem;
        margin-bottom: 0.3rem;
    }
    .tunables label {
        display: flex;
        flex-direction: column;
        gap: 0.15rem;
    }
    .tunables input {
        padding: 0.2rem 0.4rem;
        background: rgb(var(--em-bg) / 0.5);
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        color: rgb(var(--em-fg));
    }
    .actions {
        grid-column: 1 / -1;
        display: flex;
        gap: 0.5rem;
        margin-top: 0.5rem;
    }
    .actions button {
        padding: 0.3rem 0.8rem;
        background: rgb(var(--em-muted) / 0.15);
        border: 1px solid rgb(var(--em-muted) / 0.4);
        border-radius: 3px;
        color: rgb(var(--em-fg));
        cursor: pointer;
    }
    .actions button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    .ok {
        grid-column: 1 / -1;
        color: rgb(var(--em-ok, var(--em-fg)));
        margin-top: 0.4rem;
    }
    .error {
        grid-column: 1 / -1;
        color: rgb(var(--em-critical));
        margin-top: 0.4rem;
    }
    .loading {
        color: rgb(var(--em-muted));
        font-style: italic;
        padding: 0.2rem 0;
    }
</style>
