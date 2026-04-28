use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App, Mode};

/// Translate a single keystroke into an `Action`. Pure: depends only on the
/// key + the current `App` mode (Normal vs Filter input).
///
/// Filter mode swallows printable keys into the filter buffer so the user
/// can type process names without colliding with the navigation hotkeys.
pub fn translate(key: KeyEvent, app: &App) -> Action {
    // Ctrl-C is universally "quit" — works even mid-filter.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
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
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('d') => Action::ToggleDryRun,
            KeyCode::Char('k') => Action::ConfirmKill,
            KeyCode::Char('/') => Action::StartFilter,
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
}
