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
