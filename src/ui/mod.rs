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

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::runtime::Runtime;
use crate::ui::panels::armed_banner::ArmedKill;
use crate::ui::panels::live_detail::{LiveDetail, LiveDetailBuffers, LiveDetailCard};
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
    // L17 / §5 — sparkline rolling buffers for the live-detail card.
    // Pinned to the card's PID via `LiveDetailBuffers.pid` so a
    // focus shift to another workload resets the buffers cleanly.
    // Stays paired with `live_detail` (both created on Enter, both
    // dropped on dismiss / expiry) — see L16's BACKLOG entry for
    // why this state lives here rather than on `App`. Lifting both
    // to App together is filed as a follow-up refactor row.
    let mut live_buffers: Option<LiveDetailBuffers> = None;

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
            // Row 1 INV-5 — crossterm 0.28 emits Press / Repeat /
            // Release variants. Only Press should drive dispatch;
            // Repeat events from a briefly-held key would otherwise
            // re-enter the kill-arming branch and silently switch
            // the armed PID across vitals refreshes, and Release
            // would double-dispatch every press. Synthesized
            // KeyEvents from terminals without kind reporting
            // default to Press in crossterm, so this filter doesn't
            // drop events on legacy hosts.
            && should_dispatch_key(key.kind)
            && let Some(action) = input::translate(key, &app)
        {
            apply_action(
                action,
                runtime,
                &mut app,
                &mut live_detail,
                &mut live_buffers,
            );
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
            // L17 / §5 — append one sample to each sparkline buffer
            // when a live-detail card is open. No-op when the card
            // is closed (buffers are None) or when the focused PID
            // has exited mid-card (sample() short-circuits on the
            // PID lookup). Tied to the tick cadence — one sample
            // per `runtime.tick_interval_ms`, which defaults to
            // 1 s; the 60-entry buffer therefore holds 60 s.
            if let Some(buffers) = live_buffers.as_mut() {
                buffers.sample(runtime.state());
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
                // Card gone → drop the sparkline buffers too. They
                // re-init on the next Enter, pinned to whatever PID
                // is focused at that moment.
                live_buffers = None;
            }
            // L19 — `tick_overlays` may have auto-dismissed an
            // expired post-mortem card. Drain the dispatcher's
            // dismissed-PID signal so the runtime stderr buffer
            // doesn't linger past the card's visibility (a no-op
            // when `sweep_expired_stderr` already pruned the entry).
            if let Some(pid) = app.take_dismissed_pid() {
                runtime.clear_stderr(pid);
            }
            terminal.draw(|f| {
                panels::render(
                    f,
                    runtime.state(),
                    &app,
                    &theme,
                    live_detail.as_ref(),
                    live_buffers.as_ref(),
                )
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
    live_buffers: &mut Option<LiveDetailBuffers>,
) {
    match action {
        Action::Quit => app.request_quit(),
        Action::ToggleHelp => app.toggle_help(),
        Action::SelectUp => app.select_prev(runtime.state()),
        Action::SelectDown => app.select_next(runtime.state()),
        Action::KillOrConfirm => {
            // CAR-14 / Row 1 — `k` is arm-only. Confirm fires on
            // Enter (see Action::OpenDetail below). Pre-CAR-14 the
            // second `k` press fired the kill; the contract changed
            // after smoke testing surfaced "did I press k once or
            // twice?" ambiguity.
            //
            // INV-6 — pressing `k` while a kill is already armed:
            //   * Same focused PID → refresh armed_at (extends the
            //     5s window from now).
            //   * Different focused PID → switch armed to the new
            //     PID with armed_at = Instant::now() (treats the
            //     second press as an explicit retarget).
            // Both cases are covered uniformly by ARMING fresh on
            // every `k` press: the new ArmedKill replaces the
            // prior one, and its `armed_at` is the new now.
            //
            // No-focus case: leave any prior armed_kill in place
            // and do nothing — operator can still press Enter to
            // confirm if a focus blip is transient.
            if let Some(pid) = app.selected_pid(runtime.state()) {
                let state = runtime.state();
                let proc = state.annotated.iter().find(|p| p.pid == pid);
                let name = proc.map(|p| p.name.clone()).unwrap_or_default();
                let allowlisted = runtime.config().policy.allowlist.contains(&name);
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
        Action::OpenDetail => {
            // CAR-14 / Row 1 INV-2 — armed kill takes priority over
            // the detail-card open path. Dispatching on `armed.pid`
            // (PINNED at arm time, snapshotted into the `ArmedKill`
            // struct) is what protects INV-1: the dispatched PID is
            // whatever the user armed, NOT whatever
            // `selected_pid(state)` returns now (which is volatile
            // across vitals refreshes because the workloads panel
            // sorts by status priority, and a workload transitioning
            // Healthy → Attention shifts row order).
            //
            // INV-3 + INV-4 — when no kill is armed (or armed-and-
            // expired), Enter falls through to the pre-CAR-14
            // `handle_open_detail` path: open live-detail if focused
            // PID is running, open post-mortem if exited+history
            // exists, otherwise no-op (logged).
            if let Some(armed) = app.take_armed_kill_if_active() {
                confirm_armed_kill(runtime, app, armed);
            } else {
                handle_open_detail(runtime, app, live_detail, live_buffers);
            }
        }
        Action::EscapeCascade => {
            // L16 — live-detail card sits at the front of the dismiss
            // queue; only when nothing live is up do we delegate to
            // `App::handle_escape` (which owns the post-mortem /
            // armed-kill / history / help / quit cascade). Keeping the
            // live branch local avoids reaching into app.rs for L16,
            // which is L24's edit territory. L17 — drop the sparkline
            // buffers alongside the card so a re-open with a different
            // PID doesn't reuse the previous workload's samples.
            if live_detail.is_some() {
                *live_detail = None;
                *live_buffers = None;
            } else {
                app.handle_escape();
                // L19 — if the cascade just dismissed a post-mortem
                // card tagged with an exited PID, drop the matching
                // transient stderr buffer in `Runtime` so the buffer
                // doesn't outlive the card's visibility per "stderr
                // is ephemeral".
                if let Some(pid) = app.take_dismissed_pid() {
                    runtime.clear_stderr(pid);
                }
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

/// CAR-14 / Row 1 — Enter-confirm dispatch for the armed kill.
///
/// Receives the armed record by value (already taken out of `App`
/// by `take_armed_kill_if_active`) so the field is guaranteed
/// cleared by the time this function runs — no second mutable
/// borrow of `app` for `disarm_kill` is needed. Calls
/// `runtime.manual_kill(armed.pid, …)` on the PINNED PID
/// (INV-1 / INV-2): even if `selected_pid(state)` has drifted to
/// a different workload between arm and confirm, the kill fires
/// on whatever the operator armed.
///
/// Dry-run mode surfaces the same status-footer hint the pre-
/// CAR-14 FIRE branch used so the operator gets explicit
/// feedback that the press was received and the signal was
/// suppressed by policy, not lost.
fn confirm_armed_kill(runtime: &mut Runtime, app: &mut App, armed: ArmedKill) {
    let was_dry_run = runtime.dry_run();
    let reason = "manual kill via TUI (Enter confirm)".to_string();
    if let Err(e) = runtime.manual_kill(armed.pid, reason) {
        tracing::warn!(pid = armed.pid, error = %e, "manual kill failed");
    } else if was_dry_run {
        app.set_status(format!(
            "DRY-RUN: would have sent SIGTERM to PID {} ({}) — press d to enforce",
            armed.pid, armed.name,
        ));
    }
}

/// Row 1 INV-5 — `true` when a `KeyEventKind` should drive
/// dispatch. Only `Press` qualifies; `Repeat` (key held) and
/// `Release` are dropped so the kill-arming branch can't re-enter
/// on a single physical keypress.
///
/// Extracted as a free function so unit tests can pin the
/// filter contract without spinning up an event loop.
pub(crate) fn should_dispatch_key(kind: KeyEventKind) -> bool {
    matches!(kind, KeyEventKind::Press)
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
    live_buffers: &mut Option<LiveDetailBuffers>,
) {
    // Enter on an already-open card dismisses it — the card's
    // footer advertises `[Enter] dismiss`, and routing the second
    // press here keeps the contract honest without needing a new
    // Action variant. L17: also drops the sparkline buffers so the
    // next open starts fresh against the then-focused PID.
    if live_detail.is_some() {
        *live_detail = None;
        *live_buffers = None;
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
    // snapshot and parks it in the local slot. L17 / §5 — also
    // spins up the per-metric rolling buffers pinned to this PID
    // and primes the first sample from the current tick so the
    // sparkline rows have something to render on the very next
    // frame (otherwise the row would read `(collecting…)` for one
    // tick before any data appeared).
    if let Some(detail) = LiveDetail::from_focused(state, pid) {
        let mut buffers = LiveDetailBuffers::new(pid);
        buffers.sample(state);
        *live_detail = Some(LiveDetailCard::new(detail));
        *live_buffers = Some(buffers);
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
        Some((post_mortem, exited_pid)) => {
            app.show_postmortem(crate::ui::panels::postmortem::PostMortemCard {
                post_mortem,
                shown_at: Instant::now(),
                // L19 — stamp the exited PID so the L24 Esc cascade
                // can drop the matching transient stderr buffer on
                // dismiss.
                pid: Some(exited_pid),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Row 1 — KeyEventKind filter (INV-5) ──────────────────────

    #[test]
    fn press_keys_dispatch() {
        assert!(should_dispatch_key(KeyEventKind::Press));
    }

    #[test]
    fn repeat_keys_are_filtered() {
        // crossterm 0.28 emits `Repeat` on held keys. Pre-Row-1 the
        // event loop accepted them, which re-entered the
        // kill-arming branch and silently retargeted the armed PID
        // when vitals reshuffled the workloads list between repeats.
        assert!(!should_dispatch_key(KeyEventKind::Repeat));
    }

    #[test]
    fn release_keys_are_filtered() {
        // Release double-dispatches every press. Filtering here
        // also matches the Windows-side audit fix so the two
        // binaries treat held-down keys identically.
        assert!(!should_dispatch_key(KeyEventKind::Release));
    }
}
