import { snapshot, connectionStatus } from './stores';
import type { WireSnapshot } from './types';

/**
 * v1.3.2 / DISPATCH 68 — REST polling client.
 *
 * Replaces the v1.0.x WebSocket push transport (`./ws.ts`). The
 * WebSocket route on the server (`/api/stream`) is left in place
 * for backward-compat with any external script that may poll it,
 * but no first-party client opens it anymore. Every web-render
 * regression this session traced to cache / stale-binary /
 * WebSocket buffering; REST `/api/snapshot` is rock-solid (tick
 * monotonic, empirically every poll on the operator's live host).
 *
 * Shape parity: `/api/snapshot` returns the EXACT same
 * `WireSnapshot` JSON the WS route pushed (the snapshot handler
 * at `src/web/handlers.rs::snapshot` reads from the same
 * `watch::Receiver<WireSnapshot>` the WS handler streams from).
 * The store layer (`./stores.ts::snapshot`) sees `.set()` with the
 * same data; every consumer component (VitalsPanel,
 * WorkloadsPanel, ActivityFeed, AlertsPanel) is untouched.
 *
 * ## Loop
 *
 * `connect()` does an immediate fetch (so the first paint isn't
 * delayed by `POLL_INTERVAL_MS`) and then arms a `setInterval`
 * that re-polls. `disconnect()` clears the interval and aborts
 * any in-flight fetch.
 *
 * ## Failure tolerance
 *
 * A failed poll does NOT clear the `snapshot` store — the last
 * good payload stays on screen so a transient blip (operator
 * restarting the binary, brief network blip) doesn't blank the
 * dashboard. `connectionStatus` flips to `'disconnected'` on the
 * failed poll and back to `'connected'` on the next success, so
 * the ConnectionPill reflects reality without losing the last
 * good frame.
 *
 * ## In-flight guard
 *
 * If a fetch is still pending when the interval fires, we skip
 * the new poll. Prevents request pile-up on a slow server
 * without dropping correctness (the next interval tick will
 * still pick up where we left off).
 */

const SNAPSHOT_PATH = '/api/snapshot';

/**
 * Default polling cadence. Matches the tick interval on the
 * server side (1 Hz default), so we're not undersampling. The
 * server's render uses the watch-channel's most-recent value, so
 * occasional re-polls within a tick are cheap (they re-deliver
 * the same snapshot; the store's writable detects no shallow
 * change and most consumers no-op).
 *
 * Exposed as a const for adjusting in dev / for a future
 * `--web-poll-ms` config knob.
 */
export const POLL_INTERVAL_MS = 1000;

let intervalHandle: ReturnType<typeof setInterval> | null = null;
let inFlight: AbortController | null = null;
let firstFetchSent = false;

function snapshotUrl(): string {
    // Same-origin fetch — the dashboard is served from the same
    // host:port as the API, so a relative path is safest (no
    // CORS surface, no proto/port mismatch).
    return SNAPSHOT_PATH;
}

async function pollOnce(): Promise<void> {
    if (inFlight) {
        // A previous poll is still pending — skip this tick to
        // avoid pile-up. The next interval fire will try again;
        // the watch-channel-based snapshot endpoint is fast
        // enough that this branch should be rare.
        return;
    }
    const controller = new AbortController();
    inFlight = controller;
    try {
        const resp = await fetch(snapshotUrl(), { signal: controller.signal });
        if (!resp.ok) {
            throw new Error(`HTTP ${resp.status}`);
        }
        const data = (await resp.json()) as WireSnapshot;
        snapshot.set(data);
        connectionStatus.set('connected');
    } catch (err) {
        // AbortError fires when disconnect() cancels the fetch.
        // That's a clean unmount, not a real error — don't flip
        // connectionStatus or warn the operator.
        if (err instanceof Error && err.name === 'AbortError') {
            return;
        }
        // Any other failure (network down, server restarting,
        // 5xx): keep the last good snapshot on screen, mark the
        // connection offline so the pill tells the truth. Next
        // successful poll recovers automatically.
        connectionStatus.set('disconnected');
        console.warn('edge_monitor: snapshot poll failed', err);
    } finally {
        if (inFlight === controller) {
            inFlight = null;
        }
    }
}

/**
 * Start polling `/api/snapshot` and publishing each successful
 * payload to the `snapshot` store. Safe to call repeatedly:
 * already-armed loops are detected and the call is a no-op.
 */
export function connect(): void {
    if (intervalHandle) {
        // Already polling — idempotent.
        return;
    }
    connectionStatus.set('connecting');
    if (!firstFetchSent) {
        firstFetchSent = true;
        // Kick the first fetch immediately so the dashboard
        // paints on the first ~RTT rather than waiting a full
        // interval. The interval below picks up from there.
        void pollOnce();
    }
    intervalHandle = setInterval(() => {
        void pollOnce();
    }, POLL_INTERVAL_MS);
}

/**
 * Stop the polling loop and cancel any in-flight fetch. Matches
 * the `disconnect()` shape that `./ws.ts` exported so
 * `App.svelte`'s `onDestroy` doesn't need restructuring across
 * the transport swap.
 */
export function disconnect(): void {
    if (intervalHandle) {
        clearInterval(intervalHandle);
        intervalHandle = null;
    }
    if (inFlight) {
        inFlight.abort();
        inFlight = null;
    }
    // Reset the immediate-fetch latch so a subsequent connect()
    // after disconnect (e.g. an HMR reload in dev) still gets a
    // kick fetch.
    firstFetchSent = false;
}
