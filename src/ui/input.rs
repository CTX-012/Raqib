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
        // §6 — `a` acknowledges all visible alerts. Silent (no
        // status footer) when no alerts are active; the dispatch
        // handler in `apply_action` decides.
        KeyCode::Char('a') => Some(Action::AcknowledgeAlerts),
        // §6 — `g` opens Grafana for the focused workload.
        KeyCode::Char('g') => Some(Action::OpenGrafana),
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
    /// L2c — `/` (filter) is not in §6 either. All five must return
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
        ] {
            assert_eq!(
                translate(key(code), &app),
                None,
                "{:?} must be unbound — not in v0.3 §6 keymap",
                code
            );
        }
    }

    #[test]
    fn g_emits_open_grafana() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('g')), &app),
            Some(Action::OpenGrafana),
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
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                tokens_per_sec: None,
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
}
