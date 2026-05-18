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
        }
    }

    /// How long the card has been open. Pure read; the card has no
    /// auto-dismiss window — the operator's explicit Enter / Esc is
    /// the only exit path.
    pub fn elapsed(&self) -> Duration {
        self.shown_at.elapsed()
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

    // 8. Prompt — bold attention color so the question reads above the
    //    field list.
    lines.push(Line::from(Span::styled(
        kcc::KILL_CONFIRM_PROMPT,
        Style::default()
            .fg(theme.attention)
            .add_modifier(Modifier::BOLD),
    )));

    // 9. Footer hint — muted so it reads as an exit-route legend.
    lines.push(Line::from(Span::styled(
        kcc::KILL_CONFIRM_HINT,
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
}
