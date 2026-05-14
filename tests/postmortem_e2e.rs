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
//!
//! L19 added the transient stderr-when-fresh tests: lifecycle of
//! `Runtime`'s per-PID stderr buffer (present immediately post-exit,
//! gone 30 s later, dropped on card dismiss), plus the 64-line × 1 KB
//! caps. They live here because the buffer's only consumer is the
//! post-mortem card, so the lifecycle is one coherent claim.

use std::time::{Duration, Instant};

use edge_monitor::config::Config;
use edge_monitor::runtime::{ExitAlertEvent, Runtime, StderrBuffer};
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
        pid: None,
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

// ----------------------------------------------------------------------------
// L19 / UX_CONTRACT.md §5 — transient stderr-when-fresh buffer.
// ----------------------------------------------------------------------------

const TEST_PID: u32 = 4242;

fn fresh_runtime() -> Runtime {
    Runtime::new(Config::default())
}

/// L19 — stderr captured during the run is queryable immediately after
/// exit. This is the canonical "fresh" case: the operator pops the
/// post-mortem card within the 30 s window and sees the captured tail.
#[test]
fn stderr_buffer_present_immediately_after_exit() {
    let mut rt = fresh_runtime();
    rt.record_stderr_line(TEST_PID, "CUDA out of memory");
    rt.record_stderr_line(TEST_PID, "Killed by OOM");
    rt.mark_stderr_exit(TEST_PID);

    let tail = rt.stderr_tail(TEST_PID);
    assert_eq!(
        tail,
        vec!["CUDA out of memory".to_string(), "Killed by OOM".to_string()],
        "buffer must return captured lines immediately after exit",
    );
}

/// L19 — 30 s after exit the buffer is gone, simulating an operator
/// who waited too long. The rewind uses `mark_stderr_exit_at` so the
/// test doesn't have to sleep; `stderr_tail` checks against
/// `Instant::now()` at call time and sees the entry as expired.
#[test]
fn stderr_buffer_gone_after_thirty_seconds() {
    let mut rt = fresh_runtime();
    rt.record_stderr_line(TEST_PID, "loading model weights...");
    rt.mark_stderr_exit_at(TEST_PID, Instant::now() - Duration::from_secs(31));

    let tail = rt.stderr_tail(TEST_PID);
    assert!(
        tail.is_empty(),
        "buffer must report empty once 30 s has elapsed past exit",
    );
}

/// L19 — `sweep_expired_stderr_at` proactively drops expired entries
/// so the map doesn't accumulate post-exit data past the 30 s
/// contract. The tick loop calls this once per second; we exercise it
/// directly here with a rewound `now` to confirm the entry is gone.
#[test]
fn sweep_drops_entries_past_expiry() {
    let mut rt = fresh_runtime();
    rt.record_stderr_line(TEST_PID, "warmup pass complete");
    rt.mark_stderr_exit_at(TEST_PID, Instant::now() - Duration::from_secs(31));
    rt.sweep_expired_stderr_at(Instant::now());

    // After the sweep, `clear_stderr` should be a no-op (entry already
    // gone); the public tail accessor still returns empty.
    rt.clear_stderr(TEST_PID);
    assert!(rt.stderr_tail(TEST_PID).is_empty());
}

/// L19 — when the post-mortem card dismisses, the matching transient
/// buffer must be dropped immediately so the data doesn't outlive the
/// card's visibility. The dispatcher's hook in `ui::apply_action`
/// drains `App::take_dismissed_pid` after `handle_escape`; the test
/// inlines those two steps because `apply_action` is module-private.
#[test]
fn stderr_buffer_cleared_when_card_dismisses_via_esc_cascade() {
    let mut rt = fresh_runtime();
    rt.record_stderr_line(TEST_PID, "exiting cleanly");
    rt.mark_stderr_exit(TEST_PID);
    assert_eq!(rt.stderr_tail(TEST_PID).len(), 1);

    let mut app = App::new();
    // Build a card stamped with the exited PID — the same shape
    // `handle_show_postmortem` builds in production via
    // `latest_postmortem`'s `(PostMortem, exited_pid)` return tuple.
    app.show_postmortem(PostMortemCard {
        post_mortem: fixture_post_mortem("phi3-mini"),
        shown_at: Instant::now(),
        pid: Some(TEST_PID),
    });
    assert!(app.postmortem().is_some());

    // Esc cascade: the same two-step pattern `ui::apply_action` uses
    // for `Action::EscapeCascade`.
    assert!(app.handle_escape());
    assert!(app.postmortem().is_none());
    let dismissed = app.take_dismissed_pid();
    assert_eq!(dismissed, Some(TEST_PID));
    rt.clear_stderr(dismissed.unwrap());

    assert!(
        rt.stderr_tail(TEST_PID).is_empty(),
        "buffer must be dropped synchronously with card dismissal",
    );
}

/// L19 — cards built without PID context (unit-test fixtures) do not
/// trigger a runtime clear: the dispatcher reads `None` from
/// `take_dismissed_pid` and skips the `clear_stderr` call. Pinned so
/// the dismiss-clear path can never accidentally key off a
/// PID-less card and clear an unrelated buffer.
#[test]
fn dismiss_clear_is_a_noop_for_cards_with_no_pid() {
    let mut rt = fresh_runtime();
    rt.record_stderr_line(TEST_PID, "still here after dismiss");
    rt.mark_stderr_exit(TEST_PID);

    let mut app = App::new();
    app.show_postmortem(fixture_card("phi3-mini")); // pid: None

    assert!(app.handle_escape());
    assert_eq!(app.take_dismissed_pid(), None);
    // Buffer untouched — TEST_PID's entry survives because we never
    // told `Runtime` to drop it.
    assert_eq!(rt.stderr_tail(TEST_PID).len(), 1);
}

/// L19 — the buffer is a 64-line drop-oldest ring. Pin the cap so a
/// future tuning that raises the constant has to update both the
/// constant and this test together.
#[test]
fn stderr_buffer_drops_oldest_past_sixty_four_lines() {
    let mut rt = fresh_runtime();
    for i in 0..100 {
        rt.record_stderr_line(TEST_PID, &format!("line {i}"));
    }
    // No exit yet — read while live; lines stay in insertion order.
    let tail = rt.stderr_tail(TEST_PID);
    assert_eq!(
        tail.len(),
        StderrBuffer::MAX_LINES,
        "buffer must cap at MAX_LINES (={})",
        StderrBuffer::MAX_LINES,
    );
    // First retained line should be "line 36" (100 - 64).
    assert_eq!(tail.first().unwrap(), "line 36");
    assert_eq!(tail.last().unwrap(), "line 99");
}

/// L19 — per-line byte cap. Lines longer than `MAX_LINE_BYTES` are
/// truncated at the nearest UTF-8 boundary ≤ the cap. We use ASCII
/// only here so the boundary is byte-exact; a separate test could
/// pin multi-byte boundary handling if a workload starts emitting
/// non-ASCII stderr.
#[test]
fn stderr_buffer_truncates_lines_past_one_kb() {
    let mut rt = fresh_runtime();
    let huge = "x".repeat(StderrBuffer::MAX_LINE_BYTES * 4);
    rt.record_stderr_line(TEST_PID, &huge);

    let tail = rt.stderr_tail(TEST_PID);
    assert_eq!(tail.len(), 1);
    assert!(
        tail[0].len() <= StderrBuffer::MAX_LINE_BYTES,
        "stored line length {} exceeded MAX_LINE_BYTES={}",
        tail[0].len(),
        StderrBuffer::MAX_LINE_BYTES,
    );
}

/// L19 — once a buffer is marked exited it is read-only. Late stderr
/// lines (e.g. from a sampler still processing buffered I/O) must
/// not mutate the captured tail, or the post-mortem card could show
/// data that arrived after the operator started reading.
#[test]
fn record_after_exit_is_silently_dropped() {
    let mut rt = fresh_runtime();
    rt.record_stderr_line(TEST_PID, "first");
    rt.mark_stderr_exit(TEST_PID);
    rt.record_stderr_line(TEST_PID, "after-exit");

    assert_eq!(rt.stderr_tail(TEST_PID), vec!["first".to_string()]);
}
