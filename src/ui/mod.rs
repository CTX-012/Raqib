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
pub mod panels;

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
use crate::ui::panels::armed_banner::ArmedKill;

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
            // Drop expired armed-kill / post-mortem snapshots before
            // drawing so the banner doesn't render with `0s` remaining
            // for one extra frame.
            app.tick_overlays();
            // Drain any pending post-mortem cards from the runtime so
            // an AI exit between user keystrokes still surfaces. Done
            // inside the render budget so we're not racing the tick.
            for card in runtime.drain_postmortems() {
                app.show_postmortem(card);
            }
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
                    // Resolve name + allowlist status at the moment
                    // of the keypress so the banner renders without
                    // having to re-walk the runtime state every frame.
                    let state = runtime.state();
                    let proc = state.annotated.iter().find(|p| p.pid == pid);
                    let name = proc.map(|p| p.name.clone()).unwrap_or_default();
                    let allowlisted = runtime
                        .config()
                        .policy
                        .allowlist
                        .contains(&name);
                    app.arm_kill(ArmedKill {
                        pid,
                        name,
                        allowlisted,
                        armed_at: Instant::now(),
                    });
                }
            }
        }
        Action::OpenHistory => {
            // Resolve the focused row to a model name (preferring the
            // resolved model over the bare process name so multiple
            // PIDs of the same model cluster).
            if let Some(pid) = app.selected_pid(runtime.state()) {
                let state = runtime.state();
                let proc = state.annotated.iter().find(|p| p.pid == pid);
                let key = proc
                    .and_then(|p| p.model_name.clone())
                    .or_else(|| proc.map(|p| p.name.clone()))
                    .unwrap_or_default();
                let records = if key.is_empty() {
                    Vec::new()
                } else {
                    runtime.history(&key, 20)
                };
                app.open_history(key, records);
            }
        }
        Action::CloseHistory => app.close_history(),
        Action::ToggleDetailMode => app.toggle_detail_mode(),
        Action::OpenDashboard => handle_open_dashboard(runtime, app),
        Action::DismissPostmortem => app.dismiss_postmortem(),
        Action::Escape => {
            // Cascading priority is in `App::handle_escape`. Filter
            // mode's Esc is handled earlier by `input::translate`
            // (CancelFilter), so we never get here in filter mode.
            app.handle_escape();
        }
        Action::None => {}
    }
}

/// `g` keybinding handler ([UX-3]) per UI Contract v2.
///
/// URL source priority (highest first):
///   1. `[dashboard].url_template` from config, if set and non-empty
///   2. `EDGE_MONITOR_GRAFANA_URL` environment variable, if set
///   3. Hardcoded fallback `http://localhost:3000/d/edge_monitor`
///
/// Refuses with a status hint when no row is focused.
/// `compute_dashboard_url` does the `{model}`/`{pid}` substitution.
fn handle_open_dashboard(runtime: &Runtime, app: &App) {
    let Some(pid) = app.selected_pid(runtime.state()) else {
        tracing::info!("No workload focused — select a row first");
        return;
    };
    let template = resolve_dashboard_template(runtime.config());
    let state = runtime.state();
    let model = state
        .annotated
        .iter()
        .find(|p| p.pid == pid)
        .and_then(|p| p.model_name.clone());
    let url = compute_dashboard_url(&template, model.as_deref(), pid);
    match webbrowser::open(&url) {
        Ok(_) => tracing::info!(%url, "Opened {url}"),
        Err(e) => tracing::warn!(
            %url,
            error = %e,
            "Could not open browser — URL: {url}",
        ),
    }
}

/// Resolve the dashboard URL template per UI Contract v2 priority
/// order. Always returns *some* template — empty config + empty env
/// fall through to the hardcoded `localhost:3000/d/edge_monitor`
/// fallback. Pure aside from the env var read; exposed (`pub(crate)`)
/// so integration tests can pin the priority order without spinning
/// a browser.
pub fn resolve_dashboard_template(config: &crate::config::Config) -> String {
    if !config.dashboard.url_template.is_empty() {
        return config.dashboard.url_template.clone();
    }
    if let Ok(env) = std::env::var("EDGE_MONITOR_GRAFANA_URL")
        && !env.is_empty()
    {
        return env;
    }
    "http://localhost:3000/d/edge_monitor".to_string()
}

/// Pure substitution: applies `{model}` and `{pid}` against the
/// template. `None` for `model` substitutes empty (per UI Contract —
/// not a dash, not a placeholder; templates that include `{model}`
/// can target a fallback dashboard with a literal `var-model=` query
/// param when the value is empty). Exposed so the integration tests
/// can pin the substitution rules without spinning a browser.
pub fn compute_dashboard_url(template: &str, model: Option<&str>, pid: u32) -> String {
    template
        .replace("{model}", model.unwrap_or(""))
        .replace("{pid}", &pid.to_string())
}
