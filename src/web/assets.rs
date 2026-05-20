//! Sprint-6 — embedded static assets for the web UI.
//!
//! `rust-embed` reads `web/dist/` at compile time and turns it into
//! a `RustEmbed`-trait implementation. The handler in
//! `handlers::serve_asset` looks up by path and returns the bytes
//! with the right MIME type — no filesystem dependency at runtime.
//!
//! ## Build sequence
//!
//! The dist directory must exist before `cargo build`:
//!
//! ```text
//! cd web
//! npm install
//! npm run build      # writes web/dist/{index.html, assets/*.js, *.css}
//! cd ..
//! cargo build
//! ```
//!
//! The README's "Building from source" section pins the same
//! sequence. CI bakes both into a single pipeline.

use rust_embed::RustEmbed;

/// Embeds everything under `web/dist/` into the binary. Path
/// resolution is relative to `CARGO_MANIFEST_DIR`, so the embed root
/// is `<repo>/web/dist`.
///
/// When `web/dist/` doesn't exist yet (fresh clone before `npm run
/// build`) the embed is empty and the handler falls back to a
/// "Frontend not built" placeholder so the operator sees a
/// recoverable error instead of a 404.
#[derive(RustEmbed)]
#[folder = "web/dist/"]
pub struct WebAssets;

/// Convenience: does the dist bundle look like it was built? A
/// freshly-cloned repo (`cargo build` before `npm run build`) will
/// have zero embedded files; we surface that to the operator on the
/// root route instead of pretending the dashboard exists.
pub fn dist_is_populated() -> bool {
    WebAssets::iter().next().is_some()
}
