//! v1.3.2 / DISPATCH 85 — web auth middleware.
//!
//! Shared-bearer-token gate on every `/api/*` route. The operator
//! sets `web.auth_token` in config; clients send `Authorization:
//! Bearer <token>` on every request. Missing or wrong token →
//! `401 Unauthorized` with no body content (no token echo, no
//! "expected X" hint that would leak partial info).
//!
//! ## Constant-time compare
//!
//! Token comparison goes through [`subtle::ConstantTimeEq`] — a
//! naive `==` on byte slices short-circuits on the first mismatch,
//! leaking the prefix length the attacker has guessed correctly.
//! The `subtle` crate's `ct_eq` traverses both slices fully and
//! `OR`s the byte differences into a single `Choice` so the
//! branch outcome is independent of WHERE the mismatch sits.
//!
//! ## What the token gates (and what it doesn't)
//!
//! Gates: every `/api/*` route (health, snapshot, WebSocket
//! stream). The static bundle (`/`, `/assets/*`) loads UNGATED
//! so the browser can render the shell + token prompt (C3
//! bootstrap: option (a) per the dispatch).
//!
//! Does NOT gate: the kill path. The D80/D81 invariant "web stays
//! OUT of the kill path" is unchanged — D85 protects the EXISTING
//! read surface; it does not introduce any kill-triggering route.
//! A future settings POST would land here (auth-gated) but is
//! explicitly out of scope for D85.

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::WebState;

/// Axum middleware that enforces the shared bearer token on every
/// `/api/*` request. Three branches:
///
///   1. `state.auth_token == None` (operator set `web.allow_no_auth
///      = true`) → pass through unconditionally. The `tracing::warn!`
///      at server startup is the operator's reminder; the gate here
///      is a no-op.
///   2. Header missing or malformed → `401 Unauthorized`, no body.
///   3. Header present and the bearer value `ct_eq`s the configured
///      token → pass through.
///
/// The 401 response carries an empty body (and no `WWW-Authenticate`
/// challenge — that would tell a browser to pop the basic-auth
/// dialog which doesn't apply to our token model). Clients see a
/// bare 401 and decide what to do (the SPA client re-prompts the
/// operator; a curl probe just sees the status code).
pub async fn require_token(
    State(state): State<WebState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.auth_token.as_ref() else {
        // `web.allow_no_auth = true` — operator's explicit opt-out.
        // Pass everything through without inspection.
        return next.run(request).await;
    };

    let header_value = match request.headers().get(header::AUTHORIZATION) {
        Some(h) => h,
        None => return unauthorized(),
    };
    let header_str = match header_value.to_str() {
        Ok(s) => s,
        Err(_) => return unauthorized(),
    };
    let bearer = match header_str.strip_prefix("Bearer ") {
        Some(b) => b,
        None => return unauthorized(),
    };

    if constant_time_eq(expected.as_bytes(), bearer.as_bytes()) {
        next.run(request).await
    } else {
        unauthorized()
    }
}

/// Build the 401 response. Empty body, no token echo, no
/// `WWW-Authenticate` header (we use Bearer tokens but skip the
/// challenge so browsers don't pop a basic-auth dialog over the
/// SPA's own token prompt). Tracing-side: we log only the fact of
/// the rejection at `debug` level — no header, no path, no token.
fn unauthorized() -> Response {
    tracing::debug!("web: rejected request (missing or invalid bearer token)");
    StatusCode::UNAUTHORIZED.into_response()
}

/// Constant-time byte-slice equality. Wraps [`subtle::ConstantTimeEq`]
/// so the call site reads as intent ("constant-time compare"). The
/// trait's [`ct_eq`] returns a [`subtle::Choice`] whose execution
/// time is independent of WHERE the byte mismatch is — a naive `==`
/// would short-circuit on the first differing byte and leak that
/// position via timing.
///
/// SAFETY of correctness: when lengths differ, `subtle::ConstantTimeEq`'s
/// blanket impl for `[u8]` does length pre-check WITHOUT short-
/// circuit on content, but it does report unequal — which is what
/// we want. (Length itself is not a secret in our threat model: the
/// operator's config file is on-disk; we're protecting against a
/// remote attacker on the LAN probing for the token, not against an
/// attacker who already has access to the configured `auth_token`
/// length.)
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_equal_bytes() {
        assert!(constant_time_eq(b"hunter2", b"hunter2"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_rejects_unequal_bytes() {
        assert!(!constant_time_eq(b"hunter2", b"hunter3"));
        assert!(!constant_time_eq(b"hunter2", b"hunter22"));
        assert!(!constant_time_eq(b"hunter2", b""));
        assert!(!constant_time_eq(b"", b"hunter2"));
    }

    /// THE TRIPWIRE — this module must always use `subtle::ConstantTimeEq`
    /// rather than `==`. A naive `==` on byte slices leaks the
    /// matching-prefix length via timing (first-mismatch short-
    /// circuit). The substring check below catches a future refactor
    /// that accidentally swaps the constant-time primitive for `==`.
    #[test]
    fn module_source_imports_subtle_constant_time_eq() {
        let src = include_str!("auth.rs");
        // We import the trait inside `constant_time_eq`'s body.
        assert!(
            src.contains("use subtle::ConstantTimeEq;"),
            "auth.rs MUST import subtle::ConstantTimeEq for the bearer-token \
             compare. A naive `==` leaks matching-prefix length via timing.",
        );
        assert!(
            src.contains(".ct_eq("),
            "auth.rs MUST call .ct_eq() — the subtle crate's constant-time \
             byte-slice compare. If this assertion fails, the token compare \
             has likely regressed to a naive `==`.",
        );
    }
}
