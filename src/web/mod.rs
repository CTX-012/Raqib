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
pub mod auth;
pub mod handlers;
pub mod settings;
pub mod stream;
pub mod tunables;
pub mod wire;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router, middleware,
    routing::{any, get},
};
use tokio::sync::watch;
use tower_http::cors::{Any, CorsLayer};

pub use wire::WireSnapshot;

/// Shared axum router state — every handler gets a clone of this.
/// `watch::Receiver` is cheap to clone (it's just an Arc internally),
/// so per-request clones are fine.
///
/// v1.3.2 / DISPATCH 85 — `auth_token` lives here so the middleware
/// can grab it via `State<WebState>`. Wrapped in `Arc<str>` for
/// cheap clones across request handlers; the token is a SECRET
/// and must NEVER be serialized into a response body or log line.
/// `None` ⇒ open access (the operator explicitly set
/// `web.allow_no_auth = true`).
///
/// v1.3.2 / DISPATCH 86 — `tunables` carries the structural
/// allowlist of web-writable settings (see [`tunables::RuntimeTunables`]).
/// `None` ⇒ the settings endpoints return 503; legacy callers
/// that never plumbed the shared tunables don't surface settings.
/// The read-only-boundary fields (`auto_actuate_at_load`,
/// `default_ai_action_at_load`) are snapshotted at server start
/// for display in the GET response — they're informational, NEVER
/// readable into a web write path.
#[derive(Clone)]
pub struct WebState {
    pub rx: watch::Receiver<WireSnapshot>,
    pub auth_token: Option<Arc<str>>,
    pub tunables: Option<tunables::SharedTunables>,
    pub config_path: Option<std::path::PathBuf>,
    /// Read-only mirror of `config.governor.auto_actuate` at the
    /// moment the web server was launched. The settings GET shows
    /// this to the operator as "Auto-actuate: ON/OFF — set in
    /// config file to change." Display honesty: the operator sees
    /// the state, the web offers no toggle.
    pub auto_actuate_at_load: bool,
    /// Read-only mirror of `config.policy.default_ai_action` at
    /// load time. Same display-honesty rationale.
    pub default_ai_action_at_load: String,
}

/// Construct the axum router. Exposed so integration tests can
/// drive the routes without binding a real socket.
///
/// v1.3.2 / DISPATCH 85 — the static-bundle routes (`/`, `/assets/*`)
/// stay UNGATED so the browser can load the shell to render the
/// token prompt (C3 bootstrap: option (a) — lock data, not the
/// empty HTML shell). Every `/api/*` route — including `/api/health`
/// — is gated by [`auth::require_token`]; when `state.auth_token`
/// is `None`, the middleware passes every request through (the
/// `web.allow_no_auth = true` opt-out).
pub fn router(state: WebState) -> Router {
    // CORS: locked to "any origin" because the server binds to
    // localhost-only by default; CORS isn't a security boundary
    // here, just a convenience for browser tooling. If a future
    // row exposes the server on a real network interface, this
    // policy needs tightening.
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any);

    // v1.3.2 / DISPATCH 85 — split into two sub-routers:
    //   * `api_routes` carries everything under `/api/*` and runs
    //     through the auth middleware before reaching the handler.
    //   * The top-level router serves the static bundle (`/`,
    //     `/assets/*`) unguarded so the browser can load the shell
    //     to render the token prompt.
    //
    // The `api_routes` middleware is attached BEFORE `with_state`
    // so axum infers the state type from the middleware itself.
    let api_routes = Router::new()
        .route("/health", get(handlers::health))
        .route("/snapshot", get(handlers::snapshot))
        // v1.3.2 / DISPATCH 68 — the first-party web dashboard now
        // polls `/api/snapshot` on a 1 Hz interval (see
        // `web/src/lib/rest.ts`) instead of subscribing here. The
        // route is intentionally retained for backward-compat with
        // any external script that may already speak it; a future
        // dispatch can remove it after a deprecation window. The
        // existing `tests/web_e2e.rs::websocket_initial_snapshot
        // _then_updates` continues to exercise the route end-to-end
        // so the regression surface stays visible if the handler
        // bit-rots while still mounted.
        .route("/stream", any(stream::websocket))
        // v1.3.2 / DISPATCH 86 — settings surface. GET returns the
        // current tunables + read-only view of the boundary fields;
        // POST mutates only the structurally-allowlisted fields
        // (see `settings::SettingsUpdate`). Both are auth-gated by
        // the D85 middleware applied below.
        .route(
            "/settings",
            get(settings::get_settings).post(settings::update_settings),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    Router::new()
        .route("/", get(handlers::index))
        .route("/assets/*path", get(handlers::serve_asset))
        .nest("/api", api_routes)
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
        let _r = router(WebState {
            rx,
            auth_token: None,
            tunables: None,
            config_path: None,
            auto_actuate_at_load: false,
            default_ai_action_at_load: "Allow".into(),
        });
    }
}
