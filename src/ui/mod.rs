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
    // L4 / UX_CONTRACT.md §15 — once-per-session symbol-set decision,
    // pinned on `App` so render sites pick the right glyphs even on
    // SSH bastions and `LANG=C` environments. Not re-evaluated on
    // resize.
    let mut app = App::with_symbol_set(symbols::detect());

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
            apply_action(action, runtime, &mut app);
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
            // for one extra frame.
            app.tick_overlays();
            terminal.draw(|f| panels::render(f, runtime.state(), &app))?;
            last_render = Instant::now();
        }
    }

    Ok(())
}

fn apply_action(action: Action, runtime: &mut Runtime, app: &mut App) {
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
        Action::OpenDetail => handle_show_postmortem(runtime, app),
        Action::EscapeCascade => {
            // Cascading priority is in `App::handle_escape`. The
            // pre-L2a `DismissPostmortem` action is gone; dismiss now
            // flows through this cascade.
            app.handle_escape();
        }
        // §4 — `a` acknowledges all visible alerts. Silent when
        // no alerts are active; otherwise sets a transient status
        // footer via `App::acknowledge_alerts`.
        Action::AcknowledgeAlerts => {
            app.acknowledge_alerts();
        }
        // §1 region 5 — `t` cycles Top processes sort. Wired in L14
        // alongside the new panel.
        Action::CycleTopSort => {}
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
    // WP5 — TCP preflight gates the spawn. `xdg-open` against a dead
    // Grafana surfaces a generic "couldn't open this page" via the
    // browser, indistinguishable from "the keybinding is broken". The
    // probe converts that into the contract-templated unreachable
    // message instead. No `--no-preflight` escape hatch in v1.0; if the
    // probe is wrong the operator can run `xdg-open <url>` themselves.
    if let Err(e) = crate::dashboard_preflight::probe(&url) {
        tracing::warn!(%url, error = %e, "Grafana preflight failed — skipping xdg-open");
        app.set_status(format_grafana_unreachable(&url));
        return;
    }
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

/// `Enter`-on-focused-row handler ([UX-2], UI Contract v2). Builds a
/// post-mortem snapshot for the focused workload's *most recent* run
/// and pushes it as a card. Skipped silently when no row is focused
/// or the focused workload has no run history yet — the latter is
/// expected for AI processes that have never exited; surfacing a
/// blank card would mislead more than logging it skips.
fn handle_show_postmortem(runtime: &Runtime, app: &mut App) {
    let Some(pid) = app.selected_pid(runtime.state()) else {
        tracing::info!("No workload focused — select a row first");
        return;
    };
    let state = runtime.state();
    let key = state
        .annotated
        .iter()
        .find(|p| p.pid == pid)
        .and_then(|p| p.model_name.clone().or(Some(p.name.clone())));
    let Some(model) = key else {
        tracing::info!(%pid, "Focused row has no model/name — skipping post-mortem");
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

/// Substitutes `{url}` into the
/// `ux_contract::status::GRAFANA_UNREACHABLE` template (WP5). Kept as a
/// pure function so the substitution rule can be pinned by tests without
/// running the preflight probe or the spawn. The template is owned by
/// the contract crate — display strings stay byte-for-byte identical to
/// the Windows side because both consume the same const.
pub fn format_grafana_unreachable(url: &str) -> String {
    ux_contract::status::GRAFANA_UNREACHABLE.replace("{url}", url)
}

/// L22 / UX_CONTRACT.md §12 — terminal size class. The renderer picks
/// a layout variant by tier so the same panel module can produce a
/// degraded view on small terminals without each panel re-querying
/// the frame size.
///
/// Classification lives in `ui` proper (not `ui::panels`) so callers
/// outside the render path — tests, future config surfaces, the
/// resize-event hook — can ask "what tier is this size?" without
/// invoking any render machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeTier {
    /// Below `MIN_COLS × MIN_ROWS`. §12: "render
    /// `ERR_TERMINAL_TOO_SMALL` message and wait for resize. Do not
    /// attempt degraded render."
    TooSmall,
    /// `MIN ≤ size < STANDARD`. Single-column workloads, Top
    /// processes panel hidden (§12: "first to drop on narrow
    /// screens").
    Narrow,
    /// `STANDARD ≤ size < WIDE_COLS` width. The default §1 layout.
    Standard,
    /// `width ≥ WIDE_COLS`. Workloads may render side-by-side when
    /// there are 4+ workloads.
    Wide,
}

impl SizeTier {
    /// Classify a frame size against the §12 breakpoints.
    ///
    /// Width and height are checked independently against `MIN_*` and
    /// `STANDARD_*` — a terminal that is wide enough but too short for
    /// Standard still renders Narrow, matching §12's "Minimum: 80×24"
    /// row (the 80×24 dimension is a floor on *both* axes, not just
    /// width).
    ///
    /// `Wide` is keyed off `WIDE_COLS` only: the row table and the
    /// contract expose `WIDE_COLS` but no `WIDE_ROWS`, even though
    /// §12 prose says "160+ × 50+". We follow the row table literally
    /// here; a future contract amendment can add `WIDE_ROWS` if the
    /// height threshold turns out to matter for parity.
    pub fn classify(width: u16, height: u16) -> Self {
        use ux_contract::sizing;
        if width < sizing::MIN_COLS || height < sizing::MIN_ROWS {
            return SizeTier::TooSmall;
        }
        if width < sizing::STANDARD_COLS || height < sizing::STANDARD_ROWS {
            return SizeTier::Narrow;
        }
        if width < sizing::WIDE_COLS {
            return SizeTier::Standard;
        }
        SizeTier::Wide
    }
}

#[cfg(test)]
mod size_tier_tests {
    use super::SizeTier;

    #[test]
    fn below_minimum_is_too_small() {
        assert_eq!(SizeTier::classify(70, 20), SizeTier::TooSmall);
        assert_eq!(SizeTier::classify(79, 24), SizeTier::TooSmall);
        assert_eq!(SizeTier::classify(80, 23), SizeTier::TooSmall);
    }

    #[test]
    fn exactly_minimum_is_narrow() {
        assert_eq!(SizeTier::classify(80, 24), SizeTier::Narrow);
    }

    #[test]
    fn between_min_and_standard_is_narrow() {
        // wide enough but too short
        assert_eq!(SizeTier::classify(120, 39), SizeTier::Narrow);
        // tall enough but too narrow
        assert_eq!(SizeTier::classify(119, 40), SizeTier::Narrow);
    }

    #[test]
    fn exactly_standard_is_standard() {
        assert_eq!(SizeTier::classify(120, 40), SizeTier::Standard);
    }

    #[test]
    fn below_wide_cols_stays_standard_regardless_of_height() {
        assert_eq!(SizeTier::classify(159, 100), SizeTier::Standard);
    }

    #[test]
    fn at_or_above_wide_cols_is_wide() {
        assert_eq!(SizeTier::classify(160, 50), SizeTier::Wide);
        assert_eq!(SizeTier::classify(200, 40), SizeTier::Wide);
    }
}
