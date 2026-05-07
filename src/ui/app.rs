use std::time::{Duration, Instant};

use crate::runtime::RuntimeState;
use crate::storage::RunRecord;
use crate::ui::alerts::{AlertState, WorkloadRef};
use crate::ui::panels::armed_banner::ArmedKill;
use crate::ui::panels::postmortem::PostMortemCard;
use crate::ui::symbols::SymbolSet;

/// How long an ephemeral status footer message stays on screen
/// before `tick_overlays` clears it. Mirrors the operator-feedback
/// rhythm of the armed-kill banner: long enough to read, short
/// enough not to mask the keybind hints permanently.
pub(crate) const STATUS_TTL: Duration = Duration::from_secs(3);

/// L2a / UX_CONTRACT.md §6 — input actions are owned by `ux_contract`.
/// The 11 contract variants (Quit, ToggleHelp, SelectUp, SelectDown,
/// KillOrConfirm, OpenDetail, OpenGrafana, ToggleHistory,
/// AcknowledgeAlerts, CycleTopSort, EscapeCascade) are the locked
/// surface. New keymaps go through Agent A via a contract amendment,
/// not through this re-export.
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
    /// Two-stage manual-kill ([UX-1]). `Some(_)` after the first `k`
    /// press; auto-disarms after `ArmedKill::WINDOW`. Carries pid +
    /// name + allowlisted so the banner can render without re-reading
    /// the runtime state on every frame.
    armed_kill: Option<ArmedKill>,
    /// Most recent post-mortem-eligible exit ([UX-2]). Latest wins;
    /// dismissed by Esc or auto at `PostMortemCard::WINDOW`.
    postmortem: Option<PostMortemCard>,
    /// `Some(_)` while the history overlay is open. Snapshotted on key
    /// press so subsequent ticks don't replace the records the user is
    /// reading.
    history: Option<HistoryOverlay>,
    /// Ephemeral status footer message + when it was set. Auto-cleared
    /// by `tick_overlays` after `STATUS_TTL`. Used to surface kill-flow
    /// feedback (especially the dry-run "would-have-sent" message) so
    /// the operator gets confirmation a keypress was received even
    /// when the underlying signal was suppressed.
    status: Option<(String, Instant)>,
    /// L4 / UX_CONTRACT.md §15 — symbol set resolved at TUI startup.
    /// Render sites must route status-dot rendering through
    /// `symbol_set.workload_status(status)` rather than calling
    /// `WorkloadStatus::symbol()` directly. Once-per-session — never
    /// re-evaluated after a resize or reconnect.
    symbol_set: SymbolSet,
    /// L5/L6 / UX_CONTRACT.md §4 — alert state machine. Lives on
    /// `App` (not `RuntimeState`) because acks are session-scoped UI
    /// state and one of the breach inputs (`armed_kill_pid`) lives
    /// on `App` already; keeping the state on the same shelf
    /// avoids an awkward cross-boundary dispatch every tick. The L
    /// plan row originally spec'd `RuntimeState`; deviation is
    /// documented in the L6 commit.
    alerts: AlertState,
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
            armed_kill: None,
            postmortem: None,
            history: None,
            status: None,
            symbol_set,
            alerts: AlertState::new(),
        }
    }

    /// Read-only access to the alert state machine. The render
    /// layer (`panels::alerts`) calls `app.alerts().visible()` and
    /// `app.alerts().active_count()`.
    pub fn alerts(&self) -> &AlertState {
        &self.alerts
    }

    /// Mutable access for tests and the per-tick observation path.
    pub fn alerts_mut(&mut self) -> &mut AlertState {
        &mut self.alerts
    }

    /// L8 — fire the alert(s) queued by the runtime's lifecycle
    /// exit hook. Called once per tick by the UI loop after
    /// `Runtime::drain_exit_alerts`. Each event resolves to a
    /// single `AlertState::observe_exit` call — no sustain gate,
    /// reason captured at fire time.
    pub fn observe_exit(&mut self, now: Instant, event: &crate::runtime::ExitAlertEvent) {
        self.alerts.observe_exit(
            now,
            WorkloadRef::workload(event.pid, &event.workload_name),
            event.alert_id,
            event.reason.clone(),
        );
    }

    /// L7 — handle the `a` key by ack'ing every active alert and
    /// surfacing a transient status footer ("Acknowledged N alerts")
    /// per `ux_contract::status::ALERTS_ACKNOWLEDGED`. Silent when
    /// no alerts are active — pressing `a` on an empty alert region
    /// shouldn't pop a "Acknowledged 0 alerts" message at the user.
    /// Returns the count for the dispatch site / tests.
    pub fn acknowledge_alerts(&mut self) -> usize {
        let count = self.alerts.ack_all();
        if count > 0 {
            let msg = ux_contract::status::ALERTS_ACKNOWLEDGED
                .replace("{n}", &count.to_string());
            self.set_status(msg);
        }
        count
    }

    /// Per-tick alert observation. Reads the metrics that already
    /// flow through `RuntimeState` (system RAM, total VRAM,
    /// per-process VRAM, KV cache occupancy) plus `App`'s own
    /// `armed_kill_pid` and dispatches `(workload, alert_id,
    /// breaching)` flags into the alert state machine.
    ///
    /// Out of scope for L6 (lands in L8): `OomDetected` and
    /// `WorkloadExited`. Both are exit-driven instant-fire alerts
    /// whose natural firing site is the lifecycle exit hook, not a
    /// per-tick metric scan.
    pub fn observe_alerts(&mut self, now: Instant, state: &RuntimeState) {
        use ux_contract::AlertId;
        use ux_contract::thresholds::{KV_ATTENTION_PCT, RAM_ATTENTION_PCT, VRAM_ATTENTION_PCT};

        // RAM pressure — system-scope, only one slot for the whole
        // host.
        let ram_pct = state
            .last_snapshot
            .as_ref()
            .map(|s| s.system.memory_usage_percent());
        let ram_breaching = ram_pct.is_some_and(|p| p >= RAM_ATTENTION_PCT);
        self.alerts.observe(
            now,
            WorkloadRef::system(),
            AlertId::RamPressure,
            ram_breaching,
        );

        // Per-AI-PID alerts. Snapshot the PIDs and names up front so
        // the borrow on `state.ai_processes()` is released before
        // we mutate `self.alerts` in the loop.
        let total_vram = state
            .last_snapshot
            .as_ref()
            .map(|s| s.gpu.total_vram_all_devices())
            .filter(|&v| v > 0);
        let armed_pid = self.armed_kill_pid();
        let workloads: Vec<(u32, String, Option<u64>, Option<f32>)> = state
            .ai_processes()
            .map(|p| {
                let kv = state
                    .live_telemetry
                    .get(&p.pid)
                    .and_then(|lt| lt.kv_cache_peak_pct);
                (p.pid, p.name.clone(), p.vram_bytes, kv)
            })
            .collect();

        for (pid, name, vram_bytes, kv_pct) in &workloads {
            let workload = WorkloadRef::workload(*pid, name);

            // VRAM: device-relative percentage. `{pct}` is rendered
            // from the same numerator/denominator at render time
            // (see panels::alerts::live_values_for) — the threshold
            // check here only needs the boolean.
            let vram_pct = match (total_vram, *vram_bytes) {
                (Some(total), Some(used)) => Some((used as f64 / total as f64) * 100.0),
                _ => None,
            };
            let vram_breaching = vram_pct.is_some_and(|p| p >= VRAM_ATTENTION_PCT);
            self.alerts
                .observe(now, workload, AlertId::VramPressure, vram_breaching);

            // KV cache: LLM-only signal; non-LLM workloads have no
            // KV reading and therefore can't breach.
            let kv = kv_pct.map(|v| v as f64);
            let kv_breaching = kv.is_some_and(|p| p >= KV_ATTENTION_PCT);
            self.alerts
                .observe(now, workload, AlertId::KvPressure, kv_breaching);

            // GovernorArmed: this PID is the one currently armed.
            // Instant fire; clears as soon as the arm is released.
            let armed = armed_pid == Some(*pid);
            self.alerts
                .observe(now, workload, AlertId::GovernorArmed, armed);
        }
    }

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
    pub fn armed_kill_pid(&self) -> Option<u32> {
        self.armed_kill.as_ref().map(|a| a.pid)
    }

    /// Full armed-kill state for the banner panel. `None` when no kill
    /// is armed.
    pub fn armed_kill(&self) -> Option<&ArmedKill> {
        self.armed_kill.as_ref()
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

    /// Arm a kill on `armed`. Replaces any prior arm (e.g. user moves
    /// focus and re-arms on a new row). The caller is expected to
    /// resolve `name` + `allowlisted` against the runtime at the time
    /// of the keypress so the banner doesn't have to re-resolve them
    /// on every render frame.
    pub fn arm_kill(&mut self, armed: ArmedKill) {
        self.armed_kill = Some(armed);
    }
    pub fn disarm_kill(&mut self) {
        self.armed_kill = None;
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
    pub fn dismiss_postmortem(&mut self) {
        self.postmortem = None;
    }

    /// Drop expired armed-kill / post-mortem snapshots. Called once
    /// per render tick (10 Hz). No I/O, no side effects beyond the
    /// `App`'s own state — the loop runs even when the user isn't
    /// pressing keys, so countdown displays decay smoothly.
    pub fn tick_overlays(&mut self) {
        if let Some(armed) = &self.armed_kill
            && armed.is_expired()
        {
            self.armed_kill = None;
        }
        if let Some(card) = &self.postmortem
            && card.is_expired()
        {
            self.postmortem = None;
        }
        if let Some((_, t)) = &self.status
            && t.elapsed() >= STATUS_TTL
        {
            self.status = None;
        }
    }

    /// Cascading-priority Escape handler per UI Contract v2.
    ///
    /// Priority order:
    ///   1. post-mortem card → dismiss
    ///   2. armed kill → disarm
    ///   3. other overlay (history, help) → close
    ///   4. nothing to dismiss → quit (same as `q`)
    ///
    /// Returns `true` when steps 1–3 consumed the press, `false`
    /// when step 4 fired. Either way `quit_requested` is set in
    /// the step-4 branch; callers can use the return to log
    /// `Esc → quit` differently from a `q`-quit if desired.
    pub fn handle_escape(&mut self) -> bool {
        if self.postmortem.is_some() {
            self.dismiss_postmortem();
            return true;
        }
        if self.armed_kill.is_some() {
            self.disarm_kill();
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
        // UI Contract v2 step 4: nothing to dismiss → quit.
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
        self.armed_kill = None;
    }

    pub fn select_prev(&mut self, _state: &RuntimeState) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.armed_kill = None;
    }

    /// Resolve the currently selected list to PIDs. Used both by `select_*`
    /// and by the kill action so they stay consistent.
    pub fn selected_pid(&self, state: &RuntimeState) -> Option<u32> {
        self.visible(state).get(self.selected).copied()
    }

    /// PIDs visible in the AI Workloads panel.
    /// Stable PID-sorted so user selection doesn't jump between ticks.
    /// L2b removed the focus-panel switch (Registry/Rogues/Culprits) —
    /// only the AI Workloads list remains in the v1.0 layout. L2c
    /// removed the substring filter (deferred to v1.1).
    pub fn visible(&self, state: &RuntimeState) -> Vec<u32> {
        let mut pids: Vec<u32> = state.ai_processes().map(|p| p.pid).collect();
        pids.sort();
        pids
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

    fn ann(pid: u32, name: &str, cat: AICategory) -> AnnotatedProcess {
        AnnotatedProcess {
            pid,
            name: name.into(),
            category: cat,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
        }
    }

    fn fake_armed(pid: u32, name: &str, allowlisted: bool) -> ArmedKill {
        ArmedKill {
            pid,
            name: name.into(),
            allowlisted,
            armed_at: std::time::Instant::now(),
        }
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
    fn arm_then_confirm_kill_clears_arm() {
        let mut app = App::new();
        app.arm_kill(fake_armed(42, "ollama", false));
        assert_eq!(app.armed_kill_pid(), Some(42));
        app.disarm_kill();
        assert_eq!(app.armed_kill_pid(), None);
    }

    /// Safety invariant: any navigation movement clears a pending arm
    /// so the user can't accidentally fire on a different PID after
    /// the selection drifts. This invariant survived the
    /// focus-mechanism removal in L2b — keep it locked even though
    /// `select_next` (j) and `select_prev` (K/Up) are the only nav
    /// paths now, and stays load-bearing if v1.1 ever re-introduces
    /// multi-panel focus. Don't remove this test as redundant.
    #[test]
    fn select_disarms_kill_for_safety() {
        let s = state_with(vec![
            ann(1, "ollama", AICategory::Inference),
            ann(2, "vllm", AICategory::Inference),
        ]);
        let mut app = App::new();
        app.arm_kill(fake_armed(42, "ollama", false));
        app.select_next(&s);
        assert_eq!(app.armed_kill_pid(), None);
    }

    #[test]
    fn arm_kill_records_pid_and_name_and_allowlisted() {
        let mut app = App::new();
        app.arm_kill(fake_armed(4242, "ollama", false));
        let armed = app.armed_kill().expect("should be armed");
        assert_eq!(armed.pid, 4242);
        assert_eq!(armed.name, "ollama");
        assert!(!armed.allowlisted);
        // Just-armed window has 5 integer seconds remaining.
        assert_eq!(armed.seconds_remaining(), 5);
    }

    #[test]
    fn esc_disarms_kill_when_no_postmortem_present() {
        let mut app = App::new();
        app.arm_kill(fake_armed(4242, "ollama", false));
        let consumed = app.handle_escape();
        assert!(consumed);
        assert!(app.armed_kill().is_none());
    }

    #[test]
    fn esc_dismisses_postmortem_in_priority_over_disarm() {
        let mut app = App::new();
        app.arm_kill(fake_armed(4242, "ollama", false));
        app.show_postmortem(test_card());
        let consumed = app.handle_escape();
        // Cascading priority: card cleared first; armed kill survives
        // this Esc and would need a second Esc to clear.
        assert!(consumed);
        assert!(app.postmortem().is_none());
        assert!(
            app.armed_kill().is_some(),
            "Esc should clear card before disarming",
        );
    }

    /// UI Contract v2 step 4 — when no overlay / armed kill / card is
    /// present, Esc falls through to quit. Matches the user's intuition
    /// that Esc means "get me out of whatever I'm in", and gives the
    /// keyboard a second route to quit alongside `q`. Returns `false`
    /// (only steps 1–3 return `true`); the quit signal lives in
    /// `quit_requested`, not in the return value.
    #[test]
    fn esc_quits_when_nothing_to_dismiss() {
        let mut app = App::new();
        // No card, no armed kill, no overlay, no help.
        assert!(app.postmortem().is_none());
        assert!(app.armed_kill().is_none());
        assert!(!app.is_history_open());
        assert!(!app.show_help());

        let consumed = app.handle_escape();
        assert!(
            !consumed,
            "fall-through-to-quit must return false to distinguish \
             from a card/disarm/overlay-close consumption",
        );
        assert!(
            app.should_quit(),
            "Esc with nothing to dismiss must request quit per UI Contract v2",
        );
    }

    #[test]
    fn tick_overlays_drops_expired_armed_kill() {
        let mut app = App::new();
        app.arm_kill(ArmedKill {
            pid: 4242,
            name: "ollama".into(),
            allowlisted: false,
            // Arm 6s ago; the 5s window has already lapsed.
            armed_at: std::time::Instant::now() - std::time::Duration::from_secs(6),
        });
        app.tick_overlays();
        assert!(app.armed_kill().is_none());
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
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                tokens_per_sec: None,
                exit_reason: ExitReason::CleanExit,
                stderr_tail: Vec::new(),
                baseline_status: BaselineStatus::NotAvailable,
            },
            shown_at: std::time::Instant::now(),
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
        let count = app.acknowledge_alerts();
        assert_eq!(count, 0);
        assert_eq!(app.status(), None);
    }

    #[test]
    fn acknowledge_alerts_returns_count_and_sets_status_when_active() {
        use crate::ui::alerts::WorkloadRef;
        let mut app = App::new();
        let now = std::time::Instant::now();
        // Two instant-fire alerts → two Active slots.
        app.alerts_mut().observe(
            now,
            WorkloadRef::workload(206, "phi3"),
            ux_contract::AlertId::GovernorArmed,
            true,
        );
        app.alerts_mut().observe(
            now,
            WorkloadRef::workload(207, "vllm"),
            ux_contract::AlertId::OomDetected,
            true,
        );
        assert_eq!(app.alerts().visible().len(), 2);

        let count = app.acknowledge_alerts();
        assert_eq!(count, 2);
        // Both moved to Suppressed → out of visible.
        assert_eq!(app.alerts().visible().len(), 0);
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
        let now = std::time::Instant::now();
        app.alerts_mut().observe(
            now,
            WorkloadRef::workload(206, "phi3"),
            ux_contract::AlertId::GovernorArmed,
            true,
        );
        app.acknowledge_alerts();
        let status = app.status().unwrap_or("");
        let expected = ux_contract::status::ALERTS_ACKNOWLEDGED.replace("{n}", "1");
        assert_eq!(status, expected);
    }
}
