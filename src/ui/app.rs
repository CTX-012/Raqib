use std::time::{Duration, Instant};

use crate::runtime::RuntimeState;
use crate::storage::RunRecord;
// v1.1.11 / DISPATCH 36 — `WorkloadRef` is no longer used by `App`
// directly (its `observe_alerts` and `observe_exit` moved to
// `Runtime`). The two test sites that still need it import it
// locally inside their function bodies.
use crate::ui::panels::TopProcessesSort;
use crate::ui::panels::kill_confirm::KillConfirmCard;
use crate::ui::panels::postmortem::PostMortemCard;
use crate::ui::symbols::SymbolSet;

/// How long an ephemeral status footer message stays on screen
/// before `tick_overlays` clears it. Long enough to read, short
/// enough not to mask the keybind hints permanently.
pub(crate) const STATUS_TTL: Duration = Duration::from_secs(3);

/// L2a / UX_CONTRACT.md §6 — input actions are owned by `ux_contract`.
/// The contract variants (Quit, ToggleHelp, SelectUp, SelectDown,
/// KillOrConfirm, OpenDetail, ToggleHistory, AcknowledgeAlerts,
/// CycleTopSort, EscapeCascade) are the active surface. New keymaps
/// go through Agent A via a contract amendment, not through this
/// re-export.
pub use ux_contract::Action;

// L2a introduced a transitional `LegacyAction` enum and a `Dispatch`
// wrapper to carry bindings the contract didn't define. L2b removed
// Group D (`d`/`v`/Tab/BackTab) and L2c removes the filter family.
// Both enums are gone now: `input::translate` returns `Option<Action>`
// directly, and the §6 keymap is the entire input surface.

/// Cached per-model run history shown by the overlay. Loaded on `h`
/// keypress and cleared on Esc; not refreshed every frame to avoid
/// hammering the `RunStore` from the render path.
#[derive(Debug, Clone)]
pub struct HistoryOverlay {
    pub model: String,
    pub records: Vec<RunRecord>,
}

/// Pure state machine for the TUI. No I/O, no rendering. Cheap to clone.
///
/// L2b removed the `focus` and `detail_mode` fields together with the
/// Group D bindings (`d`/`v`/Tab/BackTab). L2c removed the `mode` and
/// `filter` fields together with the `/` filter UX (deferred to v1.1
/// per the contract). The v1.0 input surface is the §6 keymap and a
/// single selectable list (AI Workloads); selection state lives on
/// `selected`.
#[derive(Debug, Clone)]
pub struct App {
    selected: usize,
    show_help: bool,
    quit_requested: bool,
    /// CAR-17 — kill_confirm card. `Some(_)` while the modal is open
    /// after the user pressed `k` on the focused workload. Carries the
    /// workload snapshot the card renders, including the PID it will
    /// dispatch against on Enter. Replaces the v0.3.x `ArmedKill`
    /// banner state — there is no auto-dismiss timer, the operator
    /// must explicitly confirm (Enter) or cancel (Esc).
    kill_confirm: Option<KillConfirmCard>,
    /// Most recent post-mortem-eligible exit ([UX-2]). Latest wins;
    /// dismissed by Esc or auto at `PostMortemCard::WINDOW`.
    postmortem: Option<PostMortemCard>,
    /// `Some(_)` while the history overlay is open. Snapshotted on key
    /// press so subsequent ticks don't replace the records the user is
    /// reading.
    history: Option<HistoryOverlay>,
    /// Ephemeral status footer message + when it was set. Auto-cleared
    /// by `tick_overlays` after `STATUS_TTL`. Used to surface
    /// transient action feedback (alerts acknowledged, sort cycled,
    /// kill dispatched) so the operator gets confirmation a keypress
    /// was received.
    status: Option<(String, Instant)>,
    /// L4 / UX_CONTRACT.md §15 — symbol set resolved at TUI startup.
    /// Render sites must route status-dot rendering through
    /// `symbol_set.workload_status(status)` rather than calling
    /// `WorkloadStatus::symbol()` directly. Once-per-session — never
    /// re-evaluated after a resize or reconnect.
    symbol_set: SymbolSet,
    // v1.1.11 / DISPATCH 36 / Phase 3 step 1: `alerts: AlertState`
    // was on `App` from L5/L6 (deviation from the original L-plan
    // RuntimeState spec — see the L6 commit for the
    // session-scoped-UI-state rationale that held under v1.0.x's
    // UX-only audience). Phase 3 needs alerts emitted on every UI
    // mode (TUI, headless, web), so ownership returned to
    // `RuntimeState::alerts` where the tick path can drive it
    // regardless of whether `App` was constructed. The
    // kill_confirm-card → GovernorArmed bridge runs through
    // `Runtime::set_armed_pid` (the dispatcher calls it each tick
    // before `runtime.tick`).
    /// L14 / UX_CONTRACT.md §1 region 5 — current sort for the
    /// Top processes panel, cycled by `t` (`Action::CycleTopSort`).
    /// Session-scoped UI state, defaults to `Ram` per the §13
    /// default. Field placement mirrors `alerts` above — both
    /// belong on `App` rather than `RuntimeState` because they're
    /// UI-only state with no relevance to the platform/governor
    /// pipeline.
    top_processes_sort: TopProcessesSort,
    /// L19 — one-shot signal set by `handle_escape` when it dismisses
    /// a post-mortem card. Consumed by the dispatcher in
    /// `ui::apply_action` so it can ask `Runtime` to drop the matching
    /// transient stderr buffer. Lives on `App` rather than being
    /// returned from `handle_escape` so the existing
    /// `handle_escape -> bool` contract (consumed-or-quit) stays
    /// stable for the L24 cascade tests.
    dismissed_pid: Option<u32>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_symbol_set(SymbolSet::default())
    }

    /// Constructor used by `ui::run` to pin the symbol set resolved
    /// from the process locale at TUI startup. Tests use this to
    /// force a specific set without touching env vars.
    pub fn with_symbol_set(symbol_set: SymbolSet) -> Self {
        Self {
            selected: 0,
            show_help: false,
            quit_requested: false,
            kill_confirm: None,
            postmortem: None,
            history: None,
            status: None,
            symbol_set,
            top_processes_sort: TopProcessesSort::default(),
            dismissed_pid: None,
        }
    }

    // v1.1.11 / DISPATCH 36 — `alerts()`, `alerts_mut()`,
    // `observe_exit`, and `observe_alerts` were REMOVED from `App`:
    // the state machine moved to `RuntimeState::alerts`, the
    // exit-driven fire-path moved into `Runtime::tick` (which
    // drains its own `pending_exit_alerts` queue and calls
    // `observe_exit_alert` per event), and the metric-driven
    // per-tick path moved to `Runtime::observe_alerts` (also
    // called from `tick`). Consumers read via
    // `runtime.state().alerts()`; render-layer tests drive
    // `runtime.state_mut().alerts` directly.

    /// L7 — handle the `a` key by ack'ing every active alert and
    /// surfacing a transient status footer ("Acknowledged N alerts")
    /// per `ux_contract::status::ALERTS_ACKNOWLEDGED`. Silent when
    /// no alerts are active — pressing `a` on an empty alert region
    /// shouldn't pop a "Acknowledged 0 alerts" message at the user.
    /// Returns the count for the dispatch site / tests.
    ///
    /// v1.1.11 / DISPATCH 36 — the ack itself now goes through
    /// `Runtime::acknowledge_alerts` (the state machine lives on
    /// `RuntimeState`); this method stays on `App` because the
    /// status footer that echoes it ("Acknowledged N alerts") is
    /// transient UI state owned by `App`.
    pub fn acknowledge_alerts(&mut self, runtime: &mut crate::runtime::Runtime) -> usize {
        let count = runtime.acknowledge_alerts();
        if count > 0 {
            let msg = ux_contract::status::ALERTS_ACKNOWLEDGED
                .replace("{n}", &count.to_string());
            self.set_status(msg);
        }
        count
    }

    /// Current Top processes panel sort. Read by `panels::render`
    /// to pick the sort fn + panel title.
    pub fn top_processes_sort(&self) -> TopProcessesSort {
        self.top_processes_sort
    }

    /// L14 — handle the `t` key by advancing the Top processes
    /// sort cyclically (Ram → Cpu → Vram → Ram) and surfacing a
    /// transient status footer ("Top processes sorted by {dim}")
    /// per `ux_contract::status::TOP_SORT_CHANGED`. The status
    /// echo mirrors the §6 pattern for every other action key
    /// (KILL_ARMED, ALERTS_ACKNOWLEDGED) — the contract template
    /// exists specifically for this action, and leaving it unused
    /// would be the anomaly.
    pub fn cycle_top_sort(&mut self) {
        self.top_processes_sort = self.top_processes_sort.next();
        let msg = ux_contract::status::TOP_SORT_CHANGED
            .replace("{dimension}", self.top_processes_sort.dimension_label());
        self.set_status(msg);
    }

    // v1.1.11 / DISPATCH 36 — App's `observe_alerts` method was
    // lifted to `Runtime::observe_alerts` (called from
    // `Runtime::tick`). The pre-v1.1.11 doc-comment block lived
    // here and described the per-tick metric scan; that
    // description now lives on the lifted method in `runtime.rs`.
    // The dispatcher in `ui/mod.rs` forwards
    // `app.kill_confirm_pid()` via `runtime.set_armed_pid` each
    // tick so the GovernorArmed eval still has the same input
    // shape it did when it lived on App.

    /// Symbol set resolved at TUI startup. See `ui::symbols`.
    pub fn symbol_set(&self) -> SymbolSet {
        self.symbol_set
    }

    /// Set an ephemeral status footer message. Replaces any prior
    /// message; auto-clears after `STATUS_TTL` via `tick_overlays`.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// Currently visible status footer message, or `None` if no
    /// message is set or the TTL has lapsed.
    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().and_then(|(s, t)| {
            if t.elapsed() < STATUS_TTL {
                Some(s.as_str())
            } else {
                None
            }
        })
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }
    pub fn show_help(&self) -> bool {
        self.show_help
    }
    pub fn should_quit(&self) -> bool {
        self.quit_requested
    }
    /// PID the open kill_confirm card targets, or `None` when no card
    /// is open. Read by `observe_alerts` to fire `AlertId::GovernorArmed`
    /// and by the workloads panel to highlight the targeted row.
    pub fn kill_confirm_pid(&self) -> Option<u32> {
        self.kill_confirm.as_ref().map(|c| c.pid)
    }

    /// Full kill_confirm card snapshot for the overlay renderer. `None`
    /// when no card is open.
    pub fn kill_confirm(&self) -> Option<&KillConfirmCard> {
        self.kill_confirm.as_ref()
    }

    /// Most recent post-mortem-eligible exit. `None` when no card is
    /// active. Used by `panels::render` to draw the centered overlay.
    pub fn postmortem(&self) -> Option<&PostMortemCard> {
        self.postmortem.as_ref()
    }

    pub fn request_quit(&mut self) {
        self.quit_requested = true;
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// CAR-17 — open the kill_confirm card on `card`. Replaces any
    /// prior card (e.g. operator pressed `k` on a new row before
    /// confirming the previous prompt). Caller resolves the workload
    /// snapshot from the live `RuntimeState` so the renderer doesn't
    /// re-walk state every frame.
    pub fn open_kill_confirm(&mut self, card: KillConfirmCard) {
        self.kill_confirm = Some(card);
    }

    /// Dismiss the kill_confirm card without firing. Called by the
    /// Esc cascade and on focus shifts (`select_next` / `select_prev`)
    /// for the same safety reason the v0.3.x banner cleared on nav.
    pub fn dismiss_kill_confirm(&mut self) {
        self.kill_confirm = None;
    }

    /// CAR-17 — take the kill_confirm card for Enter-confirm dispatch.
    /// Returns `Some(card)` when one is open; the slot becomes empty
    /// so the next render frame drops the overlay. Returns `None`
    /// when no card is open — Enter then falls through to the
    /// live_detail / post_mortem dispatch path.
    ///
    /// Pinning the kill target on the *card* (not on `selected_pid`)
    /// is the safety invariant the v0.3.x ARMED banner enforced via
    /// `ArmedKill::pid` — it survives here because the card carries
    /// the same field.
    pub fn take_kill_confirm(&mut self) -> Option<KillConfirmCard> {
        self.kill_confirm.take()
    }

    /// Push a new post-mortem snapshot. Latest wins; any existing card
    /// is replaced (no queue). Triggered from the runtime's exit hook
    /// and from the exec wrapper's exit path.
    pub fn show_postmortem(&mut self, card: PostMortemCard) {
        self.postmortem = Some(card);
    }

    /// Clear the post-mortem card. Triggered by `Enter`, by the
    /// cascading `Esc` priority, or by `tick_overlays` when the
    /// 30-second window lapses.
    ///
    /// L19 — funnels through `dismissed_pid` so the dispatcher's
    /// post-Esc hook can drop the matching `Runtime` stderr buffer.
    /// `tick_overlays`'s auto-dismiss path also sets the signal,
    /// but by that time the buffer has been swept by
    /// `Runtime::sweep_expired_stderr` anyway (both fire at the
    /// 30 s mark) — the dispatcher's `clear_stderr` call is a no-op
    /// when the entry is already gone.
    pub fn dismiss_postmortem(&mut self) {
        if let Some(card) = self.postmortem.take() {
            self.dismissed_pid = card.pid;
        }
    }

    /// L19 — take-and-clear the most recent dismissed-card PID. The
    /// dispatcher in `ui::apply_action` calls this after every Esc
    /// to find out whether the cascade just dismissed a card, and
    /// if so for which PID, so it can ask `Runtime` to drop the
    /// matching transient stderr buffer. `None` when no card was
    /// dismissed this turn or the dismissed card had no PID context.
    pub fn take_dismissed_pid(&mut self) -> Option<u32> {
        self.dismissed_pid.take()
    }

    /// Drop expired post-mortem snapshots and stale status footers.
    /// Called once per render tick (10 Hz). No I/O, no side effects
    /// beyond the `App`'s own state. CAR-17 — the kill_confirm card
    /// has no auto-dismiss window (the operator must explicitly
    /// confirm or cancel), so it is not swept here.
    pub fn tick_overlays(&mut self) {
        if let Some(card) = &self.postmortem
            && card.is_expired()
        {
            // L19 — funnel through `dismiss_postmortem` so the auto-
            // dismiss path also signals `dismissed_pid`. The runtime
            // stderr buffer for this PID has already been swept by
            // `Runtime::sweep_expired_stderr` at the same 30 s mark,
            // so the dispatcher's `clear_stderr` call is a no-op
            // here — but signalling keeps the two dismiss paths
            // (Esc / auto) behaviourally symmetric.
            self.dismiss_postmortem();
        }
        if let Some((_, t)) = &self.status
            && t.elapsed() >= STATUS_TTL
        {
            self.status = None;
        }
    }

    /// Cascading-priority Escape handler per UX_CONTRACT.md §6.
    ///
    /// Priority order:
    ///   1. kill_confirm card → dismiss without firing (CAR-17)
    ///   2. post-mortem card → dismiss
    ///   3. history or help overlay → close
    ///   4. alerts visible → acknowledge all (same effect as `a`)
    ///   5. nothing to dismiss → quit (same as `q`)
    ///
    /// Returns `true` when steps 1–4 consumed the press, `false`
    /// when step 5 fired.
    ///
    /// CAR-17 — the kill_confirm card sits at the front of the
    /// cascade because it's the destructive prompt: an Esc with a
    /// pending kill must always cancel the kill before being routed
    /// to any other overlay, never the other way around.
    ///
    /// L24 / §6 — step 4 sits after the overlay-close step on
    /// purpose. When the alert region is non-empty *and* history or
    /// help is also open, the first Esc closes the overlay; the
    /// operator has to press Esc a second time to ack the alerts.
    ///
    /// v1.1.11 / DISPATCH 36 — `&mut Runtime` parameter added so the
    /// §6 step 4 ack-alerts branch can reach the state machine on
    /// `RuntimeState` (lifted from `App` per Phase 3 step 1). The
    /// cascade semantics are unchanged: Esc still ack's alerts
    /// immediately, no one-tick delay.
    pub fn handle_escape(&mut self, runtime: &mut crate::runtime::Runtime) -> bool {
        if self.kill_confirm.is_some() {
            self.dismiss_kill_confirm();
            return true;
        }
        if self.postmortem.is_some() {
            // L19 — `dismiss_postmortem` funnels the dismissed
            // card's PID into `dismissed_pid` so the dispatcher
            // can ask `Runtime` to drop the matching transient
            // stderr buffer post-cascade.
            self.dismiss_postmortem();
            return true;
        }
        if self.history.is_some() {
            self.close_history();
            return true;
        }
        if self.show_help {
            self.show_help = false;
            return true;
        }
        // §6 step 4 — when no card / disarm / overlay is in the way
        // but alerts are visible on screen, Esc acknowledges them.
        // Sits below the overlay-close step so an Esc with history
        // or help open closes the overlay first.
        if runtime.state().alerts.active_count() > 0 {
            self.acknowledge_alerts(runtime);
            return true;
        }
        // §6 step 5: nothing to dismiss → quit.
        self.quit_requested = true;
        false
    }

    pub fn open_history(&mut self, model: String, records: Vec<RunRecord>) {
        self.history = Some(HistoryOverlay { model, records });
    }
    pub fn close_history(&mut self) {
        self.history = None;
    }
    pub fn history(&self) -> Option<&HistoryOverlay> {
        self.history.as_ref()
    }
    pub fn is_history_open(&self) -> bool {
        self.history.is_some()
    }

    pub fn select_next(&mut self, state: &RuntimeState) {
        let len = self.visible(state).len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1).min(len - 1);
        // CAR-17 safety invariant — focus drift cancels any pending
        // kill_confirm so the prompt's PID can never silently retarget.
        self.kill_confirm = None;
    }

    pub fn select_prev(&mut self, _state: &RuntimeState) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.kill_confirm = None;
    }

    /// Resolve the currently selected list to PIDs. Used both by `select_*`
    /// and by the kill action so they stay consistent.
    pub fn selected_pid(&self, state: &RuntimeState) -> Option<u32> {
        self.visible(state).get(self.selected).copied()
    }

    /// PIDs visible in the AI Workloads panel, in render order
    /// (grouped by `WorkloadCategory` per UX_CONTRACT.md §1 region 4,
    /// then sorted within group by status priority then PID).
    /// Selection navigation reads this so `j`/`K` move through the
    /// rows in the same order the user sees them on screen — group
    /// headers are not selectable; this method returns workload PIDs
    /// only.
    ///
    /// L2b removed the focus-panel switch; L2c removed the substring
    /// filter (deferred to v1.1); L11b moved sort-order ownership
    /// from this method to `panels::workloads::ordered_pids`.
    pub fn visible(&self, state: &RuntimeState) -> Vec<u32> {
        crate::ui::panels::workloads::ordered_pids(state, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AICategory;
    use crate::runtime::{AnnotatedProcess, RuntimeState};

    fn state_with(procs: Vec<AnnotatedProcess>) -> RuntimeState {
        RuntimeState {
            annotated: procs,
            ..Default::default()
        }
    }

    /// v1.1.11 / DISPATCH 36 — `handle_escape` and
    /// `acknowledge_alerts` now take `&mut Runtime` because the
    /// `AlertState` they drive lives on `RuntimeState`. Tests
    /// construct a runtime via this helper.
    fn empty_runtime() -> crate::runtime::Runtime {
        crate::runtime::Runtime::new(crate::config::Config::default())
            .expect("Runtime::new must succeed with contract default config")
    }

    fn ann(pid: u32, name: &str, cat: AICategory) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: cat,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            // App-state tests don't exercise the Loading warmup gate;
            // any non-zero `Instant` works.
            first_observed_at: std::time::Instant::now(),
        }
    }

    fn fake_card(pid: u32, name: &str, allowlisted: bool) -> KillConfirmCard {
        KillConfirmCard::new(
            name.into(),
            pid,
            "LLM".into(),
            "Running".into(),
            42,
            17.0,
            512,
            None,
            allowlisted,
        )
    }

    #[test]
    fn visible_only_includes_ai() {
        let s = state_with(vec![
            ann(1, "ollama", AICategory::Inference),
            ann(2, "bash", AICategory::NotAi),
        ]);
        let app = App::new();
        let pids = app.visible(&s);
        assert_eq!(pids, vec![1]);
    }

    #[test]
    fn select_next_clamps_to_visible_len() {
        let s = state_with(vec![ann(1, "a", AICategory::Inference)]);
        let mut app = App::new();
        app.select_next(&s);
        app.select_next(&s);
        app.select_next(&s);
        assert_eq!(app.selected_index(), 0); // only one item, clamps
    }

    #[test]
    fn select_prev_does_not_underflow() {
        let s = state_with(vec![ann(1, "a", AICategory::Inference)]);
        let mut app = App::new();
        app.select_prev(&s);
        assert_eq!(app.selected_index(), 0);
    }

    #[test]
    fn open_then_dismiss_kill_confirm_clears_slot() {
        let mut app = App::new();
        app.open_kill_confirm(fake_card(42, "ollama", false));
        assert_eq!(app.kill_confirm_pid(), Some(42));
        app.dismiss_kill_confirm();
        assert_eq!(app.kill_confirm_pid(), None);
    }

    /// CAR-17 safety invariant: any navigation movement dismisses a
    /// pending kill_confirm card so the operator can't accidentally
    /// fire on a different PID after the selection drifts. Inherited
    /// from the v0.3.x ARMED banner invariant (which itself survived
    /// the L2b focus-mechanism removal). Don't remove as redundant.
    #[test]
    fn select_dismisses_kill_confirm_for_safety() {
        let s = state_with(vec![
            ann(1, "ollama", AICategory::Inference),
            ann(2, "vllm", AICategory::Inference),
        ]);
        let mut app = App::new();
        app.open_kill_confirm(fake_card(42, "ollama", false));
        app.select_next(&s);
        assert_eq!(app.kill_confirm_pid(), None);
    }

    #[test]
    fn open_kill_confirm_records_pid_and_workload_fields() {
        let mut app = App::new();
        app.open_kill_confirm(fake_card(4242, "ollama", false));
        let card = app.kill_confirm().expect("card open");
        assert_eq!(card.pid, 4242);
        assert_eq!(card.display_name, "ollama");
        assert!(!card.allowlisted);
    }

    // ── CAR-17 — Enter-confirm dispatch surface ──────────────────

    #[test]
    fn take_kill_confirm_returns_none_when_no_card() {
        let mut app = App::new();
        assert!(app.take_kill_confirm().is_none());
    }

    #[test]
    fn take_kill_confirm_returns_and_clears_slot() {
        // Enter on the kill_confirm card must take ownership of the
        // snapshot AND clear the slot atomically so the next render
        // frame drops the overlay.
        let mut app = App::new();
        app.open_kill_confirm(fake_card(4242, "ollama", false));
        let taken = app.take_kill_confirm().expect("card was open");
        assert_eq!(taken.pid, 4242);
        assert_eq!(taken.display_name, "ollama");
        assert!(
            app.kill_confirm().is_none(),
            "take_* must clear the slot to match Option::take semantics",
        );
    }

    /// CAR-17 PID-pinning invariant: the kill_confirm card carries
    /// `pid` as a frozen `u32`. Subsequent state reshuffles (workload
    /// list sort order changes between vitals refreshes) must not
    /// retarget the card. Confirm-dispatch reads from the card, not
    /// from `selected_pid(state)`.
    #[test]
    fn kill_confirm_pid_pinned_across_selected_pid_shifts() {
        let s_first = state_with(vec![
            ann(101, "a", AICategory::Inference),
            ann(202, "b", AICategory::Inference),
        ]);
        let mut app = App::new();
        let focused_a = app.selected_pid(&s_first).expect("first PID");
        app.open_kill_confirm(fake_card(focused_a, "a", false));

        // Reshuffle: same selected=0, different PID set.
        let s_after = state_with(vec![
            ann(303, "c", AICategory::Inference),
            ann(202, "b", AICategory::Inference),
        ]);
        let focused_after = app.selected_pid(&s_after).expect("first PID after shift");
        assert_ne!(
            focused_a, focused_after,
            "precondition: selected_pid must drift across the state reshuffle"
        );

        let card = app.kill_confirm().expect("card still open");
        assert_eq!(
            card.pid, focused_a,
            "card PID must be pinned at open time, not recomputed from selected_pid"
        );

        let taken = app.take_kill_confirm().expect("card open");
        assert_eq!(taken.pid, focused_a);
    }

    #[test]
    fn esc_dismisses_kill_confirm_when_no_postmortem_present() {
        let mut app = App::new();
        let mut runtime = empty_runtime();
        app.open_kill_confirm(fake_card(4242, "ollama", false));
        let consumed = app.handle_escape(&mut runtime);
        assert!(consumed);
        assert!(app.kill_confirm().is_none());
    }

    /// CAR-17 — kill_confirm sits at the FRONT of the Esc cascade
    /// (above postmortem). The destructive prompt must be canceled
    /// before any other overlay is touched, never the other way
    /// around.
    #[test]
    fn esc_dismisses_kill_confirm_before_postmortem() {
        let mut app = App::new();
        let mut runtime = empty_runtime();
        app.open_kill_confirm(fake_card(4242, "ollama", false));
        app.show_postmortem(test_card());
        let consumed = app.handle_escape(&mut runtime);
        assert!(consumed);
        assert!(
            app.kill_confirm().is_none(),
            "kill_confirm must dismiss first — destructive prompt has top priority"
        );
        assert!(
            app.postmortem().is_some(),
            "post-mortem card must survive the first Esc when kill_confirm is open"
        );
    }

    /// §6 step 5 — when no overlay / kill_confirm / card is present,
    /// Esc falls through to quit. Returns `false`; the quit signal
    /// lives in `quit_requested`.
    #[test]
    fn esc_quits_when_nothing_to_dismiss() {
        let mut app = App::new();
        let mut runtime = empty_runtime();
        assert!(app.postmortem().is_none());
        assert!(app.kill_confirm().is_none());
        assert!(!app.is_history_open());
        assert!(!app.show_help());

        let consumed = app.handle_escape(&mut runtime);
        assert!(
            !consumed,
            "fall-through-to-quit must return false to distinguish \
             from a card/dismiss/overlay-close consumption",
        );
        assert!(
            app.should_quit(),
            "Esc with nothing to dismiss must request quit per UI Contract v2",
        );
    }

    /// CAR-17 — the kill_confirm card has NO auto-dismiss timer; it
    /// stays open until the operator explicitly confirms or cancels.
    /// `tick_overlays` must not sweep it away.
    #[test]
    fn tick_overlays_does_not_drop_open_kill_confirm() {
        let mut app = App::new();
        app.open_kill_confirm(fake_card(4242, "ollama", false));
        for _ in 0..100 {
            app.tick_overlays();
        }
        assert!(
            app.kill_confirm().is_some(),
            "kill_confirm card must persist across ticks — only explicit Enter/Esc dismisses it"
        );
    }

    #[test]
    fn tick_overlays_drops_expired_postmortem() {
        let mut app = App::new();
        let mut card = test_card();
        card.shown_at = std::time::Instant::now() - std::time::Duration::from_secs(31);
        app.show_postmortem(card);
        app.tick_overlays();
        assert!(app.postmortem().is_none());
    }

    fn test_card() -> PostMortemCard {
        // UI Contract v2 — `PostMortemCard` wraps a transient
        // `PostMortem` (not a `RunRecord` clone). Build a minimal
        // fixture for App-level state tests; the card's render
        // contract is exercised by the postmortem panel's own tests.
        use crate::storage::run_store::ExitReason;
        use crate::ui::panels::postmortem::{BaselineStatus, PostMortem};
        PostMortemCard {
            post_mortem: PostMortem {
                display_name: "phi3-mini".into(),
                duration_secs: 42,
                avg_cpu_pct: 0.0,
                peak_cpu_pct: 0.0,
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                tokens_per_sec: None,
                workload_category: None,
                exit_reason: ExitReason::CleanExit,
                stderr_tail: Vec::new(),
                baseline_status: BaselineStatus::NotAvailable,
            },
            shown_at: std::time::Instant::now(),
            pid: None,
        }
    }

    #[test]
    fn quit_request_propagates() {
        let mut app = App::new();
        assert!(!app.should_quit());
        app.request_quit();
        assert!(app.should_quit());
    }

    #[test]
    fn default_app_uses_unicode_symbol_set() {
        // App::new() defaults to Unicode (matches the SymbolSet
        // default and the contract's preferred glyphs). ui::run
        // overrides this with the detected set at startup.
        let app = App::new();
        assert_eq!(app.symbol_set(), SymbolSet::Unicode);
        assert_eq!(
            app.symbol_set()
                .workload_status(ux_contract::WorkloadStatus::Healthy),
            "●"
        );
    }

    #[test]
    fn app_with_ascii_symbol_set_renders_ascii_glyphs() {
        let app = App::with_symbol_set(SymbolSet::Ascii);
        assert_eq!(app.symbol_set(), SymbolSet::Ascii);
        assert_eq!(
            app.symbol_set()
                .workload_status(ux_contract::WorkloadStatus::Healthy),
            "*"
        );
        assert_eq!(
            app.symbol_set()
                .workload_status(ux_contract::WorkloadStatus::Critical),
            "X"
        );
    }

    // ====================================================================
    // L7 / UX_CONTRACT.md §6 — `acknowledge_alerts` dispatch helper.
    // ====================================================================

    /// Pressing `a` with nothing in the alert region must not pop
    /// "Acknowledged 0 alerts" at the user — that would teach an
    /// operator that the keymap noise is normal. Silent no-op.
    #[test]
    fn acknowledge_alerts_is_silent_when_none_active() {
        let mut app = App::new();
        let mut runtime = empty_runtime();
        let count = app.acknowledge_alerts(&mut runtime);
        assert_eq!(count, 0);
        assert_eq!(app.status(), None);
    }

    #[test]
    fn acknowledge_alerts_returns_count_and_sets_status_when_active() {
        use crate::ui::alerts::WorkloadRef;
        let mut app = App::new();
        let mut runtime = empty_runtime();
        let now = std::time::Instant::now();
        // Two instant-fire alerts → two Active slots. State lives
        // on Runtime now per v1.1.11 ITEM 1.
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(206, "phi3"),
            ux_contract::AlertId::GovernorArmed,
            true,
        );
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(207, "vllm"),
            ux_contract::AlertId::OomDetected,
            true,
        );
        assert_eq!(runtime.state().alerts.visible().len(), 2);

        let count = app.acknowledge_alerts(&mut runtime);
        assert_eq!(count, 2);
        // Both moved to Suppressed → out of visible.
        assert_eq!(runtime.state().alerts.visible().len(), 0);
        // Status footer shows the contract template with {n} = 2.
        assert_eq!(app.status(), Some("Acknowledged 2 alerts"));
    }

    #[test]
    fn acknowledge_alerts_uses_contract_template_with_substitution() {
        // L7 design lock — the status string is sourced from
        // `ux_contract::status::ALERTS_ACKNOWLEDGED`, not a local
        // literal. Pin against the contract const so a future
        // local-literal regression breaks here, not silently.
        use crate::ui::alerts::WorkloadRef;
        let mut app = App::new();
        let mut runtime = empty_runtime();
        let now = std::time::Instant::now();
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(206, "phi3"),
            ux_contract::AlertId::GovernorArmed,
            true,
        );
        app.acknowledge_alerts(&mut runtime);
        let status = app.status().unwrap_or("");
        let expected = ux_contract::status::ALERTS_ACKNOWLEDGED.replace("{n}", "1");
        assert_eq!(status, expected);
    }

    // ── L14 — Top processes sort cycle ────────────────────────────

    #[test]
    fn cycle_top_sort_advances_through_ram_cpu_vram() {
        // Default starts at Ram (§13). One press → Cpu, two → Vram.
        let mut app = App::new();
        assert_eq!(app.top_processes_sort(), TopProcessesSort::Ram);
        app.cycle_top_sort();
        assert_eq!(app.top_processes_sort(), TopProcessesSort::Cpu);
        app.cycle_top_sort();
        assert_eq!(app.top_processes_sort(), TopProcessesSort::Vram);
    }

    #[test]
    fn cycle_top_sort_wraps_back_to_ram_after_vram() {
        // Three presses round-trip to Ram. Locks the cyclic
        // contract — a fourth state would silently break this.
        let mut app = App::new();
        for _ in 0..3 {
            app.cycle_top_sort();
        }
        assert_eq!(app.top_processes_sort(), TopProcessesSort::Ram);
    }

    #[test]
    fn top_processes_panel_sort_persists_across_ticks() {
        // The sort is session-scoped UI state — `tick_overlays`
        // (the per-frame decay path) and unrelated key actions
        // must not reset it. Cycle once, exercise tick + selection
        // + help-toggle, then verify sort is still on Cpu.
        let mut app = App::new();
        app.cycle_top_sort();
        assert_eq!(app.top_processes_sort(), TopProcessesSort::Cpu);

        let state = RuntimeState::default();
        for _ in 0..5 {
            app.tick_overlays();
            app.select_next(&state);
            app.toggle_help();
        }
        assert_eq!(
            app.top_processes_sort(),
            TopProcessesSort::Cpu,
            "sort must survive ticks + unrelated state changes",
        );
    }

    #[test]
    fn cycle_top_sort_uses_contract_template_with_substitution() {
        // L14 design lock — status footer string comes from
        // `ux_contract::status::TOP_SORT_CHANGED`, not a local
        // literal. Mirrors the L7 contract-template lock for
        // ALERTS_ACKNOWLEDGED above.
        let mut app = App::new();
        app.cycle_top_sort(); // Ram → Cpu
        let status = app.status().unwrap_or("");
        let expected = ux_contract::status::TOP_SORT_CHANGED
            .replace("{dimension}", "CPU");
        assert_eq!(status, expected);
    }
}
