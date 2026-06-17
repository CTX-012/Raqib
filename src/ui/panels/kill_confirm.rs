//! CAR-17 / UX_CONTRACT.md kill_confirm card.
//!
//! Modal overlay shown after the user presses `k` to confirm a kill on
//! the focused workload. Replaces the v0.3.x ARMED banner pattern:
//! kill is always real, and this card IS the safety surface.
//!
//! Pairs with [`super::live_detail`] and [`super::postmortem`]: all
//! three are 64-column centered overlays with identical dimensions
//! (`postmortem::CARD_WIDTH` / `CARD_MIN_HEIGHT` / `CARD_MAX_HEIGHT`)
//! so the operator sees a consistent overlay shape regardless of
//! which card kind opens.
//!
//! Lifecycle differs: live_detail / post_mortem auto-dismiss after
//! 30 s. The kill_confirm card stays open until the operator explicitly
//! confirms (Enter → kill fires) or cancels (Esc → dismiss without
//! kill). No auto-dismiss timer; an unattended kill prompt that
//! self-dismisses would silently swallow the kill_confirm safety.
//!
//! All user-visible strings come from `ux_contract::kill_confirm_card`
//! (v0.3.8 / CAR-17).

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use ux_contract::kill_confirm_card as kcc;

use crate::ui::panels::postmortem::{CARD_MAX_HEIGHT, CARD_MIN_HEIGHT, CARD_WIDTH, format_megabytes};
use crate::ui::theme::UiTheme;

/// DISPATCH 83 / C2 — kill_confirm card sub-state.
///
/// The card flows: `Confirm` (operator pressed `k` — about to send
/// SIGTERM) → `Waiting` (SIGTERM sent, holding open for the grace
/// window; operator can force-SIGKILL via a second Enter, or Esc
/// to dismiss the card while leaving the SIGTERM in effect). The
/// pre-D83 single-state card maps to `Confirm`; the `Waiting`
/// state is the new escalation surface.
#[derive(Debug, Clone)]
pub enum KillConfirmStage {
    /// The initial state: the operator has pressed `k` and the card
    /// is asking for SIGTERM confirmation. Enter sends SIGTERM and
    /// transitions to `Waiting`; Esc dismisses.
    Confirm,
    /// Post-SIGTERM holding state. SIGTERM is already in flight;
    /// the card stays open so the operator can either watch the PID
    /// exit (auto-dismiss on lifecycle exit) or escalate via a
    /// second Enter (force-SIGKILL through the D81 identity-guard
    /// path). Esc dismisses the card without escalation; the SIGTERM
    /// remains in effect (`pending_kills` entry survives until the
    /// PID actually exits).
    Waiting {
        /// Wall-clock instant at which the SIGTERM was sent. Drives
        /// the `{secs}` countdown substitution in
        /// `KILL_CONFIRM_WAITING_PROMPT` rendering and gates whether
        /// the lifecycle "this PID hasn't exited yet" check is
        /// meaningful (a Waiting card pre-`sigterm_grace_secs` is
        /// still in the cooperative-shutdown window).
        sigterm_at: Instant,
        /// `policy.sigterm_grace_secs` snapshotted at the moment of
        /// transition so a config change mid-prompt can't shift the
        /// rendered countdown.
        grace_secs: u64,
    },
}

/// Snapshot of the workload the kill_confirm card targets. Owned by
/// `App` and replaced wholesale when the operator presses `k` (a fresh
/// snapshot is built from the current `RuntimeState`). Cheap to clone.
#[derive(Debug, Clone)]
pub struct KillConfirmCard {
    /// Display name — `model_name` when the classifier resolved one,
    /// otherwise the raw process name. Same resolution rule as the
    /// live_detail / post_mortem cards.
    pub display_name: String,
    /// Target PID. Pinned at card-open time so a focus drift between
    /// open and confirm can't redirect the kill to a different process.
    pub pid: u32,
    /// AI category for the §3 category label (LLM / Vision / etc.).
    pub category: String,
    /// Current workload status string (Healthy / Degraded / Critical
    /// / Loading / Exited). Snapshot at open time.
    pub status: String,
    /// Wall-clock seconds the workload has been running, computed from
    /// the §3 `first_observed_at` field. Frozen at card-open time so
    /// the displayed value reflects "how long ago" the operator chose
    /// to confirm, not a live countdown.
    pub runtime_secs: u64,
    pub cpu_pct: f32,
    pub rss_mb: u64,
    /// `None` when NVML didn't report VRAM for this PID (CPU-only
    /// workloads). Conditionally omits the row entirely per the
    /// post_mortem-card precedent.
    pub vram_mb: Option<u64>,
    /// Whether the workload sits on the governor allowlist. Drives an
    /// inline "(ALLOWLISTED)" note next to the workload row so the
    /// operator can't override the allowlist without seeing it. Empty
    /// for non-allowlisted workloads.
    pub allowlisted: bool,
    pub shown_at: Instant,
    /// DISPATCH 83 / C2 — escalation sub-state. Defaults to
    /// `Confirm` for fresh cards built via `KillConfirmCard::new`;
    /// transitions to `Waiting` via `into_waiting` after the
    /// operator's first Enter confirms the SIGTERM.
    pub stage: KillConfirmStage,
}

impl KillConfirmCard {
    // The card snapshots nine independent workload fields from the
    // live `RuntimeState` at open time. Wrapping them in an
    // intermediate builder/struct adds boilerplate without buying
    // readability — every field is required and named at the call
    // site already.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        display_name: String,
        pid: u32,
        category: String,
        status: String,
        runtime_secs: u64,
        cpu_pct: f32,
        rss_mb: u64,
        vram_mb: Option<u64>,
        allowlisted: bool,
    ) -> Self {
        Self {
            display_name,
            pid,
            category,
            status,
            runtime_secs,
            cpu_pct,
            rss_mb,
            vram_mb,
            allowlisted,
            shown_at: Instant::now(),
            stage: KillConfirmStage::Confirm,
        }
    }

    /// How long the card has been open. Pure read; the card has no
    /// auto-dismiss window — the operator's explicit Enter / Esc is
    /// the only exit path. Post-D83: a `Waiting`-state card can
    /// also auto-dismiss when the lifecycle reports the targeted
    /// PID has exited (the SIGTERM succeeded; no escalation needed).
    pub fn elapsed(&self) -> Duration {
        self.shown_at.elapsed()
    }

    /// DISPATCH 83 / C2 — transition a Confirm-state card into the
    /// post-SIGTERM Waiting state. Snapshots `grace_secs` from the
    /// caller (`policy.sigterm_grace_secs`) so the rendered
    /// countdown can't shift if the config mutates while the prompt
    /// is open. Idempotent on a Waiting card (the existing stage is
    /// preserved — re-transitioning would reset the countdown which
    /// would mis-represent the actual SIGTERM timestamp).
    pub fn into_waiting(self, grace_secs: u64) -> Self {
        let stage = match self.stage {
            KillConfirmStage::Confirm => KillConfirmStage::Waiting {
                sigterm_at: Instant::now(),
                grace_secs,
            },
            KillConfirmStage::Waiting { .. } => self.stage,
        };
        Self { stage, ..self }
    }

    /// `true` when the card is in the post-SIGTERM Waiting state.
    /// Used by the apply_action dispatcher to route the operator's
    /// Enter press: Confirm → SIGTERM; Waiting → force-SIGKILL.
    pub fn is_waiting(&self) -> bool {
        matches!(self.stage, KillConfirmStage::Waiting { .. })
    }

    /// Whole-second count of how much grace remains before the
    /// rendered prompt reaches `0s`. Returns 0 in the Confirm state
    /// (no SIGTERM has been sent) and `saturating_sub` so a slow
    /// render past grace just reads `0s` rather than wrapping.
    pub fn grace_secs_remaining(&self) -> u64 {
        match &self.stage {
            KillConfirmStage::Waiting {
                sigterm_at,
                grace_secs,
            } => grace_secs.saturating_sub(sigterm_at.elapsed().as_secs()),
            KillConfirmStage::Confirm => 0,
        }
    }
}

/// Label column width — longest label `Running for:` is 12 chars,
/// padded to 18 to match the live_detail / post_mortem layouts so
/// the three cards line up visually when an operator flips between
/// them.
const LABEL_WIDTH: usize = 18;

/// Render the centered kill_confirm card. Called last in the panels
/// render path (alongside the other two card kinds) so it floats above
/// every other panel.
pub fn render(frame: &mut Frame, full: Rect, card: &KillConfirmCard, theme: &UiTheme) {
    let lines = build_lines(card, theme);
    let height = lines
        .len()
        .saturating_add(2) // border top + bottom
        .clamp(CARD_MIN_HEIGHT as usize, CARD_MAX_HEIGHT as usize) as u16;
    let area = centered_rect(full, CARD_WIDTH, height);
    frame.render_widget(Clear, area);

    // Title bar — accented + critical-tinted so the operator
    // immediately reads "this is the destructive prompt." Border picks
    // up the same critical color so the whole card frame signals
    // danger without leaning on the §0 status-dot palette.
    let title = format!(" {} ", kcc::KILL_CONFIRM_TITLE);
    let border_style = Style::default()
        .fg(theme.critical)
        .add_modifier(Modifier::BOLD);
    let block = Block::default()
        .title(Span::styled(title, border_style))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let padded = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), padded);
}

/// Pure build of the card's line set. Public so the unit tests and the
/// `tests/copy_strings_via_contract.rs` guard can pin label ordering
/// and string sourcing without spinning a `Frame`.
pub fn build_lines(card: &KillConfirmCard, theme: &UiTheme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. Workload — name + optional ALLOWLISTED tag.
    let workload_value = if card.allowlisted {
        format!("{} (ALLOWLISTED)", card.display_name)
    } else {
        card.display_name.clone()
    };
    lines.push(labeled(kcc::KILL_CONFIRM_WORKLOAD_LABEL, &workload_value));
    // 2. PID:
    lines.push(labeled(
        kcc::KILL_CONFIRM_PID_LABEL,
        &card.pid.to_string(),
    ));
    // 3. Category:
    lines.push(labeled(kcc::KILL_CONFIRM_CATEGORY_LABEL, &card.category));
    // 4. Status:
    lines.push(labeled(kcc::KILL_CONFIRM_STATUS_LABEL, &card.status));
    // 5. Running for:
    lines.push(labeled(
        kcc::KILL_CONFIRM_RUNTIME_LABEL,
        &format_runtime(card.runtime_secs),
    ));
    // 6. CPU / RAM / VRAM. VRAM omitted when None — same conditional-
    //    omit rule the live_detail / post_mortem cards use.
    lines.push(labeled(
        kcc::KILL_CONFIRM_CPU_LABEL,
        &format!("{:.1}%", card.cpu_pct),
    ));
    lines.push(labeled(
        kcc::KILL_CONFIRM_RAM_LABEL,
        &format_megabytes(card.rss_mb),
    ));
    if let Some(vram) = card.vram_mb {
        lines.push(labeled(
            kcc::KILL_CONFIRM_VRAM_LABEL,
            &format_megabytes(vram),
        ));
    }

    // 7. blank
    lines.push(Line::from(""));

    // 8. Prompt — bold attention color so the question reads above
    //    the field list. DISPATCH 83 / C2 — the Waiting state
    //    substitutes the SIGTERM-in-flight prompt + countdown
    //    (`KILL_CONFIRM_WAITING_PROMPT` with `{secs}` filled). The
    //    Confirm state keeps the pre-D83 SIGTERM confirmation
    //    prompt unchanged.
    let prompt = match &card.stage {
        KillConfirmStage::Confirm => kcc::KILL_CONFIRM_PROMPT.to_string(),
        KillConfirmStage::Waiting { .. } => kcc::KILL_CONFIRM_WAITING_PROMPT
            .replace("{secs}", &card.grace_secs_remaining().to_string()),
    };
    lines.push(Line::from(Span::styled(
        prompt,
        Style::default()
            .fg(theme.attention)
            .add_modifier(Modifier::BOLD),
    )));

    // 9. Footer hint — muted so it reads as an exit-route legend.
    //    Waiting state surfaces the [Enter] force-kill / [Esc] cancel
    //    legend; Confirm state keeps the pre-D83 legend.
    let hint = match &card.stage {
        KillConfirmStage::Confirm => kcc::KILL_CONFIRM_HINT,
        KillConfirmStage::Waiting { .. } => kcc::KILL_CONFIRM_WAITING_HINT,
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.muted),
    )));

    lines
}

fn labeled(label: &str, value: &str) -> Line<'static> {
    let padded_label = format!("{label:<width$}", width = LABEL_WIDTH);
    Line::from(vec![
        Span::styled(padded_label, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(value.to_string()),
    ])
}

fn format_runtime(secs: u64) -> String {
    // Mirrors `post_mortem::format_duration`'s shape — `Hh Mm Ss` with
    // the leading components omitted for short runs. Inlined rather
    // than re-exported because the post_mortem helper is `pub(super)`
    // and pulling it out into the panels namespace would surface an
    // implementation detail that's been stable since L16.
    let h = secs / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn centered_rect(r: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(r.width);
    let h = height.min(r.height);
    let x = r.x + (r.width.saturating_sub(w)) / 2;
    let y = r.y + (r.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> KillConfirmCard {
        KillConfirmCard::new(
            "phi3-mini".into(),
            4242,
            "LLM".into(),
            "Running".into(),
            65,
            47.3,
            2_048,
            Some(4_096),
            false,
        )
    }

    fn render_string(card: &KillConfirmCard) -> String {
        let theme = crate::ui::theme::current_theme("dark");
        build_lines(card, &theme)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dimensions_match_other_overlay_cards() {
        // Three cards, one overlay shape — flip-flopping operator must
        // see the same dimensions regardless of which card opens.
        assert_eq!(CARD_WIDTH, crate::ui::panels::postmortem::CARD_WIDTH);
        assert_eq!(
            CARD_MIN_HEIGHT,
            crate::ui::panels::postmortem::CARD_MIN_HEIGHT
        );
        assert_eq!(
            CARD_MAX_HEIGHT,
            crate::ui::panels::postmortem::CARD_MAX_HEIGHT
        );
    }

    #[test]
    fn render_uses_contract_title_string() {
        // CAR-17 contract lock: title must come from
        // `ux_contract::kill_confirm_card::KILL_CONFIRM_TITLE`, not a
        // local literal. The render function embeds it in the block
        // title; build_lines doesn't emit it (the block does), so this
        // test pins the constant directly.
        assert_eq!(kcc::KILL_CONFIRM_TITLE, "Kill Confirmation");
    }

    #[test]
    fn body_lists_workload_pid_and_category() {
        let card = fixture();
        let rendered = render_string(&card);
        assert!(
            rendered.contains("Workload:") && rendered.contains("phi3-mini"),
            "expected Workload row with display name:\n{rendered}"
        );
        assert!(
            rendered.contains("PID:") && rendered.contains("4242"),
            "expected PID row with pid:\n{rendered}"
        );
        assert!(
            rendered.contains("Category:") && rendered.contains("LLM"),
            "expected Category row:\n{rendered}"
        );
    }

    #[test]
    fn body_uses_kcc_constants_for_every_label() {
        // CAR-17 contract surface: every label in the body must be
        // sourced from `ux_contract::kill_confirm_card::*`. The build
        // path uses the constants directly; this test pins them so a
        // future refactor that inlined literals would break here.
        let card = fixture();
        let rendered = render_string(&card);
        for label in [
            kcc::KILL_CONFIRM_WORKLOAD_LABEL,
            kcc::KILL_CONFIRM_PID_LABEL,
            kcc::KILL_CONFIRM_CATEGORY_LABEL,
            kcc::KILL_CONFIRM_STATUS_LABEL,
            kcc::KILL_CONFIRM_RUNTIME_LABEL,
            kcc::KILL_CONFIRM_CPU_LABEL,
            kcc::KILL_CONFIRM_RAM_LABEL,
            kcc::KILL_CONFIRM_VRAM_LABEL,
            kcc::KILL_CONFIRM_PROMPT,
            kcc::KILL_CONFIRM_HINT,
        ] {
            assert!(
                rendered.contains(label),
                "card body must include `{label}` (sourced from \
                 ux_contract::kill_confirm_card):\n{rendered}",
            );
        }
    }

    #[test]
    fn vram_row_omitted_when_unavailable() {
        let mut card = fixture();
        card.vram_mb = None;
        let rendered = render_string(&card);
        assert!(
            !rendered.contains("VRAM:"),
            "vram=None must omit the row entirely:\n{rendered}"
        );
    }

    #[test]
    fn allowlisted_workload_marked_inline() {
        let mut card = fixture();
        card.allowlisted = true;
        let rendered = render_string(&card);
        assert!(
            rendered.contains("ALLOWLISTED"),
            "allowlisted card must surface override warning inline:\n{rendered}"
        );
    }

    #[test]
    fn no_dry_run_strings_anywhere_in_card() {
        // CAR-17 design lock — kill is always real, the card IS the
        // safety. Any residual dry-run copy here would re-introduce
        // the operator-confusion the v0.3.8 contract removed.
        let card = fixture();
        let rendered = render_string(&card);
        for needle in ["DRY-RUN", "dry-run", "dry_run", "Would stop"] {
            assert!(
                !rendered.contains(needle),
                "kill_confirm card must not reference dry-run \
                 (found {needle:?}):\n{rendered}",
            );
        }
    }

    #[test]
    fn format_runtime_renders_h_m_s_components() {
        assert_eq!(format_runtime(5), "5s");
        assert_eq!(format_runtime(65), "1m 5s");
        assert_eq!(format_runtime(3_725), "1h 2m 5s");
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 83 / C2 — Waiting-state rendering and stage helpers.
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn new_card_starts_in_confirm_stage() {
        let card = fixture();
        assert!(
            matches!(card.stage, KillConfirmStage::Confirm),
            "fresh KillConfirmCard::new() MUST default to Confirm stage; \
             the Waiting transition only happens after manual_kill succeeds"
        );
        assert!(!card.is_waiting());
        assert_eq!(card.grace_secs_remaining(), 0);
    }

    #[test]
    fn into_waiting_transitions_only_from_confirm() {
        let card = fixture();
        let waiting = card.into_waiting(7);
        assert!(waiting.is_waiting());
        assert!(matches!(
            waiting.stage,
            KillConfirmStage::Waiting { grace_secs: 7, .. }
        ));
        // Idempotent: re-transitioning a Waiting card MUST preserve
        // the original sigterm_at so the rendered countdown reflects
        // the actual SIGTERM time, not a reset.
        let pre = match &waiting.stage {
            KillConfirmStage::Waiting { sigterm_at, .. } => *sigterm_at,
            _ => unreachable!(),
        };
        let again = waiting.into_waiting(99);
        let post = match &again.stage {
            KillConfirmStage::Waiting {
                sigterm_at,
                grace_secs,
            } => (*sigterm_at, *grace_secs),
            _ => unreachable!(),
        };
        assert_eq!(
            post.0, pre,
            "re-transitioning Waiting must not reset sigterm_at"
        );
        assert_eq!(
            post.1, 7,
            "re-transitioning Waiting must preserve original grace_secs (not 99)"
        );
    }

    #[test]
    fn waiting_card_renders_v0_3_19_waiting_strings() {
        // DISPATCH 83 / C2 — pin that the Waiting state renders the
        // v0.3.19 contract strings (`KILL_CONFIRM_WAITING_PROMPT`
        // with `{secs}` substituted + `KILL_CONFIRM_WAITING_HINT`),
        // NOT the Confirm-state strings.
        let card = fixture().into_waiting(5);
        let rendered = render_string(&card);
        // Waiting prompt with the literal "{secs}" replaced by a
        // small number (the test runs near-instantly so the
        // countdown reads 5).
        assert!(
            rendered.contains("SIGTERM sent") && rendered.contains("graceful shutdown"),
            "Waiting card must render KILL_CONFIRM_WAITING_PROMPT:\n{rendered}",
        );
        assert!(
            !rendered.contains("{secs}"),
            "Waiting prompt MUST have {{secs}} substituted (not the \
             literal placeholder):\n{rendered}",
        );
        assert!(
            rendered.contains(kcc::KILL_CONFIRM_WAITING_HINT),
            "Waiting card must render KILL_CONFIRM_WAITING_HINT:\n{rendered}",
        );
        // Pre-D83 Confirm-state strings MUST NOT appear (otherwise
        // both prompts/hints render on top of each other).
        assert!(
            !rendered.contains(kcc::KILL_CONFIRM_PROMPT),
            "Confirm prompt MUST NOT render in Waiting state:\n{rendered}",
        );
        assert!(
            !rendered.contains(kcc::KILL_CONFIRM_HINT),
            "Confirm hint MUST NOT render in Waiting state:\n{rendered}",
        );
    }

    #[test]
    fn grace_secs_remaining_saturates_at_zero() {
        // Build a Waiting card with an in-the-past sigterm_at so
        // the elapsed time exceeds grace_secs immediately. The
        // saturating_sub MUST return 0, not wrap.
        let mut card = fixture();
        card.stage = KillConfirmStage::Waiting {
            sigterm_at: Instant::now() - std::time::Duration::from_secs(999),
            grace_secs: 5,
        };
        assert_eq!(
            card.grace_secs_remaining(),
            0,
            "elapsed > grace_secs MUST saturate to 0, not wrap"
        );
    }
}
