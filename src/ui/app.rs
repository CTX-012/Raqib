use crate::model::AICategory;
use crate::runtime::RuntimeState;
use crate::storage::RunRecord;

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
    /// Two-stage manual-kill: when `Some(pid)`, pressing `k` again sends.
    armed_kill: Option<u32>,
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
        self.armed_kill
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

    pub fn arm_kill(&mut self, pid: u32) {
        self.armed_kill = Some(pid);
    }
    pub fn disarm_kill(&mut self) {
        self.armed_kill = None;
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

    #[test]
    fn leaving_detail_mode_disarms_pending_kill() {
        let mut app = App::new();
        app.toggle_detail_mode();
        app.arm_kill(99);
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
        app.arm_kill(42);
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
        app.arm_kill(42);
        app.focus_next();
        assert_eq!(app.armed_kill_pid(), None);
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
