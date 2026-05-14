//! [UX-2] (UI Contract v2) — post-mortem card integration tests.
//!
//! Pins the App lifecycle for a card (push, query, dismiss, replace,
//! cascading-Esc priority over an armed kill) and the v2 contract's
//! baseline-status banding. Render-shape assertions live in
//! `src/ui/panels/postmortem.rs::tests` since `build_lines` is
//! pub-crate; this file owns the cross-module integration claims.
//!
//! L24 added the §6 Esc cascade tests that pin overlay-close >
//! alerts-ack > quit precedence. They live in this file because the
//! pre-existing card > armed-kill cascade test already lived here and
//! the §6 cascade is one coherent contract.

use std::time::Instant;

use edge_monitor::runtime::ExitAlertEvent;
use edge_monitor::storage::run_store::ExitReason;
use edge_monitor::ui::app::App;
use edge_monitor::ui::panels::armed_banner::ArmedKill;
use edge_monitor::ui::panels::postmortem::{
    BaselineStatus, PostMortem, PostMortemCard,
};
use ux_contract::AlertId;

/// Fire an instant exit-driven alert (`OomDetected`) so the test sees
/// `active_count() > 0` without having to step through the per-tick
/// sustain gate. Exit-driven alerts bypass the sustain window by
/// design — they're "this PID exited with X reason", which has no
/// "still breaching" semantics.
fn fire_active_alert(app: &mut App) {
    app.observe_exit(
        Instant::now(),
        &ExitAlertEvent {
            pid: 1234,
            workload_name: "test-llm".into(),
            alert_id: AlertId::OomDetected,
            reason: None,
        },
    );
    assert!(
        app.alerts().active_count() > 0,
        "test fixture must leave at least one Active alert behind",
    );
}

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

/// L24 / §6 step 3 > step 4 — when history is open AND alerts are
/// visible, Esc closes history first. The user has to press Esc a
/// second time to acknowledge the alerts. This is the rule the row
/// description calls out explicitly ("ack all comes after history/
/// help close").
#[test]
fn cascading_escape_closes_history_before_acking_alerts() {
    let mut app = App::new();
    app.open_history("phi3-mini".into(), Vec::new());
    fire_active_alert(&mut app);
    assert!(app.is_history_open());

    // First Esc: history closes; alerts must NOT be ack'd this round.
    assert!(app.handle_escape());
    assert!(!app.is_history_open());
    assert!(
        app.alerts().active_count() > 0,
        "alerts must survive the Esc that closed history — step 3 \
         is strictly above step 4 in the §6 cascade",
    );

    // Second Esc: nothing else in the way, alerts get ack'd.
    assert!(app.handle_escape());
    assert_eq!(app.alerts().active_count(), 0);
    assert!(!app.should_quit(), "step 4 ack must not fall through to step 5 quit");
}

/// L24 / §6 step 3 > step 4 — help variant of the precedence rule.
/// History and help are both step 3 per §6; pin help separately so a
/// future refactor that splits the two cases cannot regress one
/// without the other failing visibly.
#[test]
fn cascading_escape_closes_help_before_acking_alerts() {
    let mut app = App::new();
    app.toggle_help();
    fire_active_alert(&mut app);
    assert!(app.show_help());

    assert!(app.handle_escape());
    assert!(!app.show_help());
    assert!(
        app.alerts().active_count() > 0,
        "alerts must survive the Esc that closed help",
    );

    assert!(app.handle_escape());
    assert_eq!(app.alerts().active_count(), 0);
    assert!(!app.should_quit());
}

/// L24 / §6 step 4 > step 5 — when alerts are visible and no card /
/// disarm / overlay is in the way, Esc acknowledges the alerts
/// instead of quitting. Without this step, an alert region open
/// over an otherwise-idle layout would receive a quit on Esc, which
/// would be a footgun for an operator using Esc as "clear this
/// noise".
#[test]
fn cascading_escape_acks_alerts_before_quit_when_no_overlay_is_open() {
    let mut app = App::new();
    fire_active_alert(&mut app);
    assert!(app.postmortem().is_none());
    assert!(app.armed_kill().is_none());
    assert!(!app.is_history_open());
    assert!(!app.show_help());

    let consumed = app.handle_escape();
    assert!(consumed, "step 4 must return true to distinguish from step 5 quit");
    assert_eq!(app.alerts().active_count(), 0);
    assert!(
        !app.should_quit(),
        "step 4 ack must take precedence over step 5 quit per §6",
    );
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
