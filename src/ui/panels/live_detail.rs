//! L16 / UX_CONTRACT.md §5 — live-detail card (`Enter` on a running
//! workload).
//!
//! Pairs with [`super::postmortem`]: both are 64-column centered
//! overlays with identical dimensions and lifetime mechanics (30-second
//! auto-dismiss, latest-wins, Esc/Enter cuts the window short). They
//! differ only in content — this card shows **live** metrics for a
//! workload that is still running; the post-mortem card shows the
//! retrospective summary of a workload that has exited.
//!
//! L17 will populate sparkline rows here from rolling per-PID
//! ring-buffers. L16 only installs the card structure + render
//! plumbing so the dispatch site in `ui::mod.rs` can route
//! `Enter`-on-running to live_detail without conflating with
//! `Enter`-on-exited.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::runtime::{AnnotatedProcess, RuntimeState};
use crate::ui::panels::postmortem::{
    CARD_MAX_HEIGHT, CARD_MIN_HEIGHT, CARD_WIDTH, format_megabytes,
};
use crate::ui::theme::UiTheme;

/// Transient live-snapshot payload. Built at `Enter`-keypress time
/// from `RuntimeState` for the focused PID; the card's `shown_at`
/// pins the 30-second window. Cheap to clone.
///
/// L17 will add per-metric ring buffers (`Vec<f32>` for CPU, RAM,
/// VRAM, tokens/sec) so the sparkline rows can render — but L16 does
/// not need them and including them now would require platform-side
/// rolling-buffer plumbing this row deliberately skips.
#[derive(Debug, Clone)]
pub struct LiveDetail {
    /// Card title — same resolution rule as `PostMortem::display_name`
    /// (`model_name` when the classifier extracted one, otherwise the
    /// raw process name).
    pub display_name: String,
    /// Focused workload PID. Surfaced as a line in the body so the
    /// operator can cross-reference with `ps`.
    pub pid: u32,
    /// Instantaneous CPU%, one core == 100%.
    pub cpu_pct: f32,
    /// Resident set size in megabytes.
    pub rss_mb: u64,
    /// Per-process VRAM in megabytes, `None` when NVML didn't report
    /// this PID or no GPU is present.
    pub vram_mb: Option<u64>,
    /// Live tokens/sec when the dispatcher has a reading; `None`
    /// otherwise (non-LLM workloads, or LLM that hasn't emitted a
    /// sample yet).
    pub tokens_per_sec: Option<f32>,
}

impl LiveDetail {
    /// Build a snapshot from the focused workload in `state`. Returns
    /// `None` when the PID isn't currently running (the caller should
    /// then fall through to the post-mortem path).
    pub fn from_focused(state: &RuntimeState, pid: u32) -> Option<Self> {
        let proc = state.annotated.iter().find(|p| p.pid == pid)?;
        // NotAi processes are not surfaced in the Workloads panel,
        // but the workloads panel is the only selectable list today;
        // if a future row makes other rows selectable we'd refuse
        // here. For v0.3 the filter is permissive — any focused row
        // with a live AnnotatedProcess gets a card.
        Some(Self::from_annotated(proc, state))
    }

    fn from_annotated(proc: &AnnotatedProcess, _state: &RuntimeState) -> Self {
        let display_name = proc
            .model_name
            .clone()
            .unwrap_or_else(|| proc.name.clone());
        let vram_mb = proc.vram_bytes.map(|b| b / (1024 * 1024)).filter(|&v| v > 0);
        Self {
            display_name,
            pid: proc.pid,
            cpu_pct: proc.cpu_pct,
            rss_mb: proc.rss_mb,
            vram_mb,
            // L17 hooks into the telemetry dispatcher for live
            // tokens/sec readings. v0.3 has no live token rate path
            // on `AnnotatedProcess`, so leave it None here; the card
            // omits the row when None per the same conditional-omit
            // rule the post-mortem card uses for `Throughput:`.
            tokens_per_sec: None,
        }
    }
}

/// Centered overlay wrapping a [`LiveDetail`]. Same dimensions and
/// lifetime semantics as [`super::postmortem::PostMortemCard`] — the
/// two cards are interchangeable from the layout / dismissal
/// machinery's perspective; only the body content differs.
#[derive(Debug, Clone)]
pub struct LiveDetailCard {
    pub live: LiveDetail,
    pub shown_at: Instant,
}

impl LiveDetailCard {
    /// 30-second auto-dismiss window. Matches `PostMortemCard::WINDOW`
    /// so the operator gets a consistent dismissal timer across both
    /// card kinds.
    pub const WINDOW: Duration = Duration::from_secs(30);

    pub fn new(live: LiveDetail) -> Self {
        Self {
            live,
            shown_at: Instant::now(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.shown_at.elapsed() >= Self::WINDOW
    }

    /// Seconds remaining, rounded UP (so a freshly-shown card reads
    /// `30s` rather than `29s` for the first 999 ms — mirrors the
    /// post-mortem behaviour).
    pub fn seconds_remaining(&self) -> u64 {
        let remaining = Self::WINDOW.saturating_sub(self.shown_at.elapsed());
        let secs = remaining.as_secs();
        if remaining.subsec_nanos() > 0 {
            secs + 1
        } else {
            secs
        }
    }
}

/// Label column width — longest label `Tokens/sec:` is 11 chars,
/// padded to 18 to match the post-mortem layout column so the two
/// cards align visually when an operator flips between them.
const LABEL_WIDTH: usize = 18;

/// Render the centered live-detail card. Called from
/// `panels::render` after every other panel so the card floats above
/// the rest of the frame — same z-order as the post-mortem card.
pub fn render(frame: &mut Frame, full: Rect, card: &LiveDetailCard, theme: &UiTheme) {
    let lines = build_lines_themed(card, theme);
    let height = lines
        .len()
        .saturating_add(2) // border top + bottom
        .clamp(CARD_MIN_HEIGHT as usize, CARD_MAX_HEIGHT as usize) as u16;
    let area = centered_rect(full, CARD_WIDTH, height);
    frame.render_widget(Clear, area);

    // Title format mirrors the post-mortem card's " {display_name} "
    // wrapping for visual parity. The accent fg picks up the active
    // theme — L21 will refine accent across panels; L20 already
    // routes the theme into this card via the parameter.
    let title = format!(" {} (live) ", card.live.display_name);
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
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

/// Pre-L21 build path: returns lines styled with ratatui's named
/// colors. Kept for the existing unit tests that pin label
/// ordering / conditional row omission without a theme handle.
/// `build_lines_themed` is what the live render path uses.
pub fn build_lines(card: &LiveDetailCard) -> Vec<Line<'static>> {
    build_lines_with(card, None)
}

/// L21 / §14 — themed build path. Subdued spans (sparkline
/// placeholder + dismiss-hint footer) render in `theme.muted` so a
/// `--theme light` or `--theme high-contrast` session reads with the
/// matching palette.
pub fn build_lines_themed(card: &LiveDetailCard, theme: &UiTheme) -> Vec<Line<'static>> {
    build_lines_with(card, Some(theme))
}

fn build_lines_with(card: &LiveDetailCard, theme: Option<&UiTheme>) -> Vec<Line<'static>> {
    let live = &card.live;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(labeled("PID:", &live.pid.to_string()));
    lines.push(labeled("CPU:", &format!("{:.1}%", live.cpu_pct)));
    lines.push(labeled("RAM:", &format_megabytes(live.rss_mb)));
    if let Some(vram) = live.vram_mb {
        lines.push(labeled("GPU memory:", &format_megabytes(vram)));
    }
    if let Some(tps) = live.tokens_per_sec {
        lines.push(labeled("Tokens/sec:", &format!("{tps:.1} tokens/sec")));
    }

    lines.push(Line::from(""));

    let muted = match theme {
        Some(t) => t.muted,
        None => Color::DarkGray,
    };

    // L17 sparkline placeholder. Renders a faded-text hint so the
    // operator sees that the card is meant to host trends without
    // misreading a blank space as "no data" — the row count keeps
    // the height budget consistent with the post-mortem card and
    // gives L17 a clean target to swap into without re-sizing.
    lines.push(Line::from(Span::styled(
        "Sparklines (CPU / RAM / VRAM / tokens) — pending L17",
        Style::default().fg(muted).add_modifier(Modifier::ITALIC),
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s",
            n = card.seconds_remaining()
        ),
        Style::default().fg(muted),
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

    fn fixture(name: &str) -> LiveDetail {
        LiveDetail {
            display_name: name.to_string(),
            pid: 4242,
            cpu_pct: 47.3,
            rss_mb: 2048,
            vram_mb: Some(4096),
            tokens_per_sec: None,
        }
    }

    fn freshly_shown(live: LiveDetail) -> LiveDetailCard {
        LiveDetailCard::new(live)
    }

    #[test]
    fn dimensions_match_postmortem_card() {
        // §5 split locks the two cards to identical dimensions so the
        // operator gets a consistent overlay shape regardless of
        // which kind opens.
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
    fn auto_dismiss_window_matches_postmortem_window() {
        assert_eq!(
            LiveDetailCard::WINDOW,
            crate::ui::panels::postmortem::PostMortemCard::WINDOW
        );
    }

    #[test]
    fn seconds_remaining_counts_down_from_thirty() {
        let card = freshly_shown(fixture("phi3-mini"));
        assert_eq!(card.seconds_remaining(), 30);
        assert!(!card.is_expired());
    }

    #[test]
    fn expired_after_window() {
        let mut card = freshly_shown(fixture("phi3-mini"));
        card.shown_at = Instant::now() - Duration::from_secs(31);
        assert_eq!(card.seconds_remaining(), 0);
        assert!(card.is_expired());
    }

    #[test]
    fn build_lines_emits_required_fields_in_order() {
        let card = freshly_shown(fixture("phi3-mini"));
        let lines = build_lines(&card);
        let labels: Vec<String> = lines
            .iter()
            .filter_map(|l| l.spans.first().map(|s| s.content.to_string()))
            .map(|s| s.trim_end().to_string())
            .collect();

        let required_order = ["PID:", "CPU:", "RAM:", "GPU memory:"];
        let mut idx = 0;
        for label in labels.iter() {
            if idx < required_order.len() && label == required_order[idx] {
                idx += 1;
            }
        }
        assert_eq!(
            idx,
            required_order.len(),
            "live-detail field order violated; got labels: {labels:?}"
        );
    }

    #[test]
    fn gpu_memory_row_omitted_when_vram_unavailable() {
        let mut live = fixture("phi3-mini");
        live.vram_mb = None;
        let lines = build_lines(&freshly_shown(live));
        let rendered: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("GPU memory:"),
            "vram=None must omit the row entirely:\n{rendered}",
        );
    }

    #[test]
    fn tokens_per_sec_row_omitted_when_none() {
        let lines = build_lines(&freshly_shown(fixture("phi3-mini")));
        let rendered: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Fixture sets tokens_per_sec=None — the row must not render.
        assert!(
            !rendered.contains("Tokens/sec:"),
            "tokens_per_sec=None must omit the row:\n{rendered}"
        );
    }

    #[test]
    fn tokens_per_sec_row_renders_when_some() {
        let mut live = fixture("phi3-mini");
        live.tokens_per_sec = Some(38.4);
        let lines = build_lines(&freshly_shown(live));
        let rendered: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("Tokens/sec:") && rendered.contains("38.4"),
            "tokens_per_sec=Some must render the row + value:\n{rendered}"
        );
    }

    #[test]
    fn sparkline_placeholder_marks_l17_expansion_site() {
        // L17's job is to swap this placeholder for live ring-buffer
        // sparklines. Until then the card carries the deferred-row
        // marker so neither operators nor future maintainers
        // misread the blank-ish region.
        let lines = build_lines(&freshly_shown(fixture("phi3-mini")));
        let rendered: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("pending L17"),
            "sparkline placeholder missing — L17 will overwrite this row:\n{rendered}",
        );
    }
}
