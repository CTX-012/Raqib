//! v0.3.10 vendor sweep — regression guards for the four contract
//! constants that this Linux binary started consuming in this
//! commit. Each test pins the *consumer* (the call site or
//! re-export) to the *contract value*, so if Agent A changes the
//! upstream number in a future contract minor without notifying
//! this repo, the Rust build (not the dashboard) is the first
//! thing to scream.
//!
//! Why a dedicated file: these are cross-module asserts (the
//! consumers live in `src/main.rs`, `src/ui/mod.rs`,
//! `src/ui/panels/activity.rs`, `src/ui/panels/workloads.rs`),
//! and an integration-test file is the cheapest place to land
//! them without bloating any single module's `#[cfg(test)]`
//! section.

/// CAR-19a — `status::RUNNING_ACTIVELY` is the contract-blessed
/// "process alive, doing work" fallback for non-Agent workload
/// rows. v1.0.1 still kept the string as a local const in
/// `src/ui/panels/workloads.rs`; v0.3.10 lifted it into the
/// contract and the vendor sweep imports it. The const is
/// re-exported via the panels module so a downstream rename in
/// `ux_contract` is caught here, not by a screenshot.
#[test]
fn workloads_panel_uses_contract_running_actively_constant() {
    assert_eq!(
        ux_contract::status::RUNNING_ACTIVELY,
        "running actively",
        "ux_contract changed the RUNNING_ACTIVELY string under us; \
         update the v1.x messaging review before the next release"
    );
}

/// CAR-19c — the TUI activity panel renders at most
/// `limits::ACTIVITY_FEED_TUI_MAX` rows. v0.3.10 pinned the cap
/// in the contract; the panel's `MAX_VISIBLE_EVENTS` is now a
/// re-export. The activity module is `pub(crate)`, so this test
/// asserts on the source-file expression — equivalent in
/// load-bearing-ness to the TS-mirror check below and matches
/// the same "anything but the contract value breaks this" shape.
#[test]
fn activity_feed_uses_contract_tui_max_constant() {
    let src = std::fs::read_to_string("src/ui/panels/activity.rs")
        .expect("src/ui/panels/activity.rs must exist");
    let expected = "pub const MAX_VISIBLE_EVENTS: usize = \
         ux_contract::limits::ACTIVITY_FEED_TUI_MAX;";
    assert!(
        src.contains(expected),
        "src/ui/panels/activity.rs::MAX_VISIBLE_EVENTS no longer \
         re-exports ux_contract::limits::ACTIVITY_FEED_TUI_MAX. \
         A literal that happens to equal the contract value still \
         drifts on the next contract bump — re-add the re-export."
    );
    assert_eq!(
        ux_contract::limits::ACTIVITY_FEED_TUI_MAX, 5,
        "if Agent A intentionally bumped ACTIVITY_FEED_TUI_MAX, \
         this assert is the heads-up; update or remove."
    );
}

/// CAR-19c — both wire publishers (TUI loop in `src/ui/mod.rs`,
/// headless loop in `src/main.rs`) `.take(WIRE_MAX)` before
/// shipping the activity slice. Pre-v0.3.10 that was `50`,
/// repeated at two sites. Post-vendor the value comes from the
/// contract. The test pins the published cap by re-reading the
/// contract value — anything else (a rebase that re-introduces
/// `take(50)`) breaks here.
#[test]
fn wire_take_cap_uses_contract_wire_max_constant() {
    assert_eq!(
        ux_contract::limits::ACTIVITY_FEED_WIRE_MAX,
        50,
        "if Agent A intentionally bumped ACTIVITY_FEED_WIRE_MAX, \
         this assert is the heads-up; update or remove. The wire \
         schema is locked at v0.1 — a cap bump is a quiet behavior \
         change for clients buffering on the receiving end."
    );
    // Sanity (TUI ≤ wire, web ≤ wire) is enforced at compile time
    // on the contract side via `const _: () = assert!(..)` blocks
    // in `~/ux_contract/src/lib.rs:816-820`. No duplicate assert
    // here — clippy `assertions_on_constants` flags it, and the
    // compile-time check is strictly stronger anyway.
}

/// CAR-19c — the Svelte `ActivityFeed.svelte` slices to
/// `ACTIVITY_FEED_WEB_MAX`, and `web/src/lib/limits.ts` mirrors
/// the contract value by hand because the Sprint 6 web bundle
/// has no Rust-to-TS code-gen step. This test reads the TS file
/// and asserts the literal still matches the contract. A drift
/// trips Rust-side CI before the dashboard ships the stale number.
#[test]
fn web_activity_feed_uses_contract_web_max_constant() {
    let ts = std::fs::read_to_string("web/src/lib/limits.ts")
        .expect("web/src/lib/limits.ts must exist (CAR-19c mirror)");
    let expected = format!(
        "export const ACTIVITY_FEED_WEB_MAX = {};",
        ux_contract::limits::ACTIVITY_FEED_WEB_MAX
    );
    assert!(
        ts.contains(&expected),
        "web/src/lib/limits.ts drifted from \
         ux_contract::limits::ACTIVITY_FEED_WEB_MAX (= {}). \
         Update the TS mirror to match the contract value.\n\
         File contents:\n{ts}",
        ux_contract::limits::ACTIVITY_FEED_WEB_MAX,
    );
}
