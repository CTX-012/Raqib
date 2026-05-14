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
//! L17 populates the per-metric rolling buffers and renders them as
//! sparkline rows below the instantaneous-value lines. Buffers live
//! in `run_loop` local scope (Path A from L16's BACKLOG entry) — they
//! ride alongside the `Option<LiveDetailCard>` the loop already
//! threads through `apply_action` and `panels::render`, deferring the
//! "lift modal-card state to App" architectural decision to a
//! dedicated refactor row.

use std::collections::VecDeque;
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

/// L17 / §5 — 60-second rolling window per metric. One sample per
/// `runtime.tick_interval_ms` tick (defaults to 1000 ms, so 60 entries
/// == 60 seconds). Anything wider would require resampling at render
/// time; anything narrower would lose the §5 "feel" of trend at a
/// glance.
pub const SPARKLINE_WINDOW: usize = 60;

/// L17 / §5 — sparkline width in cells, rendered against the card's
/// 64-column box. The full buffer is 60s; the card displays the most
/// recent `SPARKLINE_WIDTH` values to keep the row inside the inner
/// padding. Picked at 30 so the row reads as "the last 30s of trend"
/// — long enough to show a thermal ramp, short enough to leave room
/// for the trailing instantaneous value.
pub const SPARKLINE_WIDTH: u16 = 30;

/// 8-character block ramp used by the sparkline. Ordered low→high so
/// `BLOCKS[0]` is the shortest bar (`▁`) and `BLOCKS[7]` is the tallest
/// (`█`). Glyphs are part of the §15 box-drawing/sparkline subset; a
/// future row that lands an ASCII fallback for these would map them
/// through `SymbolSet` instead.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Bounded ring buffer for one metric over the §5 sparkline window.
/// Push at the head, drop from the tail when capacity is hit — the
/// renderer reads the most-recent slice via `values()`.
#[derive(Debug, Clone)]
pub struct MetricBuffer {
    values: VecDeque<f32>,
    capacity: usize,
}

impl MetricBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, v: f32) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    pub fn values(&self) -> &VecDeque<f32> {
        &self.values
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn last(&self) -> Option<f32> {
        self.values.back().copied()
    }
}

/// L17 / §5 — per-card aggregate of the four metric buffers shown in
/// the live-detail card's sparkline rows. Pinned to the workload PID
/// at construction so a focus shift to another workload resets the
/// buffers cleanly rather than mixing samples across processes.
///
/// CPU and tokens/sec are uncapped numerically; RAM and VRAM are
/// percentages on 0..=100. The threshold-aware rendering only
/// applies to RAM/VRAM/CPU%; tokens/sec is rendered in
/// `theme.foreground` regardless of value because its range is
/// metric-specific (no "85% of tokens/sec" — the value is a rate, not
/// a capacity).
#[derive(Debug, Clone)]
pub struct LiveDetailBuffers {
    pub pid: u32,
    pub cpu: MetricBuffer,
    pub ram_pct: MetricBuffer,
    pub vram_pct: MetricBuffer,
    pub tokens_per_sec: MetricBuffer,
}

impl LiveDetailBuffers {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            cpu: MetricBuffer::new(SPARKLINE_WINDOW),
            ram_pct: MetricBuffer::new(SPARKLINE_WINDOW),
            vram_pct: MetricBuffer::new(SPARKLINE_WINDOW),
            tokens_per_sec: MetricBuffer::new(SPARKLINE_WINDOW),
        }
    }

    /// Pull one sample from `state` for the focused PID and push it
    /// onto each buffer. No-op when the PID is no longer present in
    /// `state.annotated` (workload exited mid-card — the next tick
    /// will catch the lifecycle event and dismiss the card via the
    /// expiry path).
    ///
    /// RAM/VRAM are normalized against the system totals so the
    /// sparkline scale stays comparable across hosts. Tokens/sec
    /// currently pushes 0.0 — `AnnotatedProcess` doesn't carry a
    /// live token-rate sample today, so the row evolves but stays
    /// flat. The data path lands when the telemetry dispatcher
    /// surfaces per-PID throughput on `LiveTelemetry`; this hook is
    /// already in place.
    pub fn sample(&mut self, state: &RuntimeState) {
        let Some(proc) = state.annotated.iter().find(|p| p.pid == self.pid) else {
            return;
        };
        self.cpu.push(proc.cpu_pct);

        let total_mem_bytes = state
            .last_snapshot
            .as_ref()
            .map(|s| s.system.total_memory)
            .unwrap_or(0);
        let ram_pct = if total_mem_bytes > 0 {
            (proc.rss_mb as f64 * 1024.0 * 1024.0 / total_mem_bytes as f64) * 100.0
        } else {
            0.0
        };
        self.ram_pct.push(ram_pct as f32);

        let total_vram = state
            .last_snapshot
            .as_ref()
            .map(|s| s.gpu.total_vram_all_devices())
            .unwrap_or(0);
        let vram_pct = match (total_vram, proc.vram_bytes) {
            (t, Some(used)) if t > 0 => (used as f64 / t as f64) * 100.0,
            _ => 0.0,
        };
        self.vram_pct.push(vram_pct as f32);

        // Tokens/sec: pinned at 0.0 until the telemetry dispatcher
        // exposes a per-PID rate. The buffer evolution stays
        // consistent with the other metrics so a future row that
        // wires the data doesn't need to change the buffer shape.
        self.tokens_per_sec.push(0.0);
    }
}

/// Map a value into a block-character index 0..=7. Clamps inputs to
/// `range` before mapping so out-of-band samples don't blow up the
/// scale (e.g., CPU% can briefly exceed 100 on multi-core spikes;
/// clamp prevents them from rendering as anything taller than `█`).
fn block_for(value: f32, range: (f32, f32)) -> char {
    let (min, max) = range;
    let span = (max - min).max(f32::EPSILON);
    let norm = ((value - min) / span).clamp(0.0, 1.0);
    let idx = (norm * (BLOCKS.len() as f32 - 1.0)).round() as usize;
    BLOCKS[idx.min(BLOCKS.len() - 1)]
}

/// L17 / §5 — render a metric buffer as a Vec<Span> sparkline.
///
/// Each cell becomes one `Span` so threshold colors can land per
/// sample: a buffer whose recent values cross the §14 attention /
/// critical bands renders those individual cells in `theme.attention`
/// / `theme.critical` while the rest stay `theme.foreground`. When
/// `threshold_color` is false (tokens/sec) all cells stay foreground.
///
/// Width caps the cell count; if the buffer holds more values than
/// `width`, only the most recent `width` are rendered (no
/// downsampling — operators care about the latest second-by-second
/// trend, not a smoothed average).
pub fn sparkline_spans(
    buf: &MetricBuffer,
    width: u16,
    range: (f32, f32),
    theme: &UiTheme,
    threshold_color: bool,
) -> Vec<Span<'static>> {
    if buf.is_empty() {
        return Vec::new();
    }
    let width = width as usize;
    let take = buf.len().min(width);
    let start = buf.len() - take;
    let slice: Vec<f32> = buf.values().iter().skip(start).copied().collect();

    slice
        .into_iter()
        .map(|v| {
            let glyph = block_for(v, range);
            let color = if threshold_color {
                // `theme.bar_color` consumes a 0–100 percentage and
                // resolves to foreground / attention / critical per
                // §14 thresholds. The CPU branch sometimes exceeds
                // 100 (multi-core) — clamping here keeps the cell
                // color stable rather than oscillating into `Reset`
                // territory.
                theme.bar_color(v.clamp(0.0, 100.0) as f64)
            } else {
                theme.foreground
            };
            Span::styled(glyph.to_string(), Style::default().fg(color))
        })
        .collect()
}

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
///
/// `buffers` is the optional rolling-window state appended on each
/// tick by `run_loop`. When `None` the sparkline rows render as
/// `(collecting…)` muted placeholders — the card is open but no
/// samples have landed yet (first tick after Enter).
pub fn render(
    frame: &mut Frame,
    full: Rect,
    card: &LiveDetailCard,
    theme: &UiTheme,
    buffers: Option<&LiveDetailBuffers>,
) {
    let lines = build_lines_themed(card, theme, buffers);
    let height = lines
        .len()
        .saturating_add(2) // border top + bottom
        .clamp(CARD_MIN_HEIGHT as usize, CARD_MAX_HEIGHT as usize) as u16;
    let area = centered_rect(full, CARD_WIDTH, height);
    frame.render_widget(Clear, area);

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

/// Pre-L17 build path: returns lines without sparkline rows. Kept
/// for the existing unit tests that pin label ordering / conditional
/// row omission without a theme handle. The post-L17 render path
/// uses `build_lines_themed`.
pub fn build_lines(card: &LiveDetailCard) -> Vec<Line<'static>> {
    build_lines_with(card, None, None)
}

/// L17 / §5 — themed build path with sparkline rows. The four
/// `CPU / RAM / VRAM / Tokens/s` rows replace the L16 placeholder
/// when `buffers` is `Some(_)`; when `None`, the rows render with a
/// muted `(collecting…)` hint so the card is visibly the right
/// height the moment it opens.
pub fn build_lines_themed(
    card: &LiveDetailCard,
    theme: &UiTheme,
    buffers: Option<&LiveDetailBuffers>,
) -> Vec<Line<'static>> {
    build_lines_with(card, Some(theme), buffers)
}

fn build_lines_with(
    card: &LiveDetailCard,
    theme: Option<&UiTheme>,
    buffers: Option<&LiveDetailBuffers>,
) -> Vec<Line<'static>> {
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

    // L17 / §5 — sparkline rows. Themed path renders four per-metric
    // rows; un-themed path retains the pre-L17 placeholder line so
    // the legacy `build_lines` (used by older unit tests without a
    // theme handle) keeps its single-line height.
    match (theme, buffers) {
        (Some(t), Some(b)) => {
            lines.extend(sparkline_rows(b, t));
        }
        (Some(t), None) => {
            // Card open but no samples yet. Render four
            // `(collecting…)` rows so the height matches the
            // post-collection layout.
            for label in ["CPU", "RAM", "VRAM", "Tokens/s"] {
                lines.push(sparkline_placeholder_row(label, t));
            }
        }
        _ => {
            lines.push(Line::from(Span::styled(
                "Sparklines (CPU / RAM / VRAM / tokens) — pending L17",
                Style::default().fg(muted).add_modifier(Modifier::ITALIC),
            )));
        }
    }

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

/// Sparkline label column. Shorter than `LABEL_WIDTH` (the instant-
/// value label width) because the sparkline rows are narrower in
/// content: `CPU / RAM / VRAM / Tokens/s` fit in 10 cols and the
/// extra space goes to the sparkline cells.
const SPARK_LABEL_WIDTH: usize = 10;

fn sparkline_rows(buffers: &LiveDetailBuffers, theme: &UiTheme) -> Vec<Line<'static>> {
    vec![
        spark_row(
            "CPU",
            &buffers.cpu,
            (0.0, 100.0),
            theme,
            true,
            buffers.cpu.last(),
            |v| format!("{v:>5.1}%"),
        ),
        spark_row(
            "RAM",
            &buffers.ram_pct,
            (0.0, 100.0),
            theme,
            true,
            buffers.ram_pct.last(),
            |v| format!("{v:>5.1}%"),
        ),
        spark_row(
            "VRAM",
            &buffers.vram_pct,
            (0.0, 100.0),
            theme,
            true,
            buffers.vram_pct.last(),
            |v| format!("{v:>5.1}%"),
        ),
        spark_row(
            "Tokens/s",
            &buffers.tokens_per_sec,
            // Auto-range: tokens/sec is a rate, not a percentage.
            // Pick (0, max(buffer)) so the sparkline shows relative
            // variation across the window. When the buffer is all
            // zeros (no telemetry yet) the range collapses to (0, 1)
            // and the cells render at the bottom of the ramp — that's
            // the right visual cue for "no data".
            tokens_range(&buffers.tokens_per_sec),
            theme,
            // Tokens/s is unbounded — §14 threshold coloring doesn't
            // apply. Stays foreground regardless of value.
            false,
            buffers.tokens_per_sec.last(),
            |v| {
                if v > 0.0 {
                    format!("{v:>5.1}")
                } else {
                    "  — ".to_string()
                }
            },
        ),
    ]
}

fn tokens_range(buf: &MetricBuffer) -> (f32, f32) {
    let max = buf
        .values()
        .iter()
        .copied()
        .fold(0.0_f32, f32::max)
        .max(1.0);
    (0.0, max)
}

#[allow(clippy::too_many_arguments)]
fn spark_row(
    label: &str,
    buf: &MetricBuffer,
    range: (f32, f32),
    theme: &UiTheme,
    threshold_color: bool,
    current: Option<f32>,
    fmt_value: impl Fn(f32) -> String,
) -> Line<'static> {
    let padded_label = format!("{label:<width$}", width = SPARK_LABEL_WIDTH);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity((SPARKLINE_WIDTH as usize) + 4);
    spans.push(Span::styled(
        padded_label,
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    ));
    if buf.is_empty() {
        spans.push(Span::styled(
            format!("{:<width$}", "(collecting…)", width = SPARKLINE_WIDTH as usize),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let cells = sparkline_spans(buf, SPARKLINE_WIDTH, range, theme, threshold_color);
        let drawn = cells.len();
        spans.extend(cells);
        // Pad with spaces so trailing value column lines up across
        // partially-filled buffers.
        if drawn < SPARKLINE_WIDTH as usize {
            spans.push(Span::raw(" ".repeat(SPARKLINE_WIDTH as usize - drawn)));
        }
    }
    let trailing = match current {
        Some(v) => format!(" {}", fmt_value(v)),
        None => "       ".to_string(),
    };
    spans.push(Span::styled(trailing, Style::default().fg(theme.foreground)));
    Line::from(spans)
}

fn sparkline_placeholder_row(label: &str, theme: &UiTheme) -> Line<'static> {
    let padded_label = format!("{label:<width$}", width = SPARK_LABEL_WIDTH);
    Line::from(vec![
        Span::styled(
            padded_label,
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{:<width$}",
                "(collecting…)",
                width = SPARKLINE_WIDTH as usize
            ),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ),
    ])
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
    fn legacy_build_lines_keeps_the_pre_l17_placeholder() {
        // L17 ships the themed sparkline path; the legacy
        // `build_lines` (no theme handle) still emits the pre-L17
        // single-line placeholder for backward compatibility with
        // any test fixture that constructs cards without a theme.
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
            "legacy build_lines() must keep the pre-L17 placeholder:\n{rendered}",
        );
    }

    #[test]
    fn themed_build_with_no_buffers_renders_collecting_rows() {
        // Themed path + None buffers = four `(collecting…)` rows
        // (one per metric) so the card height matches the post-
        // collection layout from the moment Enter is pressed.
        let theme = crate::ui::theme::current_theme("dark");
        let lines = build_lines_themed(&freshly_shown(fixture("phi3-mini")), &theme, None);
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
        for label in ["CPU", "RAM", "VRAM", "Tokens/s"] {
            assert!(
                rendered.contains(label),
                "expected {label} sparkline label:\n{rendered}",
            );
        }
        let n = rendered.matches("(collecting…)").count();
        assert_eq!(n, 4, "expected 4 collecting rows, got {n}:\n{rendered}");
    }

    #[test]
    fn themed_build_with_filled_buffers_renders_sparkline_glyphs() {
        // Push a known ramp into each buffer and assert the
        // rendered Spans include at least one block character from
        // BLOCKS. The exact glyph mapping is exercised by the
        // `block_for` unit tests below; here we only need to see
        // that the row is no longer the placeholder.
        let theme = crate::ui::theme::current_theme("dark");
        let mut buffers = LiveDetailBuffers::new(4242);
        for v in [10.0, 30.0, 50.0, 70.0, 90.0] {
            buffers.cpu.push(v);
            buffers.ram_pct.push(v);
            buffers.vram_pct.push(v);
            buffers.tokens_per_sec.push(v);
        }
        let lines = build_lines_themed(
            &freshly_shown(fixture("phi3-mini")),
            &theme,
            Some(&buffers),
        );
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
            !rendered.contains("(collecting…)"),
            "filled buffers must replace the collecting placeholder:\n{rendered}"
        );
        assert!(
            BLOCKS.iter().any(|c| rendered.contains(*c)),
            "rendered card should contain at least one block glyph:\n{rendered}"
        );
    }

    #[test]
    fn block_for_maps_min_to_lowest_glyph_and_max_to_highest() {
        // 8-step ramp; pin both endpoints.
        assert_eq!(block_for(0.0, (0.0, 100.0)), '▁');
        assert_eq!(block_for(100.0, (0.0, 100.0)), '█');
    }

    #[test]
    fn block_for_clamps_out_of_range_values() {
        // Negative values clamp to the lowest glyph; values past
        // the upper bound clamp to the highest. Prevents CPU%
        // multi-core spikes from rendering as a wrap-around.
        assert_eq!(block_for(-50.0, (0.0, 100.0)), '▁');
        assert_eq!(block_for(150.0, (0.0, 100.0)), '█');
    }

    #[test]
    fn metric_buffer_drops_oldest_at_capacity() {
        let mut buf = MetricBuffer::new(3);
        buf.push(1.0);
        buf.push(2.0);
        buf.push(3.0);
        buf.push(4.0);
        assert_eq!(buf.len(), 3);
        let values: Vec<f32> = buf.values().iter().copied().collect();
        assert_eq!(values, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn metric_buffer_starts_empty() {
        let buf = MetricBuffer::new(60);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(buf.last().is_none());
    }

    #[test]
    fn sparkline_spans_emits_one_span_per_cell() {
        let theme = crate::ui::theme::current_theme("dark");
        let mut buf = MetricBuffer::new(SPARKLINE_WINDOW);
        for v in [10.0, 30.0, 50.0, 70.0, 90.0] {
            buf.push(v);
        }
        // Threshold coloring on: each cell gets its own Span so
        // attention/critical bands can land per sample.
        let spans = sparkline_spans(&buf, SPARKLINE_WIDTH, (0.0, 100.0), &theme, true);
        assert_eq!(spans.len(), 5, "expected one span per buffered sample");
    }

    #[test]
    fn sparkline_spans_threshold_coloring_marks_critical_cells() {
        // §14 — values ≥95% render in `theme.critical`.
        let theme = crate::ui::theme::current_theme("dark");
        let mut buf = MetricBuffer::new(SPARKLINE_WINDOW);
        buf.push(50.0);
        buf.push(97.0);
        let spans = sparkline_spans(&buf, SPARKLINE_WIDTH, (0.0, 100.0), &theme, true);
        assert_eq!(spans[0].style.fg, Some(theme.foreground));
        assert_eq!(spans[1].style.fg, Some(theme.critical));
    }

    #[test]
    fn sparkline_spans_threshold_off_keeps_foreground() {
        // Tokens/sec branch — `threshold_color = false` keeps every
        // cell in `theme.foreground` regardless of value, because
        // tokens/sec is a rate without a capacity ceiling.
        let theme = crate::ui::theme::current_theme("dark");
        let mut buf = MetricBuffer::new(SPARKLINE_WINDOW);
        buf.push(99.0);
        let spans = sparkline_spans(&buf, SPARKLINE_WIDTH, (0.0, 100.0), &theme, false);
        assert_eq!(spans[0].style.fg, Some(theme.foreground));
    }

    #[test]
    fn sparkline_spans_empty_buffer_returns_empty_vec() {
        let theme = crate::ui::theme::current_theme("dark");
        let buf = MetricBuffer::new(SPARKLINE_WINDOW);
        let spans = sparkline_spans(&buf, SPARKLINE_WIDTH, (0.0, 100.0), &theme, true);
        assert!(spans.is_empty());
    }

    #[test]
    fn sparkline_spans_caps_at_render_width() {
        // Buffer holds more than the render width; only the most
        // recent `width` values render.
        let theme = crate::ui::theme::current_theme("dark");
        let mut buf = MetricBuffer::new(SPARKLINE_WINDOW);
        for i in 0..SPARKLINE_WINDOW {
            buf.push(i as f32);
        }
        let spans = sparkline_spans(&buf, SPARKLINE_WIDTH, (0.0, 100.0), &theme, false);
        assert_eq!(spans.len(), SPARKLINE_WIDTH as usize);
    }
}
