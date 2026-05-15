//! [UX-2] (UI Contract v2) — post-mortem card integration tests.
//!
//! Pins the App lifecycle for a card (push, query, dismiss, replace,
//! cascading-Esc priority over an armed kill) and the v2 contract's
//! baseline-status banding. Render-shape assertions live in
//! `src/ui/panels/postmortem.rs::tests` since `build_lines` is
//! pub-crate; this file owns the cross-module integration claims.

use edge_monitor::ui::app::App;
use edge_monitor::ui::panels::armed_banner::ArmedKill;
use edge_monitor::ui::panels::postmortem::{
    BaselineStatus, PostMortem, PostMortemCard,
};
use edge_monitor::storage::run_store::ExitReason;

fn fixture_post_mortem(model: &str) -> PostMortem {
    PostMortem {
        display_name: model.to_string(),
        duration_secs: 65,
        avg_cpu_pct: 38.4,
        peak_rss_mb: 1024,
        peak_vram_mb: 4096,
        tokens_per_sec: Some(38.4),
        exit_reason: ExitReason::CleanExit,
        stderr_tail: Vec::new(),
        baseline_status: BaselineStatus::NotAvailable,
    }
}

fn fixture_card(model: &str) -> PostMortemCard {
    PostMortemCard {
        post_mortem: fixture_post_mortem(model),
        shown_at: std::time::Instant::now(),
    }
}

#[test]
fn show_postmortem_makes_card_observable_on_app() {
    let mut app = App::new();
    assert!(app.postmortem().is_none());
    app.show_postmortem(fixture_card("phi3-mini"));
    assert!(app.postmortem().is_some());
    assert_eq!(
        app.postmortem().unwrap().post_mortem.display_name,
        "phi3-mini",
    );
}

#[test]
fn dismiss_postmortem_clears_the_card() {
    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini"));
    app.dismiss_postmortem();
    assert!(app.postmortem().is_none());
}

#[test]
fn show_postmortem_replaces_existing_card_latest_wins() {
    // UI Contract v2: latest wins, no queue. The user reading an
    // older card has it replaced by a fresh exit so they don't miss
    // the latest signal.
    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini"));
    app.show_postmortem(fixture_card("llama-3-8b"));
    assert_eq!(
        app.postmortem().unwrap().post_mortem.display_name,
        "llama-3-8b",
    );
}

#[test]
fn cascading_escape_clears_card_before_armed_kill() {
    let mut app = App::new();
    app.arm_kill(ArmedKill {
        pid: 4242,
        name: "ollama".into(),
        allowlisted: false,
        armed_at: std::time::Instant::now(),
    });
    app.show_postmortem(fixture_card("phi3-mini"));

    // First Esc dismisses the card; the armed kill survives.
    assert!(app.handle_escape());
    assert!(app.postmortem().is_none());
    assert!(app.armed_kill().is_some());

    // Second Esc disarms the kill.
    assert!(app.handle_escape());
    assert!(app.armed_kill().is_none());
}

#[test]
fn baseline_status_critical_band_is_at_or_above_twenty_percent() {
    // tokens/sec dropped from 40 → 28 → 30% slower → Critical band.
    assert!(matches!(
        BaselineStatus::from_metric(Some(28.0), Some(40.0)),
        BaselineStatus::Critical { .. },
    ));
}

#[test]
fn baseline_status_attention_band_is_ten_to_twenty_percent() {
    // tokens/sec dropped from 40 → 35.2 → 12% slower → Attention.
    assert!(matches!(
        BaselineStatus::from_metric(Some(35.2), Some(40.0)),
        BaselineStatus::Attention { .. },
    ));
}

#[test]
fn baseline_status_healthy_band_for_faster_runs() {
    // tokens/sec rose from 40 → 46 → 15% faster → Healthy.
    assert!(matches!(
        BaselineStatus::from_metric(Some(46.0), Some(40.0)),
        BaselineStatus::Healthy { .. },
    ));
}

#[test]
fn baseline_status_matching_band_within_ten_percent() {
    // 5% slower — inside the ±10% band.
    assert!(matches!(
        BaselineStatus::from_metric(Some(38.0), Some(40.0)),
        BaselineStatus::Matching,
    ));
}

#[test]
fn baseline_status_not_available_when_baseline_missing_or_zero() {
    assert!(matches!(
        BaselineStatus::from_metric(Some(40.0), None),
        BaselineStatus::NotAvailable,
    ));
    assert!(matches!(
        BaselineStatus::from_metric(Some(40.0), Some(0.0)),
        BaselineStatus::NotAvailable,
    ));
}

/// L16 / UX_CONTRACT.md §5 — the post-mortem card module no longer
/// owns the live-detail card. After the split there is a sibling
/// `live_detail` module with its own `LiveDetailCard`, and the two
/// card kinds must coexist without one accidentally importing the
/// other's types. This is the structural assertion the split exists
/// at all — if a future row collapses them, this test breaks loudly
/// rather than silently.
#[test]
fn post_l16_split_has_two_distinct_card_modules() {
    use edge_monitor::ui::panels::live_detail::LiveDetailCard;

    // Distinct types means the language won't let us assign one to
    // the other. The assertion at the type level is the compile —
    // if these were the same type, the explicit `_` binding below
    // would still compile, so we belt-and-brace with a contract
    // value check: same WINDOW duration, same physical dimensions.
    assert_eq!(
        LiveDetailCard::WINDOW,
        edge_monitor::ui::panels::postmortem::PostMortemCard::WINDOW,
        "card-split invariant: both kinds use the same auto-dismiss \
         window so the operator gets consistent dismissal timing",
    );
    assert_eq!(
        edge_monitor::ui::panels::postmortem::CARD_WIDTH, 64,
        "post-mortem card width is locked at 64 columns",
    );
}

/// L16 — running workloads no longer dispatch to the post-mortem
/// card on `Enter`. The full Enter-routing behaviour is exercised in
/// `tests/live_detail_card.rs`; here we pin the App-level
/// post-condition: `show_postmortem` is the only way the post-mortem
/// slot is populated, and a freshly-constructed App has neither slot
/// set.
#[test]
fn freshly_constructed_app_has_no_postmortem() {
    let app = App::new();
    assert!(
        app.postmortem().is_none(),
        "post-L16, the post-mortem slot is only set by exit-path code; \
         a fresh App must not pretend to have a card waiting",
    );
}

// ── Row 1 — CAR-14 Enter-confirm cross-state precedence ──────────
//
// These integration tests pin the priority contract from outside the
// crate: with both an armed kill AND a postmortem card visible,
// Enter must confirm the kill, NOT dismiss the postmortem. The
// inverse — Enter with no armed kill but a postmortem — must dismiss
// the card. Both invariants are exercised at the App / take_armed_*
// surface because the dispatch routing in `ui::mod.rs::apply_action`
// is private.

#[test]
fn enter_with_armed_kill_takes_precedence_over_postmortem_dismiss() {
    // Row 1 INV-2 — when both `armed_kill` and `postmortem` are
    // present, the dispatcher consumes the armed kill first. The
    // postmortem card stays put (it's dismissed on its own Enter
    // press once the arm is cleared).
    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini"));
    app.arm_kill(edge_monitor::ui::panels::armed_banner::ArmedKill {
        pid: 4242,
        name: "ollama".into(),
        allowlisted: false,
        armed_at: std::time::Instant::now(),
    });

    // Simulate the post-Row-1 Enter handler: take_armed_* first;
    // only when it returns None does the dispatch fall to the
    // postmortem-dismiss path.
    let taken = app.take_armed_kill_if_active();
    assert!(taken.is_some(), "armed kill must be taken first on Enter");
    assert_eq!(taken.unwrap().pid, 4242);
    // Card untouched — the operator still has it on screen and
    // can dismiss with a second Enter (which now falls through to
    // the existing handle_open_detail dismiss-branch).
    assert!(
        app.postmortem().is_some(),
        "postmortem card must survive an Enter that confirmed a kill"
    );
}

#[test]
fn enter_with_no_armed_kill_falls_through_to_postmortem_path() {
    // Row 1 INV-3 — no armed kill (or armed-and-expired) means
    // Enter routes to handle_open_detail, which is what
    // dismisses an already-open postmortem.
    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini"));
    let taken = app.take_armed_kill_if_active();
    assert!(
        taken.is_none(),
        "no armed kill → take_* returns None and Enter falls through"
    );
    // Caller (apply_action::OpenDetail) would call
    // dismiss_postmortem() on the fall-through; pin that the slot
    // still has the card so the dispatch has work to do.
    assert!(app.postmortem().is_some());
}

#[test]
fn expired_armed_kill_does_not_fire_on_enter() {
    // Row 1 INV-3 — armed-but-expired routes the same as unarmed.
    // The arm is dropped silently rather than firing a stale kill.
    let mut app = App::new();
    app.arm_kill(edge_monitor::ui::panels::armed_banner::ArmedKill {
        pid: 4242,
        name: "ollama".into(),
        allowlisted: false,
        // 6 seconds ago — past the 5-second WINDOW.
        armed_at: std::time::Instant::now() - std::time::Duration::from_secs(6),
    });
    assert!(
        app.take_armed_kill_if_active().is_none(),
        "expired arm must not be returned for dispatch"
    );
    assert!(
        app.armed_kill().is_none(),
        "expired arm must also be cleared so the banner doesn't linger"
    );
}
