// v1.3.2 / DISPATCH 100 / PHASE 5 display modes step 1 — URL sync
// for the mode store.
//
// PHASE5_DISPLAY_MODES_DESIGN.md §2.2 pinned URL query param over
// sessionStorage / a router library:
//   * shareable + refresh-safe (bookmark `?mode=kiosk` for a wall
//     monitor)
//   * matches D68's no-localStorage precedent
//   * ~30 lines of Svelte reactivity, zero new dependencies
//
// This module is the ~30 lines. It runs once at App mount and
// wires:
//   URL   →  store   (initial parse + browser back/forward via popstate)
//   store →  URL     (pushState on mode change; ?mode + ?pid + preserved
//                     other params)

import { get } from 'svelte/store';
import { coerceMode, focusPid, mode, type ModeName } from './stores';

/**
 * Parse the current URL and seed the stores. Called once at mount.
 * Absent / unknown mode → `dashboard` (mirrors the theme store's
 * forgiving pattern at App.svelte:24-26).
 */
function readFromUrl(): void {
    if (typeof window === 'undefined') return;
    const params = new URLSearchParams(window.location.search);
    const parsedMode = coerceMode(params.get('mode'));
    const rawPid = params.get('pid');
    let parsedPid: number | null = null;
    if (rawPid !== null) {
        const n = Number.parseInt(rawPid, 10);
        // Reject NaN and negative PIDs — the URL is untrusted input.
        // A garbage `?pid=lol` behaves like no pid was set.
        if (Number.isInteger(n) && n > 0) parsedPid = n;
    }
    mode.set(parsedMode);
    focusPid.set(parsedPid);
}

/**
 * Write the current stores back into the URL via `history.pushState`.
 * Preserves any URL params we DON'T own (theme override links,
 * hypothetical `?token=...` deep-links, `?interval=...`, etc.) —
 * only `mode` and `pid` are touched.
 *
 * `pushState` (not `replaceState`) so browser back/forward navigate
 * through mode history — matches the design's back/forward story.
 */
function writeToUrl(nextMode: ModeName, nextPid: number | null): void {
    if (typeof window === 'undefined') return;
    const url = new URL(window.location.href);
    if (nextMode === 'dashboard') {
        // Dashboard is the default; omit `?mode=dashboard` so the
        // canonical dashboard URL stays clean. Bookmarking / sharing
        // reads more naturally without the redundant param.
        url.searchParams.delete('mode');
    } else {
        url.searchParams.set('mode', nextMode);
    }
    if (nextMode === 'focus' && nextPid !== null) {
        url.searchParams.set('pid', String(nextPid));
    } else {
        // Non-Focus modes: drop the pid param. Keeping it around
        // would leak state across mode switches (e.g. flipping to
        // Kiosk should not carry a stale `?pid=42`).
        url.searchParams.delete('pid');
    }
    const next = url.pathname + (url.search || '') + url.hash;
    // Only push a new history entry if the URL actually changed;
    // otherwise a rapid subscribe fire (Svelte writable can double-
    // fire on identical set) would spam history with duplicates.
    const current = window.location.pathname + window.location.search + window.location.hash;
    if (next !== current) {
        window.history.pushState({}, '', next);
    }
}

/**
 * Wire the two-way URL ↔ store sync. Idempotent: safe to call
 * repeatedly (later calls no-op after the first). Returns a
 * cleanup fn the caller can invoke on unmount.
 */
let installed = false;
let cleanup: (() => void) | null = null;
export function installModeUrlSync(): () => void {
    if (installed) return cleanup ?? (() => {});
    installed = true;

    // 1. Seed stores from the URL on mount.
    readFromUrl();

    // 2. Any store change → push to URL. `get(...)` at write time
    //    picks up the latest values without a fresh subscribe.
    const unsubMode = mode.subscribe((m) => {
        writeToUrl(m, get(focusPid));
    });
    const unsubPid = focusPid.subscribe((p) => {
        writeToUrl(get(mode), p);
    });

    // 3. Browser back/forward → re-seed stores. Guard against the
    //    subscribe callbacks above firing back into pushState by
    //    checking whether the value actually changed before calling
    //    `.set()` (writable de-dupes but is defensive against the
    //    URL→store→URL→URL echo).
    const onPop = (): void => {
        readFromUrl();
    };
    window.addEventListener('popstate', onPop);

    cleanup = () => {
        unsubMode();
        unsubPid();
        window.removeEventListener('popstate', onPop);
        installed = false;
    };
    return cleanup;
}
