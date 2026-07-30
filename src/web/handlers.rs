//! Sprint-6 — REST + static-asset handlers for the web UI.
//!
//! Endpoints (locked at v0.1 of the wire schema; see `wire.rs`):
//!
//!   `GET /`                  embedded `index.html` (Svelte SPA)
//!   `GET /assets/*path`      embedded static (JS / CSS bundle)
//!   `GET /api/snapshot`      full WireSnapshot for one-shot polls
//!   `GET /api/health`        liveness probe (just `ok`)
//!
//! The WebSocket stream at `/api/stream` lives in `stream.rs` —
//! kept separate because the upgrade flow is structurally different
//! from these one-shot JSON responses.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use super::WebState;
use super::assets::{WebAssets, dist_is_populated};

/// `GET /api/health` — operator-facing liveness probe. Returns `ok`
/// when the axum router is up; doesn't peek at the runtime. Tooling
/// (Grafana, systemd) can hit this to confirm the binary is alive
/// even when the tick loop is idle.
pub async fn health() -> &'static str {
    "ok"
}

/// `GET /api/snapshot` — one-shot full snapshot. Reads the latest
/// value from the watch channel; never blocks waiting for a new
/// tick. Use the WebSocket at `/api/stream` for live updates.
pub async fn snapshot(State(state): State<WebState>) -> impl IntoResponse {
    let snap = state.rx.borrow().clone();
    Json(snap)
}

/// `GET /` — serve the embedded `index.html` from the Svelte build.
/// Falls back to the "frontend not built" placeholder when
/// `web/dist/` was missing at compile time, so a freshly-cloned
/// repo without `npm run build` gives a recoverable hint instead
/// of a 404.
pub async fn index() -> Response {
    if let Some(content) = WebAssets::get("index.html") {
        let mime = "text/html; charset=utf-8";
        return (
            [(header::CONTENT_TYPE, HeaderValue::from_static(mime))],
            content.data,
        )
            .into_response();
    }
    placeholder_html().into_response()
}

/// `GET /assets/*path` — serve any other embedded file. Bytes get
/// the right MIME type via `mime_guess` so JS/CSS/SVG/images all
/// render correctly without us hand-mapping extensions.
///
/// v1.3.2 / DISPATCH 57 C1 — add `Cache-Control: no-cache` + `ETag`
/// headers. The web-zero bug surfaced via Tester DISPATCH 56:
/// browsers heuristically cache assets whose response carries
/// nothing but a `Content-Type`, so after a rebuild the browser
/// would happily serve a stale `index.js` from disk forever. The
/// fix is the smallest one that closes the staleness window
/// without forcing a Vite content-hash refactor: `no-cache` makes
/// the browser revalidate every load, and the `ETag` (derived from
/// the embedded file's compile-time SHA-256) is what the browser
/// sends back as `If-None-Match`. We don't yet implement the
/// conditional-GET 304 short-circuit — that's a future
/// optimisation; correctness is the win here.
pub async fn serve_asset(Path(path): Path<String>) -> Response {
    // axum's `*path` capture strips the leading `/assets/`; we look
    // up under the `assets/` prefix in the embed because the Svelte
    // build emits to `web/dist/assets/*`.
    let lookup = format!("assets/{path}");
    if let Some(content) = WebAssets::get(&lookup) {
        let mime = mime_guess::from_path(&lookup).first_or_octet_stream();
        let header_value = HeaderValue::from_str(mime.as_ref())
            // ok: expect — `mime_guess::Mime` always produces a
            // header-safe ASCII string. If this ever returns Err
            // the binary's mime crate has shipped a malformed
            // entry, which is unrecoverable at this point.
            .expect("mime_guess produces header-safe ASCII");
        // ETag from the first 8 bytes of the file's SHA-256 — 64
        // bits is plenty of entropy for a per-asset identifier
        // across a small bundle (today: index.js + index.css). The
        // wrapping quotes make this a "strong" ETag per RFC 9110
        // §8.8.3 (byte-for-byte identity); the embedded bytes are
        // immutable for a given binary, so strong semantics hold.
        let digest = content.metadata.sha256_hash();
        let etag_value = format!(
            "\"{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\"",
            digest[0],
            digest[1],
            digest[2],
            digest[3],
            digest[4],
            digest[5],
            digest[6],
            digest[7],
        );
        let etag_header = HeaderValue::from_str(&etag_value)
            // ok: expect — etag_value is a quoted ASCII hex string
            // by construction (16 hex digits + 2 quote chars). It
            // cannot contain bytes outside the header-safe range.
            .expect("etag is quoted ASCII by construction");
        return (
            [
                (header::CONTENT_TYPE, header_value),
                // `no-cache` per RFC 9111 §5.2.2.3: cache MAY store,
                // but MUST revalidate before reuse. Combined with
                // ETag this gives the operator a fresh asset on
                // every page load post-rebuild without spamming
                // bytes on unchanged files (once we add 304
                // handling).
                (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                (header::ETAG, etag_header),
            ],
            content.data,
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "asset not found").into_response()
}

/// Fallback HTML when `web/dist/` was empty at compile time. Tells
/// the operator the frontend wasn't built and lists the build
/// sequence to fix it. Plain HTML — no JS dependencies — so it
/// renders even when the asset bundle is broken.
fn placeholder_html() -> impl IntoResponse {
    let body = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>raqib — frontend not built</title>
<style>
body { font-family: ui-monospace, monospace; max-width: 640px; margin: 4rem auto; padding: 0 1rem; color: #c0caf5; background: #1a1b26; }
h1 { font-size: 1.3rem; color: #7aa2f7; }
code { background: #24283b; padding: 0.1rem 0.35rem; border-radius: 3px; }
pre { background: #24283b; padding: 1rem; border-radius: 5px; overflow-x: auto; }
.muted { color: #9aa5ce; }
</style>
</head>
<body>
<h1>raqib web UI — frontend not built</h1>
<p>The Rust backend is running (you're reading a response from it), but
<code>web/dist/</code> was empty at compile time, so no Svelte bundle was
embedded into the binary.</p>
<p>Build the frontend and re-build the binary:</p>
<pre>cd web
npm install
npm run build
cd ..
cargo build --release</pre>
<p class="muted">In the meantime the REST endpoints work:
<code>GET /api/snapshot</code> · <code>GET /api/health</code> ·
<code>WS  /api/stream</code>.</p>
</body>
</html>
"#;
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))],
        body,
    )
}

/// Defensive smoke: when `web/dist/` is populated we want at least
/// `index.html` present. Surfaced as a runtime check rather than a
/// compile-time error because a partial build (e.g., npm run build
/// halfway through) shouldn't kill the cargo build.
pub fn frontend_build_status() -> &'static str {
    if dist_is_populated() {
        "embedded"
    } else {
        "missing (build with `cd web && npm run build`)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_returns_ok_string() {
        assert_eq!(health().await, "ok");
    }

    #[test]
    fn frontend_build_status_reflects_dist_presence() {
        // Doesn't assert a specific value — this test runs from
        // both pre-build and post-build cargo invocations, and the
        // string flips between them. What we DO want to pin is
        // that the function returns one of the two known strings,
        // not a runtime panic.
        let s = frontend_build_status();
        assert!(
            s == "embedded" || s.starts_with("missing"),
            "unexpected build status: {s:?}"
        );
    }
}
