import { writable, type Writable } from 'svelte/store';
import { EMPTY_SNAPSHOT, type WireSnapshot } from './types';

// Sprint-6 — Svelte stores hold the latest dashboard state.
//
// v1.3.2 / DISPATCH 68: the REST polling client (`rest.ts`) writes
// into `snapshot`. Pre-v1.3.2 the WebSocket client (`ws.ts`) wrote
// here instead — the transport swap is invisible at this layer
// because both writers `.set()` the same `WireSnapshot` shape.
// Components subscribe and re-render whenever the store updates.
// Theme + connection state have their own stores so they're cheap
// to update independently of the per-tick snapshot.

export const snapshot: Writable<WireSnapshot> = writable(EMPTY_SNAPSHOT);

export type ConnectionStatus = 'connecting' | 'connected' | 'disconnected';
export const connectionStatus: Writable<ConnectionStatus> = writable('connecting');

export type ThemeName = 'dark' | 'light' | 'hc';
export const theme: Writable<ThemeName> = writable('dark');

/** Update the <body> class to apply the active theme palette. */
theme.subscribe((t) => {
    if (typeof document === 'undefined') return;
    document.body.classList.remove('theme-dark', 'theme-light', 'theme-hc');
    document.body.classList.add(`theme-${t}`);
});

// ─────────────────────────────────────────────────────────────────────
// v1.3.2 / DISPATCH 100 / PHASE 5 display modes step 1 — mode store.
//
// Five modes per PHASE5_DISPLAY_MODES_DESIGN.md §1.2 (operator-
// ratified 2026-07-08): Dashboard (default) / Focus / Timeline /
// Kiosk / History. Selection persists via the URL query param
// `?mode=X` (`+ &pid=N` for Focus) — matches §2's shareable /
// refresh-safe / no-localStorage decision.
//
// Step 1 ships DORMANT: `dashboard` renders today's page verbatim;
// the other four are "coming soon" placeholders. The reactive
// routing is here; the real views land in steps 2-5.
// ─────────────────────────────────────────────────────────────────────

export type ModeName = 'dashboard' | 'focus' | 'timeline' | 'kiosk' | 'history';

/** Ordered list of valid modes — the header dropdown iterates this. */
export const MODES: readonly ModeName[] = [
    'dashboard',
    'focus',
    'timeline',
    'kiosk',
    'history',
] as const;

/**
 * Coerce an arbitrary string to a valid `ModeName`, falling back to
 * `'dashboard'`. Mirrors the theme store's forgiving pattern at
 * `App.svelte:24-26`: an unrecognized value is silently normalized,
 * never blanks the page or throws.
 */
export function coerceMode(raw: string | null | undefined): ModeName {
    return (MODES as readonly string[]).includes(raw ?? '')
        ? (raw as ModeName)
        : 'dashboard';
}

/** Selected mode. Default `dashboard` (today's page verbatim). */
export const mode: Writable<ModeName> = writable('dashboard');

/**
 * Focused PID for Focus mode. `null` when unset. Parsed from
 * `?pid=N`; only Focus mode reads it, but the store lives at the
 * app level so URL round-tripping is unified.
 */
export const focusPid: Writable<number | null> = writable(null);
