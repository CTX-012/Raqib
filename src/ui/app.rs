use crate::model::AICategory;
use crate::runtime::RuntimeState;
use crate::storage::RunRecord;
use crate::ui::panels::armed_banner::ArmedKill;
use crate::ui::panels::postmortem::PostMortemCard;

/// Which panel currently owns selection / cursor focus.
/// Only the three list panels accept selection; vitals and audit are read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPanel {
    Registry,
    Rogues,
    Culprits,
}

impl FocusedPanel {
    pub fn next(self) -> Self {
        match self {
            Self::Registry => Self::Rogues,
            Self::Rogues => Self::Culprits,
            Self::Culprits => Self::Registry,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::Registry => Self::Culprits,
            Self::Rogues => Self::Registry,
            Self::Culprits => Self::Rogues,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Registry => "Registry",
            Self::Rogues => "Rogues",
            Self::Culprits => "Culprits",
        }
    }
}

/// One discrete intent produced by an input event. Kept narrow so the
/// outer loop is a flat match — no nested control flow inside `input::`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    ToggleDryRun,
    FocusNext,
    FocusPrev,
    SelectNext,
    SelectPrev,
    ToggleHelp,
    StartFilter,
    CommitFilter,
    CancelFilter,
    FilterChar(char),
    FilterBackspace,
    /// First press arms; second press confirms. Two-step for safety.
    ConfirmKill,
    /// Open the history overlay for the focused row's model. Tier 1.1.
    OpenHistory,
    /// Close the history overlay (Esc).
    CloseHistory,
    /// Toggle the secondary panels (Framework procs / All processes /
    /// Recent actions) on or off. They stay hidden by default so the
    /// main view is just AI Workloads + Recent runs.
    ToggleDetailMode,
    /// `g` keybinding ([UX-3]). Open `[dashboard].url_template` in the
    /// default browser with `{model}` and `{pid}` substituted against
    /// the focused row.
    OpenDashboard,
    /// Dismiss the post-mortem card ([UX-2]). Triggered by `Enter`
    /// when a card is visible; `Esc` also dismisses via the cascading
    /// priority handled in `apply_action`.
    DismissPostmortem,
    /// Show the post-mortem card for the currently focused row
    /// ([UX-2], UI Contract v2). Triggered by `Enter` in Normal mode
    /// when no card is already visible. Skipped silently when no row
    /// is focused or the focused workload has no run history yet.
    ShowPostmortemForFocused,
    /// Cascading-priority Esc: dismisses post-mortem first, then
    /// disarms a pending kill, then closes any other overlay. Filter
    /// mode handles its own Esc before this runs.
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
}

/// Cached per-model run history shown by the overlay. Loaded on `h`
/// keypress and cleared on Esc; not refreshed every frame to avoid
/// hammering the `RunStore` from the render path.
#[derive(Debug, Clone)]
pub struct HistoryOverlay {
    pub model: String,
    pub records: Vec<RunRecord>,
}

/// Pure state machine for the TUI. No I/O, no rendering. Cheap to clone.
#[derive(Debug, Clone)]
pub struct App {
    focus: FocusedPanel,
    selected: usize,
    show_help: bool,
    quit_requested: bool,
    mode: Mode,
    filter: String,
    /// Two-stage manual-kill ([UX-1]). `Some(_)` after the first `k`
    /// press; auto-disarms after `ArmedKill::WINDOW`. Carries pid +
    /// name + allowlisted so the banner can render without re-reading
    /// the runtime state on every frame.
    armed_kill: Option<ArmedKill>,
    /// Most recent post-mortem-eligible exit ([UX-2]). Latest wins;
    /// dismissed by Esc, Enter, or auto at `PostMortemCard::WINDOW`.
    postmortem: Option<PostMortemCard>,
    /// `Some(_)` while the history overlay is open. Snapshotted on key
    /// press so subsequent ticks don't replace the records the user is
    /// reading.
    history: Option<HistoryOverlay>,
    /// Detail mode — when `true`, render the secondary panels
    /// (Framework procs, All processes, Recent actions). Default `false`
    /// keeps the main view focused on AI Workloads + Recent runs, which
    /// is what the operator-feedback pass asked for. Tab focus-cycling
    /// is suppressed in default mode since only one panel is reachable.
    detail_mode: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            focus: FocusedPanel::Registry,
            selected: 0,
            show_help: false,
            quit_requested: false,
            mode: Mode::Normal,
            filter: String::new(),
            armed_kill: None,
            postmortem: None,
            history: None,
            detail_mode: false,
        }
    }

    pub fn focus(&self) -> FocusedPanel {
        self.focus
    }
    pub fn selected_index(&self) -> usize {
        self.selected
    }
    pub fn show_help(&self) -> bool {
        self.show_help
    }
    pub fn detail_mode(&self) -> bool {
        self.detail_mode
    }
    pub fn should_quit(&self) -> bool {
        self.quit_requested
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn filter(&self) -> &str {
        &self.filter
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

    pub fn focus_next(&mut self) {
        // Tab is meaningless when only AI Workloads is on screen — silently
        // no-op rather than secretly moving focus to a panel the operator
        // can't see.
        if !self.detail_mode {
            return;
        }
        self.focus = self.focus.next();
        self.selected = 0;
        self.armed_kill = None;
    }

    pub fn focus_prev(&mut self) {
        if !self.detail_mode {
            return;
        }
        self.focus = self.focus.prev();
        self.selected = 0;
        self.armed_kill = None;
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Toggle the secondary-panel view. Leaving detail mode snaps focus
    /// back to AI Workloads (FocusedPanel::Registry) so the operator
    /// returns to a known starting point — the previously focused panel
    /// is no longer on screen, so keeping focus there would be invisible
    /// state.
    pub fn toggle_detail_mode(&mut self) {
        self.detail_mode = !self.detail_mode;
        if !self.detail_mode {
            self.focus = FocusedPanel::Registry;
            self.selected = 0;
            self.armed_kill = None;
        }
    }

    pub fn start_filter(&mut self) {
        self.mode = Mode::Filter;
    }

    pub fn cancel_filter(&mut self) {
        self.mode = Mode::Normal;
        self.filter.clear();
    }

    pub fn commit_filter(&mut self) {
        self.mode = Mode::Normal;
    }

    pub fn filter_push(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
    }

    pub fn filter_pop(&mut self) {
        self.filter.pop();
        self.selected = 0;
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

    /// PIDs visible in the currently focused panel after applying the filter.
    /// Stable PID-sorted so user selection doesn't jump between ticks.
    pub fn visible(&self, state: &RuntimeState) -> Vec<u32> {
        let needle = self.filter.to_lowercase();
        let matches = |p: &crate::runtime::AnnotatedProcess| {
            needle.is_empty() || p.name.to_lowercase().contains(&needle)
        };
        let mut pids: Vec<u32> = match self.focus {
            FocusedPanel::Registry => state
                .ai_processes()
                .filter(|p| matches(p))
                .map(|p| p.pid)
                .collect(),
            FocusedPanel::Rogues => state
                .annotated
                .iter()
                .filter(|p| p.category == AICategory::Framework && matches(p))
                .map(|p| p.pid)
                .collect(),
            FocusedPanel::Culprits => {
                let mut by_mem: Vec<&crate::runtime::AnnotatedProcess> =
                    state.annotated.iter().filter(|p| matches(p)).collect();
                by_mem.sort_by_key(|p| p.pid);
                by_mem.iter().take(20).map(|p| p.pid).collect()
            }
        };
        pids.sort();
        pids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn focus_cycles_forward_and_backward() {
        let mut app = App::new();
        // Tab is meaningful only in detail mode; flip the mode on first.
        app.toggle_detail_mode();
        assert_eq!(app.focus(), FocusedPanel::Registry);
        app.focus_next();
        assert_eq!(app.focus(), FocusedPanel::Rogues);
        app.focus_prev();
        assert_eq!(app.focus(), FocusedPanel::Registry);
    }

    #[test]
    fn default_mode_locks_focus_to_registry() {
        let mut app = App::new();
        assert!(!app.detail_mode());
        // Tab / Shift-Tab are no-ops while the secondary panels are
        // hidden — moving focus to a panel the operator can't see would
        // be invisible state.
        app.focus_next();
        assert_eq!(app.focus(), FocusedPanel::Registry);
        app.focus_prev();
        assert_eq!(app.focus(), FocusedPanel::Registry);
    }

    #[test]
    fn toggle_detail_mode_flips_the_flag_and_resets_focus() {
        let mut app = App::new();
        assert!(!app.detail_mode());
        app.toggle_detail_mode();
        assert!(app.detail_mode());
        app.focus_next(); // now actually moves
        assert_eq!(app.focus(), FocusedPanel::Rogues);
        // Leaving detail mode snaps focus back to Registry; otherwise
        // the status bar would still say "focus: Rogues" while Rogues
        // is no longer drawn.
        app.toggle_detail_mode();
        assert!(!app.detail_mode());
        assert_eq!(app.focus(), FocusedPanel::Registry);
        assert_eq!(app.selected_index(), 0);
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
    fn leaving_detail_mode_disarms_pending_kill() {
        let mut app = App::new();
        app.toggle_detail_mode();
        app.arm_kill(fake_armed(99, "ollama", false));
        assert_eq!(app.armed_kill_pid(), Some(99));
        // Returning to default mode hides the panel where the operator
        // armed the kill — clearing the arm is the safe default; an
        // armed kill the user can no longer see is a footgun.
        app.toggle_detail_mode();
        assert_eq!(app.armed_kill_pid(), None);
    }

    #[test]
    fn registry_visible_only_includes_ai() {
        let s = state_with(vec![
            ann(1, "ollama", AICategory::Inference),
            ann(2, "bash", AICategory::NotAi),
        ]);
        let app = App::new();
        let pids = app.visible(&s);
        assert_eq!(pids, vec![1]);
    }

    #[test]
    fn filter_substring_narrows_visible() {
        let s = state_with(vec![
            ann(1, "ollama", AICategory::Inference),
            ann(2, "vllm", AICategory::Inference),
        ]);
        let mut app = App::new();
        app.start_filter();
        for c in "vllm".chars() {
            app.filter_push(c);
        }
        let pids = app.visible(&s);
        assert_eq!(pids, vec![2]);
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

    #[test]
    fn focus_change_disarms_kill_for_safety() {
        let mut app = App::new();
        // Tab is a no-op in default mode, so move into detail mode
        // first; that's the only mode where focus actually changes.
        app.toggle_detail_mode();
        app.arm_kill(fake_armed(42, "ollama", false));
        app.focus_next();
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
    fn cancel_filter_clears_text_and_returns_to_normal() {
        let mut app = App::new();
        app.start_filter();
        app.filter_push('x');
        app.cancel_filter();
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.filter().is_empty());
    }

    #[test]
    fn quit_request_propagates() {
        let mut app = App::new();
        assert!(!app.should_quit());
        app.request_quit();
        assert!(app.should_quit());
    }
}
