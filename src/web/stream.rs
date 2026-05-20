//! Sprint-6 — WebSocket stream at `/api/stream`.
//!
//! On every TUI tick the runtime publishes a fresh `WireSnapshot`
//! into the shared `tokio::sync::watch` channel; this handler
//! subscribes a per-client receiver and forwards each new value as
//! a JSON text frame.
//!
//! ## Why `watch` and not `broadcast`
//!
//! Dashboard semantics are "latest snapshot wins" — a client that
//! drops connection mid-tick doesn't need to replay missed deltas,
//! just resync on the next tick. `tokio::sync::watch` matches that
//! exactly: one slot, overwritten on each send, every reader sees
//! the latest. Zero backlog, zero unbounded memory growth even if
//! a client stalls.
//!
//! ## Disconnect semantics
//!
//! When the client disconnects the `ws.send()` returns an `Err`,
//! the loop exits, and the per-client task ends. No explicit
//! cleanup needed — the `watch::Receiver` drops with the task and
//! the runtime continues publishing to the rest.

use axum::{
    extract::{State, WebSocketUpgrade, ws::Message, ws::WebSocket},
    response::Response,
};
use futures_util::SinkExt;
use futures_util::stream::StreamExt;

use super::WebState;

/// Axum handler for `GET /api/stream`. Negotiates the WebSocket
/// upgrade and hands the live socket to `pump`.
pub async fn websocket(ws: WebSocketUpgrade, State(state): State<WebState>) -> Response {
    ws.on_upgrade(move |socket| pump(socket, state))
}

/// Per-client subscription loop.
///
/// Sends an immediate "current snapshot" frame so a client that
/// reconnects mid-tick doesn't wait up to `tick_interval_ms` for its
/// first paintable state. Then loops on `rx.changed().await`,
/// forwarding each new value as a JSON text frame.
///
/// Also half-attends to inbound messages so we can clean up
/// promptly when the client closes the socket. We don't actually
/// consume client → server payloads for v1.0 (the UI is read-only),
/// but draining the read half is necessary for the WebSocket to
/// honor the peer's close frame.
async fn pump(socket: WebSocket, state: WebState) {
    let (mut tx, mut rx) = socket.split();

    // Send the current snapshot up front. The watch::Receiver
    // initially reads as "unchanged"; sending the borrowed value
    // here lets the client paint immediately rather than waiting on
    // the next tick.
    let initial = serde_json::to_string(&*state.rx.borrow());
    match initial {
        Ok(json) => {
            if tx.send(Message::Text(json)).await.is_err() {
                return;
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "ws: initial snapshot serialize failed");
            return;
        }
    }

    let mut subscriber = state.rx.clone();
    loop {
        tokio::select! {
            // Server → client: forward each new snapshot.
            change = subscriber.changed() => {
                if change.is_err() {
                    // Sender dropped — runtime shutting down.
                    let _ = tx.send(Message::Close(None)).await;
                    return;
                }
                let json = match serde_json::to_string(&*subscriber.borrow_and_update()) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(error = %err, "ws: snapshot serialize failed");
                        continue;
                    }
                };
                if tx.send(Message::Text(json)).await.is_err() {
                    // Client disconnected mid-send — silent exit.
                    return;
                }
            }
            // Client → server: drain so the read half doesn't block
            // the WS frame state machine. We don't act on any
            // payload — the UI is read-only — but a Close frame
            // here ends the loop cleanly.
            incoming = rx.next() => {
                match incoming {
                    None => return,
                    Some(Err(_)) => return,
                    Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => continue,
                }
            }
        }
    }
}
