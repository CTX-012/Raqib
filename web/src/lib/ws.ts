import { snapshot, connectionStatus } from './stores';
import type { WireSnapshot } from './types';

// Sprint-6 — WebSocket client with auto-reconnect.
//
// One connection per page load, opens on app mount, parks
// indefinitely. On disconnect we wait the backoff interval and
// retry — covers the operator restarting the binary, network
// blips, etc., without forcing a page refresh.

const WS_PATH = '/api/stream';
const RECONNECT_INTERVAL_MS = 2000;

let socket: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

function wsUrl(): string {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    return `${proto}//${window.location.host}${WS_PATH}`;
}

function scheduleReconnect(): void {
    if (reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connect();
    }, RECONNECT_INTERVAL_MS);
}

export function connect(): void {
    if (socket && socket.readyState !== WebSocket.CLOSED) {
        return;
    }
    connectionStatus.set('connecting');
    socket = new WebSocket(wsUrl());

    socket.onopen = (): void => {
        connectionStatus.set('connected');
    };

    socket.onmessage = (event: MessageEvent): void => {
        try {
            const data = JSON.parse(event.data) as WireSnapshot;
            snapshot.set(data);
        } catch (err) {
            console.error('edge_monitor: failed to parse WS frame', err);
        }
    };

    socket.onclose = (): void => {
        connectionStatus.set('disconnected');
        scheduleReconnect();
    };

    socket.onerror = (err): void => {
        console.warn('edge_monitor: WebSocket error', err);
        // onclose will fire after onerror — don't double-schedule.
    };
}

export function disconnect(): void {
    if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
    }
    if (socket) {
        socket.close();
        socket = null;
    }
}
