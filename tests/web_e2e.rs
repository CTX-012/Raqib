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
    let (tx, rx) = watch::channel(WireSnapshot::empty());
    let state = WebState { rx };
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
