//! Module 7 — ratatui TUI.
//!
//! Three layers:
//! * `app`     — pure state machine: panel focus, filter, help overlay.
//! * `input`   — translates `crossterm` key events into `app::Action`s.
//! * `panels/` — one render function per panel; pure (state in, frame out).
//!
//! `run` owns the terminal lifecycle. It returns ownership of the `Runtime`
//! so callers can shut it down cleanly after the TUI exits.

pub mod app;
pub mod input;
mod panels;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::runtime::Runtime;

use app::{Action, App};

/// Drive the TUI tick/render loop until the user quits or `shutdown` is set.
/// Caller is responsible for handling SIGINT/SIGTERM via `shutdown`.
pub fn run(mut runtime: Runtime, shutdown: Arc<AtomicBool>) -> anyhow::Result<Runtime> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut runtime, shutdown);

    // Always restore the terminal, even if the loop errored.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result?;
    Ok(runtime)
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runtime: &mut Runtime,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let tick = Duration::from_millis(runtime.config().runtime.tick_interval_ms);
    let render = Duration::from_millis(runtime.config().runtime.render_interval_ms);
    let mut app = App::new();

    // Prime the state once so the first frame isn't empty.
    if let Err(e) = runtime.tick() {
        tracing::error!("initial tick failed: {}", e);
    }
    runtime.record_governor_audit();

    let mut last_tick = Instant::now();
    let mut last_render = Instant::now() - render;

    while !shutdown.load(Ordering::Relaxed) && !app.should_quit() {
        // Compute the wait budget: the smaller of (next tick, next render).
        // event::poll(timeout) is the elapsed-check primitive — it returns
        // when input arrives OR when the timeout expires. No thread::sleep.
        let now = Instant::now();
        let until_tick = tick.saturating_sub(now.saturating_duration_since(last_tick));
        let until_render = render.saturating_sub(now.saturating_duration_since(last_render));
        let wait = until_tick.min(until_render).max(Duration::from_millis(10));

        if event::poll(wait)?
            && let Ok(Event::Key(key)) = event::read()
        {
            let action = input::translate(key, &app);
            apply_action(action, runtime, &mut app);
        }

        if last_tick.elapsed() >= tick {
            if let Err(e) = runtime.tick() {
                tracing::error!("tick failed: {}", e);
            }
            runtime.record_governor_audit();
            last_tick = Instant::now();
        }

        if last_render.elapsed() >= render {
            terminal.draw(|f| panels::render(f, runtime.state(), &app))?;
            last_render = Instant::now();
        }
    }

    Ok(())
}

fn apply_action(action: Action, runtime: &mut Runtime, app: &mut App) {
    match action {
        Action::Quit => app.request_quit(),
        Action::ToggleDryRun => runtime.toggle_dry_run(),
        Action::FocusNext => app.focus_next(),
        Action::FocusPrev => app.focus_prev(),
        Action::SelectNext => app.select_next(runtime.state()),
        Action::SelectPrev => app.select_prev(runtime.state()),
        Action::ToggleHelp => app.toggle_help(),
        Action::StartFilter => app.start_filter(),
        Action::CancelFilter => app.cancel_filter(),
        Action::CommitFilter => app.commit_filter(),
        Action::FilterChar(c) => app.filter_push(c),
        Action::FilterBackspace => app.filter_pop(),
        Action::ConfirmKill => {
            if let Some(pid) = app.selected_pid(runtime.state()) {
                if app.armed_kill_pid() == Some(pid) {
                    let reason = "manual kill via TUI".to_string();
                    if let Err(e) = runtime.manual_kill(pid, reason) {
                        tracing::warn!("manual kill failed: {}", e);
                    }
                    app.disarm_kill();
                } else {
                    app.arm_kill(pid);
                }
            }
        }
        Action::None => {}
    }
}
