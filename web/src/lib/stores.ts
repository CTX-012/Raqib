import { writable, type Writable } from 'svelte/store';
import { EMPTY_SNAPSHOT, type WireSnapshot } from './types';

// Sprint-6 — Svelte stores hold the latest dashboard state. The
// WebSocket client (`ws.ts`) writes into `snapshot`; components
// subscribe and re-render on each push. Theme + connection state
// have their own stores so they're cheap to update independently of
// the per-tick snapshot.

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
