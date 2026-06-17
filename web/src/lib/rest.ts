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

/**
 * v1.3.2 / DISPATCH 85 — bearer-token auth.
 *
 * The server's `/api/*` routes are gated by a shared bearer token
 * (set in `web.auth_token` server-side). The client stores the
 * token in `sessionStorage` so it survives reloads within the tab
 * but is dropped when the tab closes (intentionally — no `localStorage`
 * so a shared computer doesn't bleed the token across operators).
 *
 * Bootstrap (C3 option (a)): the static bundle loads UNGATED.
 * The first poll attempt either succeeds (token already in
 * sessionStorage from a prior load) OR receives 401, at which point
 * `promptForToken()` opens a browser prompt for the operator. A
 * naked 401 fails VISIBLY — the dashboard panels keep their last
 * good data while the operator enters the token (or the pill flips
 * to 'disconnected' if no prior data exists).
 */
const TOKEN_STORAGE_KEY = 'em_auth_token';

function loadToken(): string | null {
    try {
        return sessionStorage.getItem(TOKEN_STORAGE_KEY);
    } catch {
        // sessionStorage can throw in some iframe / private-mode
        // configurations. The dashboard can still poll without
        // auth if the server's `allow_no_auth = true`.
        return null;
    }
}

function saveToken(token: string): void {
    try {
        sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
    } catch {
        // Storage write failed (quota, private mode). The token
        // is still held in memory below for this tab's lifetime.
    }
    inMemoryToken = token;
}

function clearToken(): void {
    try {
        sessionStorage.removeItem(TOKEN_STORAGE_KEY);
    } catch {
        // ignore
    }
    inMemoryToken = null;
}

let inMemoryToken: string | null = null;

function currentToken(): string | null {
    return inMemoryToken ?? loadToken();
}

/**
 * Open a browser prompt for the bearer token and store it. Called
 * on the first 401 response (token missing or wrong). Returns the
 * entered token, or `null` if the operator cancelled.
 *
 * The naive `window.prompt()` is the SIMPLEST universally-visible
 * surface — no Svelte component churn, no risk of the prompt
 * itself being hidden by a render bug. A future row could replace
 * this with a styled overlay; today's job is "401 fails visibly,
 * not blankly" (the dispatch's C4 hard rule).
 */
function promptForToken(): string | null {
    const entered = window.prompt(
        'edge_monitor: enter the bearer token configured in `[web] auth_token`',
        '',
    );
    if (entered === null || entered === '') {
        return null;
    }
    saveToken(entered);
    return entered;
}

/**
 * Build the `Authorization` header. Returns an empty Headers
 * object when no token is set (the request may still succeed if
 * the server has `allow_no_auth = true`).
 */
function buildAuthHeaders(): HeadersInit {
    const token = currentToken();
    if (!token) {
        return {};
    }
    return { Authorization: `Bearer ${token}` };
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
        let resp = await fetch(snapshotUrl(), {
            signal: controller.signal,
            headers: buildAuthHeaders(),
        });
        // v1.3.2 / DISPATCH 85 — token missing or wrong. Fail
        // VISIBLY (browser prompt), don't blank the dashboard.
        // The current snapshot store stays untouched until the
        // next successful poll, so the operator sees their last
        // good data WHILE entering the new token.
        if (resp.status === 401) {
            clearToken();
            const newToken = promptForToken();
            if (newToken !== null) {
                // Retry once with the freshly-entered token. A
                // second 401 means the operator typed it wrong —
                // they'll see it on the next interval and re-prompt.
                resp = await fetch(snapshotUrl(), {
                    signal: controller.signal,
                    headers: buildAuthHeaders(),
                });
            } else {
                // Operator cancelled the prompt; mark disconnected
                // and let the next interval re-prompt.
                connectionStatus.set('disconnected');
                return;
            }
        }
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

// ─────────────────────────────────────────────────────────────────────
// v1.3.2 / DISPATCH 86 — settings GET/POST wrappers.
//
// THE BOUNDARY (the most important property of this surface):
// the SettingsUpdate type in this file mirrors the Rust handler's
// allowlist EXACTLY. `auto_actuate` and `default_ai_action` are
// READ-ONLY DISPLAY fields ONLY — there is no field in
// `SettingsUpdate` that could be sent in a POST to flip them. Even
// if a future component author added a control by accident, the
// type system would reject the field at compile time.
//
// (Server-side enforcement is the ACTUAL guard — `deny_unknown_fields`
// rejects any extra key including auto_actuate — but the client
// type pin removes the temptation in the first place.)
// ─────────────────────────────────────────────────────────────────────

export interface ThresholdValues {
    thermal_amber_c?: number;
    thermal_red_c?: number;
    vram_attention_pct?: number;
    vram_critical_pct?: number;
    ram_attention_pct?: number;
    ram_critical_pct?: number;
    kv_attention_pct?: number;
    kv_critical_pct?: number;
    alert_sustain_secs?: number;
}

export interface SettingsView {
    thresholds: Required<ThresholdValues>;
    kill_sustain_secs: number;
    auto_actuate_readonly: boolean;
    default_ai_action_readonly: string;
    config_path: string | null;
}

export interface SettingsUpdate {
    thresholds: ThresholdValues;
    kill_sustain_secs?: number;
}

export interface SettingsPostResponse {
    settings: SettingsView;
    persisted: boolean;
}

const SETTINGS_PATH = '/api/settings';

async function fetchWithAuth(input: RequestInfo, init: RequestInit = {}): Promise<Response> {
    const headers = new Headers(init.headers ?? {});
    const token = currentToken();
    if (token) {
        headers.set('Authorization', `Bearer ${token}`);
    }
    let resp = await fetch(input, { ...init, headers });
    if (resp.status === 401) {
        clearToken();
        const newToken = promptForToken();
        if (newToken === null) {
            return resp;
        }
        const retryHeaders = new Headers(init.headers ?? {});
        retryHeaders.set('Authorization', `Bearer ${newToken}`);
        resp = await fetch(input, { ...init, headers: retryHeaders });
    }
    return resp;
}

/**
 * GET /api/settings — fetch the current tunables + read-only
 * boundary fields. Throws on non-2xx.
 */
export async function fetchSettings(): Promise<SettingsView> {
    const resp = await fetchWithAuth(SETTINGS_PATH);
    if (!resp.ok) {
        throw new Error(`fetchSettings: HTTP ${resp.status}`);
    }
    return (await resp.json()) as SettingsView;
}

/**
 * POST /api/settings — write the allowlisted tunables. The handler
 * validates and re-runs the sustain invariant; 400 responses carry
 * the operator-actionable message in the body.
 */
export async function postSettings(
    update: SettingsUpdate,
): Promise<SettingsPostResponse> {
    const resp = await fetchWithAuth(SETTINGS_PATH, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(update),
    });
    if (!resp.ok) {
        const text = await resp.text();
        throw new Error(`postSettings: HTTP ${resp.status} — ${text}`);
    }
    return (await resp.json()) as SettingsPostResponse;
}
