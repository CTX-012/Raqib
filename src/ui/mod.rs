//! Module 7 — ratatui TUI.
//!
//! Three layers:
//! * `app`     — pure state machine: panel focus, filter, help overlay.
//! * `input`   — translates `crossterm` key events into `app::Action`s.
//! * `panels/` — one render function per panel; pure (state in, frame out).
//!
//! `run` owns the terminal lifecycle. It returns ownership of the `Runtime`
//! so callers can shut it down cleanly after the TUI exits.

pub mod alerts;
pub mod app;
pub mod input;
pub mod panels;
pub mod symbols;
pub mod theme;

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
use crate::ui::panels::live_detail::{LiveDetail, LiveDetailCard};
use crate::ui::theme::UiTheme;

use app::{Action, App};

/// Drive the TUI tick/render loop until the user quits or `shutdown` is set.
/// Caller is responsible for handling SIGINT/SIGTERM via `shutdown`.
/// `theme` is resolved by the caller from CLI flag → config → §13
/// default; the loop owns the converted `UiTheme` for the session and
/// passes it to every render call.
pub fn run(
    mut runtime: Runtime,
    shutdown: Arc<AtomicBool>,
    theme: UiTheme,
) -> anyhow::Result<Runtime> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut runtime, shutdown, theme);

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
    theme: UiTheme,
) -> anyhow::Result<()> {
    let tick = Duration::from_millis(runtime.config().runtime.tick_interval_ms);
    let render = Duration::from_millis(runtime.config().runtime.render_interval_ms);
    // L4 / UX_CONTRACT.md §15 — once-per-session symbol-set decision,
    // pinned on `App` so render sites pick the right glyphs even on
    // SSH bastions and `LANG=C` environments. Not re-evaluated on
    // resize.
    let mut app = App::with_symbol_set(symbols::detect());

    // L16 / §5 — live-detail card slot. Lives here in the run_loop
    // rather than on `App` to keep app.rs unchanged (its `handle_escape`
    // cascade is L24's territory and merging additive fields under
    // that change would be risky). The post-mortem card stays on App
    // because pre-L16 dispatch + multiple test fixtures already wire
    // it that way.
    let mut live_detail: Option<LiveDetailCard> = None;

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
            && let Some(action) = input::translate(key, &app)
        {
            apply_action(action, runtime, &mut app, &mut live_detail);
        }

        if last_tick.elapsed() >= tick {
            if let Err(e) = runtime.tick() {
                tracing::error!("tick failed: {}", e);
            }
            runtime.record_governor_audit();
            // L6 / §4 — observe alert breach conditions for this
            // tick. AlertState lives on `App`; the metric inputs
            // come from `runtime.state()` plus `app.armed_kill_pid`.
            let now = Instant::now();
            app.observe_alerts(now, runtime.state());
            // L8 / §4 — drain exit-driven alerts queued by the
            // lifecycle exit hook this tick (OomDetected,
            // WorkloadExited).
            for event in runtime.drain_exit_alerts() {
                app.observe_exit(now, &event);
            }
            last_tick = now;
        }

        if last_render.elapsed() >= render {
            // Drop expired armed-kill / post-mortem snapshots before
            // drawing so the banner doesn't render with `0s` remaining
            // for one extra frame. The live-detail card's 30s window
            // gets the same treatment here so both detail-card kinds
            // share dismissal timing.
            app.tick_overlays();
            if let Some(card) = &live_detail
                && card.is_expired()
            {
                live_detail = None;
            }
            terminal.draw(|f| {
                panels::render(f, runtime.state(), &app, &theme, live_detail.as_ref())
            })?;
            last_render = Instant::now();
        }
    }

    Ok(())
}

fn apply_action(
    action: Action,
    runtime: &mut Runtime,
    app: &mut App,
    live_detail: &mut Option<LiveDetailCard>,
) {
    match action {
        Action::Quit => app.request_quit(),
        Action::ToggleHelp => app.toggle_help(),
        Action::SelectUp => app.select_prev(runtime.state()),
        Action::SelectDown => app.select_next(runtime.state()),
        Action::KillOrConfirm => {
            // FIRE branch: once armed, the kill is committed to the
            // armed PID. A second `k` must fire on `armed_kill_pid`
            // (not `selected_pid`) — otherwise selection drift between
            // ticks (PID list reshuffle, focus moves that the user
            // didn't notice) silently re-arms instead of firing, which
            // looks like the keypress was lost.
            if let Some(armed_pid) = app.armed_kill_pid() {
                // Snapshot name + dry-run state BEFORE disarm: the
                // armed-kill record is dropped by `disarm_kill`, and
                // `runtime.dry_run()` could in principle change between
                // the read and the message — pin both to the value at
                // the moment the kill was dispatched.
                let armed_name = app
                    .armed_kill()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let was_dry_run = runtime.dry_run();
                let reason = "manual kill via TUI".to_string();
                if let Err(e) = runtime.manual_kill(armed_pid, reason) {
                    tracing::warn!("manual kill failed: {}", e);
                } else if was_dry_run {
                    // Dry-run swallows the signal silently in
                    // `kill_sigterm` — the operator needs explicit
                    // feedback that the press was received but the
                    // process is still alive on purpose.
                    app.set_status(format!(
                        "DRY-RUN: would have sent SIGTERM to PID {armed_pid} ({armed_name}) — press d to enforce",
                    ));
                }
                app.disarm_kill();
            } else if let Some(pid) = app.selected_pid(runtime.state()) {
                // ARM branch: resolve name + allowlist status at the
                // moment of the keypress so the banner renders without
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
        Action::ToggleHistory => {
            // L2a merge — pre-L2a `OpenHistory` + `CloseHistory` collapse
            // into a single toggle per UX_CONTRACT §6 (`h` toggles).
            if app.is_history_open() {
                app.close_history();
                return;
            }
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
        Action::OpenGrafana => handle_open_dashboard(runtime, app),
        Action::OpenDetail => handle_open_detail(runtime, app, live_detail),
        Action::EscapeCascade => {
            // L16 — live-detail card sits at the front of the dismiss
            // queue; only when nothing live is up do we delegate to
            // `App::handle_escape` (which owns the post-mortem /
            // armed-kill / history / help / quit cascade). Keeping the
            // live branch local avoids reaching into app.rs for L16,
            // which is L24's edit territory.
            if live_detail.is_some() {
                *live_detail = None;
            } else {
                app.handle_escape();
            }
        }
        // §4 — `a` acknowledges all visible alerts. Silent when
        // no alerts are active; otherwise sets a transient status
        // footer via `App::acknowledge_alerts`.
        Action::AcknowledgeAlerts => {
            app.acknowledge_alerts();
        }
        // §1 region 5 / L14 — `t` cycles Top processes sort
        // (Ram → Cpu → Vram → Ram). Cycle semantics + the
        // contract-templated status footer live on the App
        // method; this dispatch site is just the routing.
        Action::CycleTopSort => app.cycle_top_sort(),
    }
}

/// `g` keybinding handler ([UX-3]) per UI Contract v2.
///
/// URL source priority (highest first):
///   1. `[dashboard].url_template` from config, if set and non-empty
///   2. `EDGE_MONITOR_GRAFANA_URL` environment variable, if set
///   3. Hardcoded fallback `http://localhost:3000/d/edge_monitor`
///
/// Spawns `xdg-open <url>` directly rather than going through the
/// `webbrowser` crate. The crate fans out across an opaque list of
/// helpers (`xdg-open` / `wslview` / `gio open` / `gnome-open` /
/// `kde-open`), which on a stripped distro silently picks the first
/// that exists — and reports a generic "no successful command"
/// error otherwise. `xdg-open` directly is the standard Linux
/// contract, fails fast with a recognisable spawn error, and lets
/// the operator install the missing piece in one step.
///
/// Surfaces a status-footer message for both outcomes so the
/// operator gets inline confirmation the keypress was received,
/// even when the spawn fails (the URL is shown so it can be
/// copy-pasted into another browser). Refuses with a status hint
/// when no row is focused.
fn handle_open_dashboard(runtime: &Runtime, app: &mut App) {
    let Some(pid) = app.selected_pid(runtime.state()) else {
        let msg = "No workload focused — select a row first";
        tracing::info!("{msg}");
        app.set_status(msg);
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
    match std::process::Command::new("xdg-open")
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            tracing::info!(%url, "Opened {url}");
            app.set_status(format!("Opening {url}"));
        }
        Err(e) => {
            // Tracing keeps the underlying spawn error for devs (the
            // most common cause on WSL is xdg-open / wslview not
            // installed; ErrorKind::NotFound surfaces as "No such file
            // or directory"). The status footer matches UI_CONTRACT
            // verbatim — no `os error 2`-style jargon for the user.
            tracing::warn!(%url, error = %e, "Could not open browser — URL: {url}");
            app.set_status(format!("Could not open browser — URL: {url}"));
        }
    }
}

/// L16 / UX_CONTRACT.md §5 — `Enter`-on-focused-row dispatch.
///
/// Routes to one of two cards based on the focused workload's
/// lifecycle state:
///   * Running PID in `state.annotated` → live-detail card with
///     instantaneous metrics (L17 will swap the placeholder for
///     sparklines).
///   * No live PID but `latest_postmortem(model)` returns a record
///     → post-mortem card with the retrospective summary.
///
/// Pressing `Enter` while a detail card is already open dismisses
/// it. Skipped silently when no row is focused — surfacing a blank
/// card would mislead more than logging-and-skipping. The two card
/// kinds are mutually exclusive at the render layer (see
/// `panels::render`); this dispatcher enforces the same invariant
/// at open time.
fn handle_open_detail(
    runtime: &Runtime,
    app: &mut App,
    live_detail: &mut Option<LiveDetailCard>,
) {
    // Enter on an already-open card dismisses it — the card's
    // footer advertises `[Enter] dismiss`, and routing the second
    // press here keeps the contract honest without needing a new
    // Action variant.
    if live_detail.is_some() {
        *live_detail = None;
        return;
    }
    if app.postmortem().is_some() {
        app.dismiss_postmortem();
        return;
    }

    let Some(pid) = app.selected_pid(runtime.state()) else {
        tracing::info!("No workload focused — select a row first");
        return;
    };
    let state = runtime.state();

    // Running-workload branch: live PID present in this tick's
    // annotated processes wins outright. Builds a LiveDetail
    // snapshot and parks it in the local slot.
    if let Some(detail) = LiveDetail::from_focused(state, pid) {
        *live_detail = Some(LiveDetailCard::new(detail));
        return;
    }

    // Exited-workload branch: the focused row is no longer
    // running but we have history for its model. This path
    // remains the pre-L16 behaviour intentionally — there is no
    // selectable Activity/history row yet (see L25 / §1 region
    // 6); when one lands, this branch widens to consume its
    // selection.
    let key = state
        .annotated
        .iter()
        .find(|p| p.pid == pid)
        .and_then(|p| p.model_name.clone().or(Some(p.name.clone())));
    let Some(model) = key else {
        tracing::info!(%pid, "Focused row has no model/name — skipping detail card");
        return;
    };
    match runtime.latest_postmortem(&model) {
        Some(post_mortem) => {
            app.show_postmortem(crate::ui::panels::postmortem::PostMortemCard {
                post_mortem,
                shown_at: Instant::now(),
            });
        }
        None => {
            tracing::info!(model = %model, "No run history yet for this workload");
        }
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
