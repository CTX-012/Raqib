use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App};

/// Translate a single keystroke into a `ux_contract::Action`. Pure:
/// depends only on the key + the current `App` state (history overlay
/// open vs not).
///
/// Returns `None` for unmapped keys — the outer loop simply skips
/// dispatch on `None`.
///
/// L2c retired the L2a transitional `Dispatch`/`LegacyAction` wrappers.
/// `ux_contract::Action` is now the entire input surface; new bindings
/// require a contract amendment, not a local enum extension.
pub fn translate(key: KeyEvent, app: &App) -> Option<Action> {
    // Ctrl-C is universally "quit" — works in any mode/overlay.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Some(Action::Quit);
    }

    // History overlay swallows input until dismissed. Esc cascades to
    // the close path via `App::handle_escape`; `q` and `h` toggle the
    // overlay shut (preserving pre-L2a behavior); everything else is
    // dropped so the user doesn't accidentally fire navigation actions
    // on the panel beneath.
    if app.is_history_open() {
        return match key.code {
            KeyCode::Esc => Some(Action::EscapeCascade),
            KeyCode::Char('q') | KeyCode::Char('h') => Some(Action::ToggleHistory),
            _ => None,
        };
    }

    // v1.3.2 / CAR-D97 / DISPATCH 97 — history-events browse modal
    // capture. Mirrors the D76 activity-browse shape: while the
    // events overlay is up, j/k belong to the events cursor, Esc
    // cascades to close, capital `H`/`q` toggle shut. Every other
    // top-level binding is suppressed so the operator doesn't
    // accidentally fire kill / detail / sort on the panel beneath.
    //
    // Reload key `r` is NOT routed through an Action here (CAR-D97
    // Option B): the run_loop peels it off BEFORE `translate` and
    // calls `App::reload_history_events_browse` directly. That
    // matches the RunStore overlay's local `h`/`q` precedent —
    // reload is a same-scope refresh, not a distinct dispatch
    // event to log or replay.
    if app.is_history_events_browsing() {
        return match key.code {
            KeyCode::Esc => Some(Action::EscapeCascade),
            KeyCode::Char('H') | KeyCode::Char('q') => {
                Some(Action::ToggleHistoryEvents)
            }
            KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectDown),
            KeyCode::Char('K') | KeyCode::Up => Some(Action::SelectUp),
            _ => None,
        };
    }

    match key.code {
        // §6 — Esc cascades through overlays / armed kill / quit.
        // App::handle_escape owns the priority order.
        KeyCode::Esc => Some(Action::EscapeCascade),
        // §6 — Enter opens the detail card. `apply_action` decides
        // live-detail vs post-mortem based on row state. Dismissal
        // flows through Esc per §6 cascade.
        KeyCode::Enter => Some(Action::OpenDetail),
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('k') => Some(Action::KillOrConfirm),
        KeyCode::Char('h') => Some(Action::ToggleHistory),
        // v1.3.2 / CAR-D97 / DISPATCH 97 — capital `H` opens the
        // history-events browse overlay. Coexists with lowercase
        // `h` (ToggleHistory / RunStore per-model overlay) above —
        // both preserved. When the overlay is open, the modal
        // capture branch at the top of `translate` swallows input.
        KeyCode::Char('H') => Some(Action::ToggleHistoryEvents),
        // §6 — `a` acknowledges all visible alerts. Silent (no
        // status footer) when no alerts are active; the dispatch
        // handler in `apply_action` decides.
        KeyCode::Char('a') => Some(Action::AcknowledgeAlerts),
        // §6 / §1 region 5 — `t` cycles the Top processes panel
        // sort (RAM → CPU → VRAM). Dispatch handler is
        // `App::cycle_top_sort`; help-overlay text is
        // `ux_contract::help::KEY_TOP_SORT`.
        KeyCode::Char('t') => Some(Action::CycleTopSort),
        KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectDown),
        // Note: §6 keymap defines lowercase `k` = KillOrConfirm.
        // Uppercase `K` (and Up arrow) bind to SelectUp as an
        // implementation extension — the contract is silent on `K`.
        // In the single-focusable v1.0 layout (Workloads is the only
        // focusable element), this resolves the vim-muscle-memory
        // expectation that `k` goes up. ux_contract v0.3.2's
        // `help::KEY_SELECT_UP` constant makes the choice
        // contract-official. Revisit if v1.1 adds multi-panel focus
        // (Tab cycling was deliberately removed by L2b).
        KeyCode::Char('K') | KeyCode::Up => Some(Action::SelectUp),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let app = App::new();
        assert_eq!(translate(key(KeyCode::Char('q')), &app), Some(Action::Quit));
    }

    /// Ctrl-C must win over any mode-specific dispatch. Pre-L2c the
    /// canary state was filter mode; with filter gone, the
    /// history overlay is the surviving "swallow input" mode and is
    /// the right canary now.
    #[test]
    fn ctrl_c_quits_even_with_history_overlay_open() {
        let mut app = App::new();
        app.open_history("phi3-mini".into(), Vec::new());
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate(ev, &app), Some(Action::Quit));
    }

    #[test]
    fn k_triggers_kill_or_confirm() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('k')), &app),
            Some(Action::KillOrConfirm)
        );
    }

    /// L7 — `a` acknowledges visible alerts. The dispatch handler
    /// (`apply_action`) decides whether to actually call ack_all
    /// based on whether any alerts are active; the input layer
    /// just routes the press.
    #[test]
    fn a_key_emits_acknowledge_alerts_action() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('a')), &app),
            Some(Action::AcknowledgeAlerts)
        );
    }

    /// L2b — `d`, `v`, Tab, Shift-Tab are not in §6 and are unbound.
    /// L2c — `/` (filter) is not in §6 either.
    /// Sprint 5 — `g` (open Grafana) is also unbound now that the
    /// dashboard integration was hard-deleted. All six must return
    /// None so the dispatch loop skips silently.
    #[test]
    fn keys_outside_section_six_are_unbound() {
        let app = App::new();
        for code in [
            KeyCode::Char('d'),
            KeyCode::Char('v'),
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Char('/'),
            KeyCode::Char('g'),
        ] {
            assert_eq!(
                translate(key(code), &app),
                None,
                "{:?} must be unbound — not in v0.3 §6 keymap (or removed in Sprint 5)",
                code
            );
        }
    }

    /// L14 — `t` cycles the Top processes panel sort. The input
    /// layer only routes the press; cycle semantics live on
    /// `App::cycle_top_sort` and are covered there.
    #[test]
    fn t_key_emits_cycle_top_sort_action() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('t')), &app),
            Some(Action::CycleTopSort),
        );
    }

    #[test]
    fn esc_in_normal_mode_routes_to_app_handle_escape() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Esc), &app),
            Some(Action::EscapeCascade)
        );
    }

    /// Enter unconditionally emits `OpenDetail`. The dispatch layer
    /// (`apply_action` in `ui/mod.rs`) is responsible for the "card
    /// already visible → replace" semantics; dismissal flows through
    /// the Esc cascade per UX_CONTRACT.md §6.
    #[test]
    fn enter_emits_open_detail_when_card_visible() {
        use crate::storage::run_store::ExitReason;
        use crate::ui::panels::postmortem::{BaselineStatus, PostMortem, PostMortemCard};
        use std::time::Instant;

        let mut app = App::new();
        app.show_postmortem(PostMortemCard {
            post_mortem: PostMortem {
                display_name: "phi3-mini".into(),
                duration_secs: 1,
                avg_cpu_pct: 0.0,
                peak_cpu_pct: 0.0,
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                tokens_per_sec: None,
                workload_category: None,
                exit_reason: ExitReason::CleanExit,
                stderr_tail: Vec::new(),
                baseline_status: BaselineStatus::NotAvailable,
            },
            shown_at: Instant::now(),
            pid: None,
        });
        assert_eq!(
            translate(key(KeyCode::Enter), &app),
            Some(Action::OpenDetail),
        );
    }

    #[test]
    fn enter_in_normal_mode_without_postmortem_emits_open_detail() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Enter), &app),
            Some(Action::OpenDetail),
        );
    }

    /// History overlay swallow: Esc routes through the cascade, `q`
    /// and `h` both toggle the overlay shut, other keys are dropped.
    #[test]
    fn esc_in_history_overlay_emits_escape_cascade() {
        let mut app = App::new();
        app.open_history("phi3-mini".into(), Vec::new());
        assert_eq!(
            translate(key(KeyCode::Esc), &app),
            Some(Action::EscapeCascade)
        );
    }

    #[test]
    fn q_in_history_overlay_toggles_it_shut() {
        let mut app = App::new();
        app.open_history("phi3-mini".into(), Vec::new());
        assert_eq!(
            translate(key(KeyCode::Char('q')), &app),
            Some(Action::ToggleHistory)
        );
    }

    #[test]
    fn unmapped_key_returns_none_in_normal_mode() {
        let app = App::new();
        // `x` is not bound to anything in §6 — the dispatch loop
        // skips silently.
        assert_eq!(translate(key(KeyCode::Char('x')), &app), None);
    }

    // ── v1.3.2 / CAR-D97 / DISPATCH 97 — history-events overlay pins ──

    /// CAR-D97 coexistence pin. Lowercase `h` opens the RunStore
    /// per-model overlay (Action::ToggleHistory); capital `H` opens
    /// the event archive browser (Action::ToggleHistoryEvents).
    /// DIFFERENT surfaces, both preserved. A future rebind that
    /// collapses either into the other fires this test.
    #[test]
    fn lowercase_h_and_capital_h_route_to_distinct_actions() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('h')), &app),
            Some(Action::ToggleHistory),
            "lowercase `h` must open the RunStore per-model overlay",
        );
        assert_eq!(
            translate(key(KeyCode::Char('H')), &app),
            Some(Action::ToggleHistoryEvents),
            "capital `H` must open the history-events browse overlay",
        );
    }

    /// CAR-D97 modal capture. While the events overlay is open, j/k
    /// route to the events cursor (SelectUp/SelectDown are re-used
    /// for the modal cursor per D76 precedent), Esc cascades, and
    /// `H`/`q` toggle shut. Every other key is dropped so the panel
    /// beneath doesn't accidentally receive kill / detail / sort.
    #[test]
    fn events_overlay_modally_captures_keys() {
        let mut app = App::new();
        app.toggle_history_events_browse(&crate::runtime::RuntimeState::default());
        // In-scope keys.
        assert_eq!(
            translate(key(KeyCode::Char('j')), &app),
            Some(Action::SelectDown),
        );
        assert_eq!(
            translate(key(KeyCode::Char('K')), &app),
            Some(Action::SelectUp),
        );
        assert_eq!(
            translate(key(KeyCode::Esc), &app),
            Some(Action::EscapeCascade),
        );
        assert_eq!(
            translate(key(KeyCode::Char('H')), &app),
            Some(Action::ToggleHistoryEvents),
        );
        assert_eq!(
            translate(key(KeyCode::Char('q')), &app),
            Some(Action::ToggleHistoryEvents),
        );
        // Suppressed. These would fire kill / detail / sort / new-
        // overlay if the modal capture leaked; they must return None.
        for suppressed in [
            KeyCode::Char('k'),
            KeyCode::Enter,
            KeyCode::Char('t'),
            KeyCode::Char('h'),
            KeyCode::Char('a'),
            KeyCode::Char('?'),
            // `r` is handled LOCALLY by the run_loop before
            // translate is called (CAR-D97 Option B) — the input
            // layer returns None here so translate stays a pure
            // key→Action mapping.
            KeyCode::Char('r'),
        ] {
            assert_eq!(
                translate(key(suppressed), &app),
                None,
                "{:?} must be modally suppressed while events overlay is open",
                suppressed,
            );
        }
    }
}
