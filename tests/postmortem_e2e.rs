//! [UX-2] — post-mortem card integration tests.
//!
//! Three claims worth pinning at the integration boundary:
//!
//! 1. `RunRecord.stderr_lines` round-trips through JSON unchanged
//!    so the post-mortem card can be re-derived from a persisted
//!    record.
//! 2. The `App` lifecycle for a card — push, query, dismiss, expire,
//!    cascading-Esc priority over an armed kill — behaves as the
//!    input layer expects.
//! 3. `format_*` helpers produce the UI Contract strings for shapes
//!    the runtime is most likely to feed in (clean exit, OOM,
//!    governor kill).
//!
//! The trigger path itself (runtime sees an AI exit → builds a card)
//! is exercised by the lib unit tests around `Runtime::tick`. Wiring
//! a synthetic process through a real Runtime tick from an
//! integration test would balloon scope; the lib tests already pin
//! that wire.

use chrono::Utc;
use edge_monitor::analysis::compare::{Regression, Severity};
use edge_monitor::lifecycle::LifecycleSummary;
use edge_monitor::model::AICategory;
use edge_monitor::storage::run_store::{ExitReason, RunRecord};
use edge_monitor::ui::app::App;
use edge_monitor::ui::panels::armed_banner::ArmedKill;
use edge_monitor::ui::panels::postmortem::{
    PostMortemCard, format_duration, format_exit_reason, format_regression,
};

fn fixture_summary(model: &str) -> LifecycleSummary {
    LifecycleSummary {
        pid: 4242,
        name: "python".into(),
        category: Some(AICategory::Inference),
        model_name: Some(model.into()),
        spawn_time: Utc::now(),
        exit_time: Utc::now(),
        uptime_secs: 65,
        exit_code: Some(0),
        signal: None,
        avg_cpu_pct: 50.0,
        peak_cpu_pct: 90.0,
        peak_rss_mb: 2048,
        peak_vram_mb: 4096,
        samples: 30,
    }
}

fn fixture_card(model: &str, with_stderr: bool, regression: Option<Regression>) -> PostMortemCard {
    let mut record = RunRecord::from_summary(fixture_summary(model));
    if with_stderr {
        record.stderr_lines = Some(vec![
            "warning: model file is from a different vocab".into(),
            "INFO: cuda init OK".into(),
            "INFO: ready".into(),
        ]);
    }
    PostMortemCard {
        record,
        worst_regression: regression,
        shown_at: std::time::Instant::now(),
    }
}

#[test]
fn run_record_stderr_lines_survives_json_round_trip() {
    let mut record = RunRecord::from_summary(fixture_summary("phi3-mini"));
    record.stderr_lines = Some(vec!["line a".into(), "line b".into()]);
    let json = serde_json::to_string(&record).expect("serialize");
    let back: RunRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        back.stderr_lines,
        Some(vec!["line a".into(), "line b".into()])
    );

    record.stderr_lines = None;
    let json = serde_json::to_string(&record).expect("serialize");
    let back: RunRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.stderr_lines, None);
}

#[test]
fn show_postmortem_makes_card_observable_on_app() {
    let mut app = App::new();
    assert!(app.postmortem().is_none());
    app.show_postmortem(fixture_card("phi3-mini", false, None));
    assert!(app.postmortem().is_some());
    assert_eq!(
        app.postmortem().unwrap().record.summary.model_name.as_deref(),
        Some("phi3-mini"),
    );
}

#[test]
fn dismiss_postmortem_clears_the_card() {
    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini", false, None));
    app.dismiss_postmortem();
    assert!(app.postmortem().is_none());
}

#[test]
fn show_postmortem_replaces_existing_card_latest_wins() {
    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini", false, None));
    app.show_postmortem(fixture_card("llama-3-8b", false, None));
    assert_eq!(
        app.postmortem().unwrap().record.summary.model_name.as_deref(),
        Some("llama-3-8b"),
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
    app.show_postmortem(fixture_card("phi3-mini", false, None));

    assert!(app.handle_escape());
    assert!(app.postmortem().is_none());
    assert!(app.armed_kill().is_some());

    assert!(app.handle_escape());
    assert!(app.armed_kill().is_none());
}

#[test]
fn duration_formatting_picks_the_right_band() {
    assert_eq!(format_duration(5), "5s");
    assert_eq!(format_duration(65), "1m 5s");
    assert_eq!(format_duration(3725), "1h 2m 5s");
}

#[test]
fn regression_color_text_matches_severity_levels() {
    let warn = Regression {
        metric: "tokens_per_sec_avg".into(),
        baseline: 40.0,
        current: 35.0,
        delta_pct: -12.5,
        severity: Severity::Warn,
    };
    let (text, _) = format_regression(Some(&warn));
    assert_eq!(text, "-12.5% vs baseline (warning)");

    let crit = Regression {
        metric: "tokens_per_sec_avg".into(),
        baseline: 40.0,
        current: 28.0,
        delta_pct: -30.0,
        severity: Severity::Critical,
    };
    let (text, _) = format_regression(Some(&crit));
    assert_eq!(text, "-30.0% vs baseline (critical)");
}

#[test]
fn exit_reason_strings_match_ui_contract() {
    assert_eq!(format_exit_reason(&ExitReason::CleanExit), "cleanly");
    assert_eq!(
        format_exit_reason(&ExitReason::OutOfMemory {
            ram: false,
            vram: true,
        }),
        "killed by system (out of GPU memory)",
    );
    assert_eq!(
        format_exit_reason(&ExitReason::GovernorKill {
            reason: "ai-process exceeded 90% CPU".into(),
        }),
        "killed by governor (ai-process exceeded 90% CPU)",
    );
}
