//! Sprint-6 — `edge_monitor` web companion.
//!
//! The web UI is a Svelte SPA embedded into the binary via
//! `rust-embed`, served by an `axum` router on a tokio runtime that
//! lives alongside the TUI's event loop. The TUI is authoritative for
//! control (kill_confirm card, theme selection, navigation); the web
//! UI is read-only for v1.0.
//!
//! ## Architecture
//!
//! ```text
//!   ┌────────────────┐   tick    ┌──────────────────┐   ws    ┌─────────┐
//!   │ TUI thread     │──────────▶│ tokio::watch     │────────▶│ axum WS │
//!   │ (Runtime owner)│  publish  │ <WireSnapshot>   │ subscribe│ clients │
//!   └────────────────┘           └──────────────────┘         └─────────┘
//!                                       │
//!                                       │ borrow()
//!                                       ▼
//!                                  REST handlers
//! ```
//!
//! The TUI loop publishes a fresh `WireSnapshot` on every tick. The
//! shared `tokio::sync::watch` channel holds the latest one;
//! REST handlers borrow it for one-shot polls, the WS handler
//! subscribes to changes for live deltas.
//!
//! ## Why `watch`, not `Arc<RwLock<Runtime>>`
//!
//! Pattern (c) from the Sprint-6 dispatch. `watch` gives us:
//!
//!   - Zero Runtime refactor — the existing TUI loop owns the
//!     Runtime as before; the only change is a `tx.send(snapshot)`
//!     call after each tick.
//!   - Latest-wins semantics — clients that reconnect mid-tick
//!     never replay stale ticks; they sync on the current value.
//!   - No backlog — a stalled client doesn't grow memory.
//!
//! ## Wire schema
//!
//! Locked at v0.1 in `wire.rs`. Future changes need contract
//! consideration because the v2 / Altara companion (separate repo)
//! consumes the same JSON.

pub mod assets;
pub mod handlers;
pub mod stream;
pub mod wire;

use std::net::SocketAddr;

use axum::{
    Router,
    routing::{any, get},
};
use tokio::sync::watch;
use tower_http::cors::{Any, CorsLayer};

pub use wire::WireSnapshot;

/// Shared axum router state — every handler gets a clone of this.
/// `watch::Receiver` is cheap to clone (it's just an Arc internally),
/// so per-request clones are fine.
#[derive(Clone)]
pub struct WebState {
    pub rx: watch::Receiver<WireSnapshot>,
}

/// Construct the axum router. Exposed so integration tests can
/// drive the routes without binding a real socket.
pub fn router(state: WebState) -> Router {
    // CORS: locked to "any origin" because the server binds to
    // localhost-only by default; CORS isn't a security boundary
    // here, just a convenience for browser tooling. If a future
    // row exposes the server on a real network interface, this
    // policy needs tightening.
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any);
    Router::new()
        .route("/", get(handlers::index))
        .route("/assets/*path", get(handlers::serve_asset))
        .route("/api/health", get(handlers::health))
        .route("/api/snapshot", get(handlers::snapshot))
        .route("/api/stream", any(stream::websocket))
        .layer(cors)
        .with_state(state)
}

/// Bind axum to `addr` and serve until `shutdown` resolves. Caller
/// is responsible for spawning this on a tokio runtime. Returns the
/// bound port so the caller can log it (useful when port 0 is
/// requested for ephemeral binding in tests).
pub async fn serve(
    addr: SocketAddr,
    state: WebState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(addr = %bound, "web: server listening");
    let app = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    #[test]
    fn router_builds_without_panicking() {
        // Defensive — Router::new + with_state should compose
        // cleanly. A future axum version that tightens type
        // bounds would surface here before any real request runs.
        let (_tx, rx) = watch::channel(WireSnapshot::empty());
        let _r = router(WebState { rx });
    }
}
