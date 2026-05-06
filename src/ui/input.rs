use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App, Dispatch, LegacyAction, Mode};

/// Translate a single keystroke into an action. Pure: depends only on
/// the key + the current `App` mode (Normal vs Filter input).
///
/// Returns `None` for unmapped keys — the outer loop simply skips
/// dispatch on `None` (replaces the pre-L2a `Action::None` no-op
/// variant with idiomatic `Option`).
///
/// L2a maps each binding to either a `ux_contract::Action` (the
/// locked v0.3 §6 keymap) or a transitional `LegacyAction` (Group D
/// bindings + filter family) which L2b and L2c will remove.
pub fn translate(key: KeyEvent, app: &App) -> Option<Dispatch> {
    // Ctrl-C is universally "quit" — works even mid-filter / overlay.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Some(Dispatch::Contract(Action::Quit));
    }

    // History overlay swallows input until dismissed. Esc cascades to
    // the close path via `App::handle_escape`; `q` toggles the
    // overlay shut (preserving the pre-L2a "q closes history"
    // behavior); `h` toggles likewise; everything else is dropped so
    // the user doesn't accidentally fire navigation actions on the
    // panel beneath.
    if app.is_history_open() {
        return match key.code {
            KeyCode::Esc => Some(Dispatch::Contract(Action::EscapeCascade)),
            KeyCode::Char('q') | KeyCode::Char('h') => {
                Some(Dispatch::Contract(Action::ToggleHistory))
            }
            _ => None,
        };
    }

    match app.mode() {
        Mode::Filter => match key.code {
            KeyCode::Esc => Some(Dispatch::Legacy(LegacyAction::CancelFilter)),
            KeyCode::Enter => Some(Dispatch::Legacy(LegacyAction::CommitFilter)),
            KeyCode::Backspace => Some(Dispatch::Legacy(LegacyAction::FilterBackspace)),
            KeyCode::Char(c) => Some(Dispatch::Legacy(LegacyAction::FilterChar(c))),
            _ => None,
        },
        Mode::Normal => match key.code {
            // §6 — Esc cascades through overlays / armed kill / quit.
            // App::handle_escape owns the priority order.
            KeyCode::Esc => Some(Dispatch::Contract(Action::EscapeCascade)),
            // §6 — Enter opens the detail card. `apply_action` decides
            // live-detail vs post-mortem based on row state. The
            // pre-L2a "Enter dismisses card when card visible" path is
            // gone; dismissal flows through Esc per §6 cascade.
            KeyCode::Enter => Some(Dispatch::Contract(Action::OpenDetail)),
            KeyCode::Char('q') => Some(Dispatch::Contract(Action::Quit)),
            KeyCode::Char('?') => Some(Dispatch::Contract(Action::ToggleHelp)),
            KeyCode::Char('k') => Some(Dispatch::Contract(Action::KillOrConfirm)),
            KeyCode::Char('h') => Some(Dispatch::Contract(Action::ToggleHistory)),
            // §6 — `g` opens Grafana for the focused workload.
            KeyCode::Char('g') => Some(Dispatch::Contract(Action::OpenGrafana)),
            KeyCode::Char('j') | KeyCode::Down => Some(Dispatch::Contract(Action::SelectDown)),
            KeyCode::Char('K') | KeyCode::Up => Some(Dispatch::Contract(Action::SelectUp)),
            // Group D — `d`/`v`/Tab/BackTab — removed by L2b. Routing
            // through LegacyAction keeps L2a a pure-rename refactor.
            KeyCode::Char('d') => Some(Dispatch::Legacy(LegacyAction::ToggleDryRun)),
            KeyCode::Char('v') => Some(Dispatch::Legacy(LegacyAction::ToggleDetailMode)),
            KeyCode::Tab => Some(Dispatch::Legacy(LegacyAction::FocusNext)),
            KeyCode::BackTab => Some(Dispatch::Legacy(LegacyAction::FocusPrev)),
            // Filter family — removed by L2c.
            KeyCode::Char('/') => Some(Dispatch::Legacy(LegacyAction::StartFilter)),
            _ => None,
        },
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
        assert_eq!(
            translate(key(KeyCode::Char('q')), &app),
            Some(Dispatch::Contract(Action::Quit))
        );
    }

    #[test]
    fn ctrl_c_quits_even_in_filter_mode() {
        let mut app = App::new();
        app.start_filter();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate(ev, &app), Some(Dispatch::Contract(Action::Quit)));
    }

    #[test]
    fn slash_starts_filter() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('/')), &app),
            Some(Dispatch::Legacy(LegacyAction::StartFilter))
        );
    }

    #[test]
    fn printable_char_in_filter_mode_appends() {
        let mut app = App::new();
        app.start_filter();
        assert_eq!(
            translate(key(KeyCode::Char('a')), &app),
            Some(Dispatch::Legacy(LegacyAction::FilterChar('a')))
        );
    }

    #[test]
    fn esc_cancels_filter() {
        let mut app = App::new();
        app.start_filter();
        assert_eq!(
            translate(key(KeyCode::Esc), &app),
            Some(Dispatch::Legacy(LegacyAction::CancelFilter))
        );
    }

    #[test]
    fn enter_commits_filter() {
        let mut app = App::new();
        app.start_filter();
        assert_eq!(
            translate(key(KeyCode::Enter), &app),
            Some(Dispatch::Legacy(LegacyAction::CommitFilter))
        );
    }

    #[test]
    fn d_toggles_dry_run() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('d')), &app),
            Some(Dispatch::Legacy(LegacyAction::ToggleDryRun))
        );
    }

    #[test]
    fn k_triggers_kill_or_confirm() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('k')), &app),
            Some(Dispatch::Contract(Action::KillOrConfirm))
        );
    }

    #[test]
    fn tab_cycles_focus_forward() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Tab), &app),
            Some(Dispatch::Legacy(LegacyAction::FocusNext))
        );
    }

    #[test]
    fn v_toggles_detail_mode() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('v')), &app),
            Some(Dispatch::Legacy(LegacyAction::ToggleDetailMode)),
        );
    }

    #[test]
    fn g_emits_open_grafana() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('g')), &app),
            Some(Dispatch::Contract(Action::OpenGrafana)),
        );
    }

    #[test]
    fn esc_in_normal_mode_routes_to_app_handle_escape() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Esc), &app),
            Some(Dispatch::Contract(Action::EscapeCascade))
        );
    }

    /// L2a — Enter unconditionally emits `OpenDetail`. The dispatch
    /// layer (`apply_action` in `ui/mod.rs`) is responsible for the
    /// "card already visible → replace" semantics; the pre-L2a
    /// `DismissPostmortem` action is gone, and dismissal flows
    /// through the Esc cascade per UX_CONTRACT.md §6.
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
        });
        assert_eq!(
            translate(key(KeyCode::Enter), &app),
            Some(Dispatch::Contract(Action::OpenDetail)),
        );
    }

    #[test]
    fn enter_in_normal_mode_without_postmortem_emits_open_detail() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Enter), &app),
            Some(Dispatch::Contract(Action::OpenDetail)),
        );
    }

    /// History overlay swallow: Esc routes through the cascade, `q`
    /// and `h` both toggle the overlay shut (preserving pre-L2a
    /// behavior), other keys are dropped (no-op).
    #[test]
    fn esc_in_history_overlay_emits_escape_cascade() {
        let mut app = App::new();
        app.open_history("phi3-mini".into(), Vec::new());
        assert_eq!(
            translate(key(KeyCode::Esc), &app),
            Some(Dispatch::Contract(Action::EscapeCascade))
        );
    }

    #[test]
    fn q_in_history_overlay_toggles_it_shut() {
        let mut app = App::new();
        app.open_history("phi3-mini".into(), Vec::new());
        assert_eq!(
            translate(key(KeyCode::Char('q')), &app),
            Some(Dispatch::Contract(Action::ToggleHistory))
        );
    }

    #[test]
    fn unmapped_key_returns_none_in_normal_mode() {
        let app = App::new();
        // `x` is not bound to anything in §6, Group D, or the filter
        // family — the dispatch loop skips silently.
        assert_eq!(translate(key(KeyCode::Char('x')), &app), None);
    }
}
