use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App, Mode};

/// Translate a single keystroke into an `Action`. Pure: depends only on the
/// key + the current `App` mode (Normal vs Filter input).
///
/// Filter mode swallows printable keys into the filter buffer so the user
/// can type process names without colliding with the navigation hotkeys.
pub fn translate(key: KeyEvent, app: &App) -> Action {
    // Ctrl-C is universally "quit" — works even mid-filter / overlay.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }

    // History overlay swallows input until dismissed. j/k still scroll
    // (future), Esc closes; everything else is a no-op so the user
    // doesn't accidentally fire navigation actions on the panel beneath.
    if app.is_history_open() {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Action::CloseHistory,
            _ => Action::None,
        };
    }

    match app.mode() {
        Mode::Filter => match key.code {
            KeyCode::Esc => Action::CancelFilter,
            KeyCode::Enter => Action::CommitFilter,
            KeyCode::Backspace => Action::FilterBackspace,
            KeyCode::Char(c) => Action::FilterChar(c),
            _ => Action::None,
        },
        Mode::Normal => match key.code {
            // Esc cascades through overlays / armed kill. App::handle_escape
            // owns the priority order; we just route the press there.
            KeyCode::Esc => Action::Escape,
            // Enter dismisses the post-mortem card when one is visible.
            // Otherwise no-op (Enter has no other Normal-mode binding
            // today; the help text doesn't promise anything either).
            KeyCode::Enter if app.postmortem().is_some() => Action::DismissPostmortem,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('d') => Action::ToggleDryRun,
            KeyCode::Char('k') => Action::ConfirmKill,
            KeyCode::Char('h') => Action::OpenHistory,
            KeyCode::Char('/') => Action::StartFilter,
            KeyCode::Char('v') => Action::ToggleDetailMode,
            // [UX-3] open dashboard. Refusal cases (no row, empty
            // template) are handled in `handle_open_dashboard`.
            KeyCode::Char('g') => Action::OpenDashboard,
            KeyCode::Char('j') | KeyCode::Down => Action::SelectNext,
            KeyCode::Char('K') | KeyCode::Up => Action::SelectPrev,
            KeyCode::Tab => Action::FocusNext,
            KeyCode::BackTab => Action::FocusPrev,
            _ => Action::None,
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
        assert_eq!(translate(key(KeyCode::Char('q')), &app), Action::Quit);
    }

    #[test]
    fn ctrl_c_quits_even_in_filter_mode() {
        let mut app = App::new();
        app.start_filter();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate(ev, &app), Action::Quit);
    }

    #[test]
    fn slash_starts_filter() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('/')), &app),
            Action::StartFilter
        );
    }

    #[test]
    fn printable_char_in_filter_mode_appends() {
        let mut app = App::new();
        app.start_filter();
        assert_eq!(
            translate(key(KeyCode::Char('a')), &app),
            Action::FilterChar('a')
        );
    }

    #[test]
    fn esc_cancels_filter() {
        let mut app = App::new();
        app.start_filter();
        assert_eq!(translate(key(KeyCode::Esc), &app), Action::CancelFilter);
    }

    #[test]
    fn enter_commits_filter() {
        let mut app = App::new();
        app.start_filter();
        assert_eq!(translate(key(KeyCode::Enter), &app), Action::CommitFilter);
    }

    #[test]
    fn d_toggles_dry_run() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('d')), &app),
            Action::ToggleDryRun
        );
    }

    #[test]
    fn k_triggers_confirm_kill() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('k')), &app),
            Action::ConfirmKill
        );
    }

    #[test]
    fn tab_cycles_focus_forward() {
        let app = App::new();
        assert_eq!(translate(key(KeyCode::Tab), &app), Action::FocusNext);
    }

    #[test]
    fn v_toggles_detail_mode() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('v')), &app),
            Action::ToggleDetailMode,
        );
    }

    #[test]
    fn g_emits_open_dashboard() {
        let app = App::new();
        assert_eq!(
            translate(key(KeyCode::Char('g')), &app),
            Action::OpenDashboard,
        );
    }

    #[test]
    fn esc_in_normal_mode_routes_to_app_handle_escape() {
        let app = App::new();
        assert_eq!(translate(key(KeyCode::Esc), &app), Action::Escape);
    }

    /// Enter dismisses the post-mortem card when a card is visible;
    /// otherwise it's a no-op (filter-mode Enter is handled earlier).
    #[test]
    fn enter_dismisses_postmortem_when_visible() {
        use crate::lifecycle::LifecycleSummary;
        use crate::model::AICategory;
        use crate::storage::run_store::RunRecord;
        use crate::ui::panels::postmortem::PostMortemCard;
        use chrono::Utc;
        use std::time::Instant;

        let mut app = App::new();
        let summary = LifecycleSummary {
            pid: 1,
            name: "python".into(),
            category: Some(AICategory::Inference),
            model_name: Some("phi3-mini".into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 1,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 0.0,
            peak_cpu_pct: 0.0,
            peak_rss_mb: 0,
            peak_vram_mb: 0,
            samples: 1,
        };
        app.show_postmortem(PostMortemCard {
            record: RunRecord::from_summary(summary),
            worst_regression: None,
            shown_at: Instant::now(),
        });
        assert_eq!(
            translate(key(KeyCode::Enter), &app),
            Action::DismissPostmortem,
        );
    }

    #[test]
    fn enter_is_noop_in_normal_mode_without_postmortem() {
        let app = App::new();
        assert_eq!(translate(key(KeyCode::Enter), &app), Action::None);
    }
}
