//! Module 7 — ratatui TUI.
//!
//! Three layers:
//!
//! * `app` — pure state machine: workload selection, help overlay,
//!   alerts, kill_confirm slot, postmortem slot, history. L2c removed
//!   filter mode; L2b removed multi-panel focus (Workloads is the
//!   only focusable element).
//! * `input` — translates `crossterm` key events into
//!   `ux_contract::Action`s. `KeyEventKind::Press` only;
//!   `Repeat`/`Release` are filtered (Row 1 INV-5).
//! * `panels/` — one render function per panel; pure (state in, frame
//!   out). Themed end-to-end (L21). Tier-aware (L22 §12).
//!
//! `run` owns the terminal lifecycle. It returns ownership of the
//! `Runtime` so callers can shut it down cleanly after the TUI exits.

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
use crate::ui::panels::kill_confirm::KillConfirmCard;
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
    web_tx: Option<tokio::sync::watch::Sender<crate::web::WireSnapshot>>,
    history_view: Option<crate::web::history::SharedHistoryView>,
) -> anyhow::Result<Runtime> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut runtime, shutdown, theme, web_tx, history_view);

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
    web_tx: Option<tokio::sync::watch::Sender<crate::web::WireSnapshot>>,
    history_view: Option<crate::web::history::SharedHistoryView>,
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
        {
            // v1.3.2 / CAR-D97 / DISPATCH 97 — local `r` handler for
            // the history-events overlay. Peeled off BEFORE
            // `input::translate` so the reload doesn't need a new
            // `Action` variant (Option B — mirrors the RunStore
            // overlay's local `h`/`q` precedent). Only fires when the
            // overlay is open; every other context leaves `r`
            // unbound (translate returns None for it).
            if app.is_history_events_browsing()
                && matches!(key.code, crossterm::event::KeyCode::Char('r'))
            {
                let count = app.reload_history_events_browse(runtime);
                let msg = ux_contract::history_events::RELOAD_TEMPLATE
                    .replace("{count}", &count.to_string());
                app.set_status(msg);
            } else if let Some(action) = input::translate(key, &app) {
                apply_action(
                    action,
                    runtime,
                    &mut app,
                    &mut live_detail,
                    &mut live_buffers,
                );
            }
        }

        if last_tick.elapsed() >= tick {
            // v1.1.11 / DISPATCH 36 — alert eval moved to Runtime.
            // Forward the kill_confirm-card armed-PID into Runtime
            // BEFORE tick() so observe_alerts (inside tick) sees the
            // current arm state for the GovernorArmed alert.
            runtime.set_armed_pid(app.kill_confirm_pid());
            if let Err(e) = runtime.tick() {
                tracing::error!("tick failed: {}", e);
            }
            runtime.record_governor_audit();

            // v1.3.2 / DISPATCH 94 / PHASE 5 step 6 — refresh the
            // history read view for the web endpoints. Called from
            // ui/mod.rs (outside runtime.rs) so the tick-loop READ
            // of history state doesn't land inside the file the D91
            // tripwire scans.
            if let Some(view) = history_view.as_ref() {
                crate::web::history::refresh_shared(
                    view,
                    runtime.state(),
                    runtime.history_capture(),
                );
            }

            // DISPATCH 83 / C2 — once per tick, check whether a
            // Waiting-state kill_confirm card's targeted PID has
            // exited; if so, dismiss the card so the next render
            // doesn't paint a stale "waiting Ns…" overlay.
            auto_dismiss_waiting_card_if_target_exited(runtime, &mut app);
            let now = Instant::now();
            // Exit-driven alerts (L8 / §4) are fired by Runtime::tick
            // itself now; this drain is preserved so other consumers
            // (post-mortem card pop, sticky footer) still see the
            // events.
            for _event in runtime.drain_exit_alerts() {
                // Reserved: any non-alert consumer hooks land here.
            }
            let _ = now;
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

            // Sprint-6 — publish a fresh wire snapshot to web
            // subscribers. The activity feed slice bounds the JSON
            // payload at ~5–10 KB by capping at 50 records;
            // older entries are still queryable via the persistent
            // run store, just not pushed every tick.
            if let Some(tx) = web_tx.as_ref() {
                // v1.0.1 B-NEW-8 — AI-only filter for exit events now
                // lives inside `web::wire::build_activity` (DISPATCH
                // 71's merged 3-source feed). Non-AI shell exits
                // still don't reach the dashboard.
                // v1.3.2 / DISPATCH 71 — wire activity is now a
                // merged time-descending event log of `completed` +
                // `audit` + `regressions`; reading directly from
                // state at the builder removed the need to
                // pre-build `recent`.
                let snap = crate::web::WireSnapshot::from_runtime_state(runtime.state());
                let _ = tx.send(snap);
            }

            last_tick = now;
        }

        if last_render.elapsed() >= render {
            // Drop expired post-mortem snapshots / status footers
            // before drawing. The kill_confirm card has no auto-
            // dismiss (CAR-17) and is not swept here. The live-detail
            // card's 30s window is handled below at this same render
            // boundary.
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
        Action::SelectUp => {
            // v1.3.2 / CAR-D75 / DISPATCH 76 — modal capture.
            // While the activity panel is in browse mode, j/k
            // belong to the activity cursor; the workloads-panel
            // selection is suppressed. Same shape kill_confirm
            // captures Enter/Esc — the active modal owns the keys
            // it cares about. The renderer's event-key list is
            // recomputed against the live state so a refresh that
            // arrived between the dispatcher's read and the
            // cursor's move sees consistent indices.
            if app.is_activity_browsing() {
                let keys: Vec<String> =
                    crate::ui::panels::activity::build_events(runtime.state())
                        .iter()
                        .map(|e| e.key())
                        .collect();
                app.activity_browse_prev(&keys);
            } else if app.is_history_events_browsing() {
                // v1.3.2 / CAR-D97 / DISPATCH 97 — modal capture:
                // j/k belong to the events overlay's cursor. The
                // snapshot is FROZEN on App itself (unlike the
                // activity browse, which re-derives keys from
                // runtime.state() each tick because the live feed
                // shifts). Snapshot-on-open ⇒ App is the source of
                // truth for the cursor.
                app.history_events_browse_prev();
            } else {
                app.select_prev(runtime.state());
            }
        }
        Action::SelectDown => {
            if app.is_activity_browsing() {
                let keys: Vec<String> =
                    crate::ui::panels::activity::build_events(runtime.state())
                        .iter()
                        .map(|e| e.key())
                        .collect();
                app.activity_browse_next(&keys);
            } else if app.is_history_events_browsing() {
                app.history_events_browse_next();
            } else {
                app.select_next(runtime.state());
            }
        }
        Action::KillOrConfirm => {
            // CAR-17 — `k` opens the kill_confirm card on the focused
            // workload. Confirm fires on Enter (Action::OpenDetail
            // below). The card replaces the v0.3.x ARMED banner: kill
            // is always real, the card IS the safety surface.
            //
            // Pressing `k` again while a card is already open replaces
            // the card with a fresh snapshot — covers both the
            // same-PID refresh case and the retarget case uniformly.
            //
            // No-focus case: leave any open card alone and do nothing
            // — operator can still press Enter to confirm if a focus
            // blip is transient.
            if let Some(pid) = app.selected_pid(runtime.state())
                && let Some(card) = build_kill_confirm_card(runtime, app, pid)
            {
                app.open_kill_confirm(card);
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
        Action::OpenDetail => {
            // CAR-17 Enter-priority cascade. The kill_confirm card has
            // the highest priority — its PID was pinned at card-open
            // time, so an Enter while the card is up dispatches the
            // kill on the pinned PID regardless of whether
            // `selected_pid(state)` has drifted since (workload panel
            // sort order is volatile across vitals refreshes).
            //
            //   1. kill_confirm open → Enter = confirm kill
            //   2. live_detail open  → Enter = dismiss
            //   3. post_mortem open  → Enter = dismiss
            //   4. focused running   → Enter = open live_detail
            //   5. fallback          → Enter = open post_mortem
            //
            // Steps 2/3 land in `handle_open_detail`, which detects
            // an already-open card and treats Enter as dismiss.
            // Steps 4/5 also land in the same handler.
            // DISPATCH 83 / C3 — Enter dispatch on a kill_confirm
            // card is now stage-dependent:
            //
            //   * Confirm  → SIGTERM (today's path) then transition
            //                to Waiting (card stays open).
            //   * Waiting  → SIGKILL via the consent-gated path
            //                (`Runtime::manual_force_kill`) then
            //                dismiss.
            //
            // The non-card cascade (activity browse / live_detail /
            // post_mortem) is unaffected — only a kill_confirm-open
            // path peels off here.
            if let Some(card) = app.take_kill_confirm() {
                // Take-then-route. The card carries its own stage,
                // so the peek-first-take dance the pre-D83 code
                // didn't need either. Confirm-path re-opens a
                // Waiting card via `app.open_kill_confirm(...)`;
                // Waiting-path consumes and dismisses.
                if card.is_waiting() {
                    force_kill_from_card(runtime, app, card);
                } else {
                    confirm_kill_from_card(runtime, app, card);
                }
            } else if app.is_activity_browsing() {
                // v1.3.2 / CAR-D75 / DISPATCH 76 — Enter in browse
                // mode toggles expand on the selected entry, but
                // ONLY when the entry has detail. Regression rows
                // are a deliberate no-op (mirrors the web's
                // disabled-button behavior from D74). The
                // selected entry is resolved via the same
                // composite-key lookup the cursor uses, so a
                // refresh between Enter dispatch and the toggle
                // doesn't lose the operator's intent.
                let events =
                    crate::ui::panels::activity::build_events(runtime.state());
                let keys: Vec<String> = events.iter().map(|e| e.key()).collect();
                if let Some(b) = app.activity_browse() {
                    let i = b
                        .selected_key
                        .as_ref()
                        .and_then(|k| keys.iter().position(|x| x == k))
                        .unwrap_or(0);
                    if let Some(ev) = events.get(i)
                        && ev.detail.is_some()
                    {
                        app.activity_browse_toggle_expand();
                    }
                    // Else: regression row OR empty feed → Enter
                    // is a no-op. No status footer noise; the lack
                    // of an expansion chevron in the render path
                    // makes it visually clear nothing happened.
                }
            } else {
                handle_open_detail(runtime, app, live_detail, live_buffers);
            }
        }
        Action::EscapeCascade => {
            // L16 — live-detail card sits at the front of the dismiss
            // queue (after kill_confirm); only when nothing live is up
            // do we delegate to `App::handle_escape` (which owns the
            // kill_confirm / post-mortem / history / help / quit
            // cascade — see `App::handle_escape` for the order).
            // L17 — drop the sparkline buffers alongside the card so
            // a re-open with a different PID doesn't reuse the
            // previous workload's samples.
            if live_detail.is_some() {
                *live_detail = None;
                *live_buffers = None;
            } else {
                app.handle_escape(runtime);
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
            app.acknowledge_alerts(runtime);
        }
        // §1 region 5 / L14 — `t` cycles Top processes sort
        // (Ram → Cpu → Vram → Ram). Cycle semantics + the
        // contract-templated status footer live on the App
        // method; this dispatch site is just the routing.
        Action::CycleTopSort => app.cycle_top_sort(),
        // v1.3.2 / CAR-D75 / DISPATCH 76 — `A` toggles the activity
        // browse mode. Lowercase `a` stays AcknowledgeAlerts; the
        // capital is the modal capture trigger. Default render is
        // unchanged — browse mode is OPT-IN.
        Action::ToggleActivityBrowse => {
            app.toggle_activity_browse(runtime.state());
        }
        // v1.3.2 / CAR-D97 / DISPATCH 97 — `H` toggles the history-
        // events browse overlay. Coexists with lowercase `h`
        // (Action::ToggleHistory / RunStore per-model overlay) —
        // DIFFERENT surfaces, both preserved. Snapshot-on-open (Q5):
        // opening the overlay ALSO immediately calls
        // `reload_history_events_browse` so the frozen event list
        // is populated on entry (`toggle_*` seeds an empty snapshot
        // sentinel; the reload call fills it). Closing the overlay
        // just drops the snapshot — no runtime touch needed.
        Action::ToggleHistoryEvents => {
            let was_open = app.is_history_events_browsing();
            app.toggle_history_events_browse(runtime.state());
            if !was_open {
                app.reload_history_events_browse(runtime);
            }
        }
    }
}

/// CAR-17 — first-Enter dispatch for the kill_confirm card (Confirm
/// state). Sends SIGTERM through the manual-kill path (now routed
/// via `governor.send_sigterm` post-D83/C1 — pending_kills carries
/// identity tokens).
///
/// DISPATCH 83 / C2 — on successful SIGTERM, the card is RE-OPENED
/// in `Waiting` state so the operator can monitor for the
/// cooperative exit OR press Enter again to force-SIGKILL through
/// the consent-gated path. On failure (PID gone, EPERM), the card
/// is dropped — the SIGTERM never actually went out, so there's
/// nothing to escalate from.
///
/// The PID is PINNED at card-open time: even if `selected_pid(state)`
/// has drifted to a different workload between card-open and confirm,
/// the kill fires on the operator-chosen target. Same invariant the
/// v0.3.x ARMED banner enforced via `ArmedKill::pid`.
fn confirm_kill_from_card(runtime: &mut Runtime, app: &mut App, card: KillConfirmCard) {
    let reason = "manual kill via TUI (kill_confirm Enter — SIGTERM)".to_string();
    let pid = card.pid;
    let name = card.display_name.clone();
    match runtime.manual_kill(pid, reason) {
        Ok(()) => {
            // DISPATCH 83 / C2 — transition the card into Waiting
            // so the operator can either watch the PID exit (the
            // SIGTERM-honoring case — most processes) OR force a
            // SIGKILL via a second Enter (the SIGTERM-ignoring
            // case — ollama, teleop). The grace window is the
            // operator's configured `policy.sigterm_grace_secs`.
            let grace_secs = runtime.config().policy.sigterm_grace_secs;
            let waiting_card = card.into_waiting(grace_secs);
            app.open_kill_confirm(waiting_card);
            // Status footer surfaces the SIGTERM that just fired —
            // operator sees confirmation of WHICH signal went out
            // (matches the WAITING_PROMPT's "SIGTERM sent" phrasing).
            let footer = ux_contract::status::KILL_SENT
                .replace("{name}", &name)
                .replace("{pid}", &pid.to_string());
            app.set_status(footer);
        }
        Err(e) => {
            tracing::warn!(pid, error = %e, "manual kill (SIGTERM) failed");
            // Card consumed; no Waiting transition. The error path
            // already failed at SIGTERM, so there's nothing in
            // pending_kills for the operator to escalate against.
        }
    }
}

/// DISPATCH 83 / C3 — second-Enter dispatch for the kill_confirm
/// card (Waiting state). The operator has consented to force the
/// uncatchable SIGKILL on a PID that survived the cooperative
/// SIGTERM. Routes through `Runtime::manual_force_kill`, which
/// goes through `governor.send_sigkill` (PID-reuse guard ALWAYS
/// engages — identity tokens captured at `manual_kill` SIGTERM
/// time are re-verified here).
///
/// The card is taken by value before this is called, so the
/// overlay drops on the next frame regardless of outcome — the
/// operator should not see the Waiting prompt linger after their
/// Enter press.
///
/// Status footer:
///   * Success → `KILL_FORCE_SENT` (operator sees confirmation
///     that SIGKILL was delivered — distinct from `KILL_SENT`
///     which names SIGTERM).
///   * Failure → log only (operator can read the audit panel for
///     the PidReusedAborted or OS error message).
fn force_kill_from_card(runtime: &mut Runtime, app: &mut App, card: KillConfirmCard) {
    let pid = card.pid;
    let name = card.display_name.clone();
    match runtime.manual_force_kill(pid) {
        Ok(()) => {
            let footer = ux_contract::status::KILL_FORCE_SENT
                .replace("{name}", &name)
                .replace("{pid}", &pid.to_string());
            app.set_status(footer);
        }
        Err(e) => {
            tracing::warn!(pid, error = %e, "manual force-kill (SIGKILL) failed");
        }
    }
}

/// DISPATCH 83 / C2 — auto-dismiss a Waiting-state kill_confirm
/// card whose targeted PID has exited (the SIGTERM-honoring case).
/// Called once per tick after `runtime.tick()` so the next render
/// frame drops the overlay; without this, a card opened on a
/// cooperative process would render its "waiting Ns…" prompt for
/// the full grace window even though there's nothing to wait for.
///
/// Confirm-state cards are NOT auto-dismissed: an operator who
/// opened the card on a process that died before they pressed
/// Enter still gets to see the prompt (and the SIGTERM dispatch
/// will surface `manual_kill`'s own error, which is louder than
/// silently dropping the card).
fn auto_dismiss_waiting_card_if_target_exited(runtime: &Runtime, app: &mut App) {
    let Some(card) = app.kill_confirm() else {
        return;
    };
    if !card.is_waiting() {
        return;
    }
    let pid = card.pid;
    let still_present = runtime
        .state()
        .last_lifecycle
        .as_ref()
        .map(|lc| lc.processes.contains_key(&pid))
        .unwrap_or(false);
    if !still_present {
        app.dismiss_kill_confirm();
    }
}

/// CAR-17 — build a `KillConfirmCard` snapshot for the focused PID.
/// Returns `None` when the PID isn't currently in `state.annotated`
/// (focus blip between key press and snapshot read) — the dispatch
/// site treats that as "leave any existing card alone."
///
/// Snapshots all renderable fields at open time so the card is pure
/// data the renderer can paint without re-walking the runtime state
/// every frame. Same approach as `LiveDetail::from_focused`.
fn build_kill_confirm_card(
    runtime: &Runtime,
    app: &App,
    pid: u32,
) -> Option<KillConfirmCard> {
    use crate::ui::panels::workloads;

    let state = runtime.state();
    let proc = state.annotated.iter().find(|p| p.pid == pid)?;

    // Sprint-7 Item 2 — kill_confirm card title preference:
    //   * Real model name (e.g. `qwen2.5-0.5b-instruct-q8_0`) is
    //     more identifying than the process name (`python3`) and
    //     wins outright.
    //   * Ollama's content-hash blob names (`sha256-XXX…`) are
    //     unreadable as a workload identifier — fall back to the
    //     process name in that case so the card title reads
    //     `ollama` not a 71-character hex string.
    //   * No model name → process name.
    let display_name = match proc.model_name.as_deref() {
        Some(m) if !m.starts_with("sha256-") => m.to_string(),
        _ => proc.name.clone(),
    };
    let category = format!("{:?}", proc.workload_category);
    let status = format!("{:?}", workloads::status_for(proc, state, app));
    let runtime_secs = proc.first_observed_at.elapsed().as_secs();
    let rss_mb = proc.rss_mb;
    let vram_mb = proc
        .vram_bytes
        .map(|b| b / (1024 * 1024))
        .filter(|&v| v > 0);
    let allowlisted = runtime.is_allowlisted(&proc.name);

    Some(KillConfirmCard::new(
        display_name,
        proc.pid,
        category,
        status,
        runtime_secs,
        proc.cpu_pct,
        rss_mb,
        vram_mb,
        allowlisted,
    ))
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
