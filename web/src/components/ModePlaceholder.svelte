<script lang="ts">
    /*
     * v1.3.2 / DISPATCH 100 / PHASE 5 display modes step 1 —
     * placeholder view for modes whose real view lands in a later
     * dispatch. Renders a centered "coming soon" panel.
     *
     * PHASE5_DISPLAY_MODES_DESIGN.md §7 step 1 ships the switch
     * DORMANT: only Dashboard is real; Focus / Timeline / Kiosk /
     * History surface this placeholder while their views land
     * across steps 2-5. Step 1's revert-safety property depends on
     * this being a stub — no logic, no data fetches, no
     * stateful behavior that a rollback would have to unwind.
     */
    import type { ModeName } from '../lib/stores';
    export let mode: ModeName;
    export let step: string;

    const TITLE: Record<ModeName, string> = {
        dashboard: 'Dashboard',
        focus: 'Focus',
        timeline: 'Timeline',
        kiosk: 'Kiosk',
        history: 'History',
    };
</script>

<div
    class="min-h-[40vh] flex flex-col items-center justify-center text-center px-6 py-10"
    data-testid="mode-placeholder"
    data-testid-mode={mode}
>
    <div class="text-fg-muted text-xs uppercase tracking-wider mb-2">
        Coming soon
    </div>
    <h2 class="text-fg text-2xl font-bold mb-2">
        {TITLE[mode]} view
    </h2>
    <p class="text-fg-muted text-sm max-w-md">
        This view lands in {step} of the display-modes build sequence
        (see docs/PHASE5_DISPLAY_MODES_DESIGN.md §7). The switch
        mechanism is live; the view itself is not yet implemented.
    </p>
    <p class="text-fg-muted text-xs mt-4">
        Return to <a class="text-accent underline" href="?mode=dashboard">Dashboard</a>.
    </p>
</div>
