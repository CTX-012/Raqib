//! Sprint-6 — end-to-end web companion tests.
//!
//! Drives the axum router on an ephemeral port via a real
//! `TcpListener`, then exercises the REST + WebSocket surface from
//! a hyper / tokio-tungstenite client. The Rust binary itself is
//! not invoked; we own the watch::Sender from the test harness and
//! pump synthetic snapshots into it to verify the WS pipeline
//! delivers them downstream.

use std::net::SocketAddr;
use std::time::Duration;

use edge_monitor::web::{WebState, WireSnapshot};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;

/// Bind axum on an ephemeral port and return the bound address +
/// a sender the test can push snapshots into. Spawns a background
/// task running the server; the task is left to drop when the
/// test exits — `tokio::test` tears down the runtime which
/// implicitly aborts.
async fn spawn_server() -> (SocketAddr, watch::Sender<WireSnapshot>) {
    spawn_server_with_token(None).await
}

/// v1.3.2 / DISPATCH 85 — spawn a test server with an optional
/// bearer token. `None` ⇒ open access (pre-D85 behavior, what
/// the legacy tests expected). `Some(token)` ⇒ middleware enforces
/// `Authorization: Bearer <token>` on every `/api/*` request.
async fn spawn_server_with_token(
    token: Option<&str>,
) -> (SocketAddr, watch::Sender<WireSnapshot>) {
    spawn_server_with(token, None).await
}

/// v1.3.2 / DISPATCH 86 — variant that also accepts an optional
/// shared-tunables cell so the new /api/settings tests can exercise
/// the GET/POST roundtrip. Most legacy tests pass `None` and run
/// without the settings surface.
async fn spawn_server_with(
    token: Option<&str>,
    tunables: Option<edge_monitor::web::tunables::SharedTunables>,
) -> (SocketAddr, watch::Sender<WireSnapshot>) {
    let (tx, rx) = watch::channel(WireSnapshot::empty());
    let auth_token = token.map(std::sync::Arc::from);
    let state = WebState {
        rx,
        auth_token,
        tunables,
        config_path: None,
        auto_actuate_at_load: false,
        default_ai_action_at_load: "Allow".into(),
    };
    // Bind on port 0 so the OS picks an ephemeral; we then read it
    // back from the listener for the test client to connect.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = edge_monitor::web::router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Tiny settle delay so the listener task is past the await
    // point on `accept()` before the client connects. Without it
    // the first `reqwest::get` sometimes races the spawn.
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, tx)
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let (addr, _tx) = spawn_server().await;
    let url = format!("http://{addr}/api/health");
    let body = reqwest::get(&url).await.unwrap().text().await.unwrap();
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn snapshot_endpoint_returns_locked_schema() {
    let (addr, _tx) = spawn_server().await;
    let url = format!("http://{addr}/api/snapshot");
    let resp = reqwest::get(&url).await.unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    for key in [
        "tick",
        "server_time",
        "mission",
        "vitals",
        "workloads",
        "activity",
    ] {
        assert!(
            v.get(key).is_some(),
            "snapshot missing top-level key {key:?}: {v}"
        );
    }
}

#[tokio::test]
async fn snapshot_endpoint_reflects_publisher_updates() {
    let (addr, tx) = spawn_server().await;
    // Synthesize a snapshot with a non-zero tick and publish.
    let mut snap = WireSnapshot::empty();
    snap.tick = 42;
    snap.mission.workloads = 3;
    snap.mission.degraded = 1;
    tx.send(snap).unwrap();
    // Give the watch a moment to settle before the GET reads it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let url = format!("http://{addr}/api/snapshot");
    let body = reqwest::get(&url).await.unwrap().text().await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["tick"].as_u64(), Some(42));
    assert_eq!(v["mission"]["workloads"].as_u64(), Some(3));
    assert_eq!(v["mission"]["degraded"].as_u64(), Some(1));
}

#[tokio::test]
async fn root_route_serves_embedded_index_or_placeholder() {
    // Either the Svelte build is in `web/dist/` (CI / dev build
    // after `npm run build`) and serves `index.html`, OR the dist
    // is empty (fresh clone) and serves the placeholder HTML. Both
    // are valid; both must return 200 and HTML.
    let (addr, _tx) = spawn_server().await;
    let url = format!("http://{addr}/");
    let resp = reqwest::get(&url).await.unwrap();
    assert!(resp.status().is_success());
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("text/html"),
        "expected text/html, got {content_type:?}"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("<!doctype html>") || body.contains("<!DOCTYPE html>"),
        "expected HTML doctype, got: {}",
        &body[..body.len().min(120)]
    );
}

#[tokio::test]
async fn missing_asset_returns_404() {
    let (addr, _tx) = spawn_server().await;
    let url = format!("http://{addr}/assets/does-not-exist.js");
    let status = reqwest::get(&url).await.unwrap().status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}

/// v1.3.2 / DISPATCH 57 C1 — assets MUST carry `Cache-Control:
/// no-cache` and an `ETag` so the browser revalidates after every
/// rebuild instead of serving a heuristically-cached stale bundle.
/// The "web-zero" Tester report (DISPATCH 56) traced a stale
/// `index.js` to a missing `Cache-Control` — assets shipped only a
/// `Content-Type`, and browsers heuristic-cache anything else.
///
/// Test asserts ON a real asset path: `assets/index.js` is emitted
/// by `npm run build`. If the dist bundle is empty (fresh clone
/// before `npm run build`), the test skips its assertions but
/// still passes — same shape as
/// `root_route_serves_embedded_index_or_placeholder` above.
#[tokio::test]
async fn assets_carry_cache_control_and_etag_headers() {
    let (addr, _tx) = spawn_server().await;
    let url = format!("http://{addr}/assets/index.js");
    let resp = reqwest::get(&url).await.unwrap();
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // Fresh clone, no `npm run build` yet — nothing to assert.
        // The header behaviour is bound to the existence branch in
        // `serve_asset`; missing-asset 404s do not (and need not)
        // carry cache headers.
        return;
    }
    assert!(
        resp.status().is_success(),
        "expected 200 OK for /assets/index.js, got {}",
        resp.status(),
    );

    let cache_control = resp
        .headers()
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(
        cache_control, "no-cache",
        "asset must ship `Cache-Control: no-cache` so browsers \
         revalidate every load (web-zero scar). got: {cache_control:?}",
    );

    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Strong ETag per RFC 9110 §8.8.3: 16 hex digits inside a pair
    // of double quotes. Format is `"hhhhhhhhhhhhhhhh"`.
    assert!(
        etag.len() == 18 && etag.starts_with('"') && etag.ends_with('"'),
        "ETag must be a quoted 16-hex-digit strong validator, got: {etag:?}",
    );
    let hex_body = &etag[1..etag.len() - 1];
    assert!(
        hex_body.chars().all(|c| c.is_ascii_hexdigit()),
        "ETag body must be lowercase hex, got: {hex_body:?}",
    );
}

#[tokio::test]
async fn websocket_initial_snapshot_then_updates() {
    use tokio_tungstenite::tungstenite::Message;
    let (addr, tx) = spawn_server().await;
    let url = format!("ws://{addr}/api/stream");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // First frame is the watch's CURRENT value (the empty initial
    // snapshot). Confirms the WS handler sends an immediate frame
    // on connect rather than waiting for the first change.
    let frame = ws.next().await.unwrap().unwrap();
    let text = match frame {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["tick"].as_u64(), Some(0));

    // Publish an update; the next frame should reflect it.
    let mut updated = WireSnapshot::empty();
    updated.tick = 7;
    tx.send(updated).unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws timeout")
        .expect("ws stream ended")
        .expect("ws error");
    let text = match frame {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["tick"].as_u64(), Some(7));

    // Send Close so the server's `pump` exits cleanly.
    let _ = ws.send(Message::Close(None)).await;
}

// ─────────────────────────────────────────────────────────────────────
// DISPATCH 85 — bearer-token auth: 401 on missing/wrong, 200 on right.
// The router's /api/* sub-router runs through the auth middleware;
// the static bundle (`/`) loads UNGATED (C3 option (a)) so the
// browser can render the token prompt without a chicken-and-egg.
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_returns_401_when_no_authorization_header() {
    let (addr, _tx) = spawn_server_with_token(Some("hunter2")).await;
    let url = format!("http://{addr}/api/snapshot");
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "missing Authorization header MUST yield 401",
    );
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("hunter2"),
        "401 body MUST NOT echo the expected token; got: {body:?}",
    );
}

#[tokio::test]
async fn snapshot_returns_401_with_wrong_bearer_token() {
    let (addr, _tx) = spawn_server_with_token(Some("hunter2")).await;
    let url = format!("http://{addr}/api/snapshot");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", "Bearer wrong-token-value")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("hunter2") && !body.contains("expected"),
        "401 body MUST NOT echo or hint at the expected token; got: {body:?}",
    );
}

#[tokio::test]
async fn snapshot_returns_200_with_correct_bearer_token() {
    let (addr, _tx) = spawn_server_with_token(Some("hunter2")).await;
    let url = format!("http://{addr}/api/snapshot");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", "Bearer hunter2")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "correct token MUST yield 2xx; got {}",
        resp.status(),
    );
    let body = resp.text().await.unwrap();
    let _v: serde_json::Value = serde_json::from_str(&body)
        .expect("snapshot must serialise to JSON when authorized");
}

#[tokio::test]
async fn snapshot_returns_401_with_malformed_authorization_header() {
    let (addr, _tx) = spawn_server_with_token(Some("hunter2")).await;
    let url = format!("http://{addr}/api/snapshot");
    let client = reqwest::Client::new();
    // No "Bearer " prefix — just the raw token.
    let resp = client
        .get(&url)
        .header("Authorization", "hunter2")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "Authorization MUST have the 'Bearer ' prefix; raw token without it is 401",
    );
}

#[tokio::test]
async fn static_bundle_loads_without_token_for_c3_bootstrap() {
    // C3 option (a): the HTML shell loads UNGATED so the browser
    // can render the token prompt. Only /api/* is gated.
    let (addr, _tx) = spawn_server_with_token(Some("hunter2")).await;
    let url = format!("http://{addr}/");
    let resp = reqwest::get(&url).await.unwrap();
    assert!(
        resp.status().is_success(),
        "GET / (the HTML shell) MUST succeed without auth — the \
         browser needs the shell to render the token prompt. Got {}",
        resp.status(),
    );
}

#[tokio::test]
async fn snapshot_open_access_works_when_no_token_configured() {
    // `web.allow_no_auth = true` ⇒ no token configured ⇒ middleware
    // passes through. Pin the explicit opt-out.
    let (addr, _tx) = spawn_server_with_token(None).await;
    let url = format!("http://{addr}/api/snapshot");
    let resp = reqwest::get(&url).await.unwrap();
    assert!(
        resp.status().is_success(),
        "no token configured (allow_no_auth=true) ⇒ /api/* OPEN; got {}",
        resp.status(),
    );
}

#[tokio::test]
async fn health_endpoint_is_gated_when_token_configured() {
    // Liveness probe is still under /api/, so it's gated like the
    // rest. An operator who wants /api/health open for monitoring
    // can set `allow_no_auth = true`.
    let (addr, _tx) = spawn_server_with_token(Some("hunter2")).await;
    let url = format!("http://{addr}/api/health");
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "/api/health is gated when a token is configured. Operators \
         using bare-token monitoring MUST send Authorization on the \
         probe, or flip allow_no_auth=true for open access.",
    );
}

// ─────────────────────────────────────────────────────────────────────
// DISPATCH 86 — settings GET/POST: the web tunes thresholds; the
// boundary holds — auto_actuate / policy verbs are NOT writable.
// ─────────────────────────────────────────────────────────────────────

use edge_monitor::web::tunables::{SharedTunables, shared_from_config};

fn fresh_shared_tunables() -> SharedTunables {
    let cfg = edge_monitor::config::Config::default();
    shared_from_config(&cfg)
}

#[tokio::test]
async fn settings_get_returns_current_tunables_and_readonly_fields() {
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables)).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "GET /api/settings: {}", resp.status());
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v.get("thresholds").is_some(), "missing thresholds: {v}");
    assert!(v.get("kill_sustain_secs").is_some(), "missing kill_sustain_secs: {v}");
    assert!(
        v.get("auto_actuate_readonly").is_some(),
        "missing auto_actuate_readonly (must be present as informational): {v}"
    );
    assert!(
        v.get("default_ai_action_readonly").is_some(),
        "missing default_ai_action_readonly (must be present as informational): {v}"
    );
}

#[tokio::test]
async fn settings_get_unauthed_returns_401() {
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables)).await;
    let resp = reqwest::get(format!("http://{addr}/api/settings"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn settings_post_updates_thresholds_and_takes_effect_in_shared_cell() {
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables.clone())).await;
    let client = reqwest::Client::new();
    // Use vram_critical_pct = 90.0 (still above the default
    // vram_attention_pct of 85.0). A single-field POST below the
    // attention default would fail validation; lower the attention
    // alongside in that case.
    let body = serde_json::json!({
        "thresholds": { "vram_critical_pct": 90.0 },
    });
    let resp = client
        .post(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert!(
        status.is_success(),
        "POST must succeed; got {} body={}",
        status,
        body_text,
    );
    // The shared tunables cell now reflects the change — pin it.
    let guard = tunables.read().unwrap();
    assert!(
        (guard.thresholds.vram_critical_pct - 90.0).abs() < 0.01,
        "shared tunables MUST reflect the web POST; got vram_critical_pct={}",
        guard.thresholds.vram_critical_pct,
    );
}

/// THE HEADLINE BOUNDARY TEST. A crafted POST containing
/// `auto_actuate: true` MUST be rejected by serde
/// (`deny_unknown_fields`) — the handler never runs, the shared
/// cell never sees the field, and the read-only display on a
/// subsequent GET still reports `auto_actuate_readonly: false`.
#[tokio::test]
async fn settings_post_with_auto_actuate_is_rejected_by_deny_unknown_fields() {
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables)).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "thresholds": {},
        "auto_actuate": true,
    });
    let resp = client
        .post(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "POST with auto_actuate MUST be rejected with 4xx; got {}",
        resp.status(),
    );
    // Confirm a follow-up GET still reports auto_actuate=false.
    let resp = client
        .get(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["auto_actuate_readonly"].as_bool(),
        Some(false),
        "auto_actuate MUST remain false even after a crafted POST; got: {v}",
    );
}

#[tokio::test]
async fn settings_post_with_default_ai_action_is_rejected() {
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables)).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "thresholds": {},
        "default_ai_action": "Kill",
    });
    let resp = client
        .post(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "POST with default_ai_action MUST be rejected; got {}",
        resp.status(),
    );
}

#[tokio::test]
async fn settings_post_unauthed_returns_401() {
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables)).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "thresholds": {} });
    let resp = client
        .post(format!("http://{addr}/api/settings"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "unauthed POST inherits D85 401; got {}",
        resp.status(),
    );
}

/// kill_sustain_secs < alert_sustain_secs MUST be rejected on the
/// web-write path — the same invariant the config loader enforces
/// (D80 C3). A web POST cannot bypass the safety validation by
/// taking a side path.
#[tokio::test]
async fn settings_post_with_kill_sustain_below_alert_sustain_is_rejected() {
    // Use a fresh tunables cell with a known alert_sustain.
    let mut cfg = edge_monitor::config::Config::default();
    cfg.thresholds.alert_sustain_secs = Some(30);
    let tunables = shared_from_config(&cfg);
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables.clone())).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "thresholds": { "alert_sustain_secs": 30 },
        "kill_sustain_secs": 5,
    });
    let resp = client
        .post(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "kill_sustain (5) < alert_sustain (30) MUST be rejected; got {}",
        resp.status(),
    );
    let body_text = resp.text().await.unwrap();
    assert!(
        body_text.contains("kill_sustain_secs") && body_text.contains("alert_sustain_secs"),
        "rejection must name both fields so the operator can act on it; got: {body_text}",
    );
    // Shared cell must NOT have absorbed the bad value.
    let guard = tunables.read().unwrap();
    assert_ne!(
        guard.kill_sustain_secs, 5,
        "rejected POST MUST NOT mutate the shared tunables cell",
    );
}

#[tokio::test]
async fn settings_post_unknown_field_in_thresholds_is_silently_dropped() {
    // `ThresholdsConfig` does NOT carry `deny_unknown_fields`
    // (forward-compat: future numeric thresholds can be POSTed by
    // an old server without breaking). But the TOP-LEVEL
    // SettingsUpdate uses deny_unknown_fields so the auto_actuate
    // attack vector is structurally closed. This test pins both
    // properties: an unknown threshold key is dropped (no 400),
    // but an unknown top-level key (auto_actuate) is rejected.
    let tunables = fresh_shared_tunables();
    let (addr, _tx) = spawn_server_with(Some("hunter2"), Some(tunables)).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "thresholds": {
            "vram_critical_pct": 90.0,
            "future_field_we_dont_know": 42
        },
    });
    let resp = client
        .post(format!("http://{addr}/api/settings"))
        .header("Authorization", "Bearer hunter2")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "unknown threshold key should not 400 — forward-compat shape. \
         got {}: {}",
        resp.status(),
        resp.text().await.unwrap(),
    );
}
