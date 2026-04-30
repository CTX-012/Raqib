//! Post-mortem card ([UX-2], UI Contract v2).
//!
//! Centered overlay shown for 30 seconds after an AI workload exits.
//! Surfaces the run summary so the operator sees the "oh, that's
//! what happened" moment without having to invoke
//! `edge_monitor history` after the fact.
//!
//! # UI Contract — locked across Linux and Windows
//!
//! See `UI_CONTRACT.md` (v2). The v2 contract supersedes v1; key
//! differences picked up here:
//!
//! * Title is the workload's `display_name`, not the literal string
//!   `Run summary`.
//! * Field set is `Duration`, `Avg CPU`, `Peak RAM`, `Peak GPU memory`
//!   (omitted when zero), `Throughput` (omitted when no tokens/sec
//!   data), `Exited`. No `Compared to baseline:` field row.
//! * Baseline indicator is a color-coded headline beneath the field
//!   block, not a labeled row.
//! * Stderr is **ephemeral** — built at exit time on a transient
//!   `PostMortem` struct, never persisted to `RunRecord`.
//! * Card is fixed 64 columns wide (was 60% in v1); height is
//!   computed from content and clamped to `[8, 22]` rows.
//!
//! Triggered by AI-classified exits only (exec-wrapped *and*
//! headless-monitored). Non-AI exits stay silent — they would be
//! noise, not signal.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::storage::RunRecord;
use crate::storage::run_store::ExitReason;

/// Transient struct constructed by the runtime at exit time and
/// handed to the renderer. **Not persisted** — `stderr_tail` is
/// dropped when the card is dismissed (per UI Contract v2 "Stderr is
/// ephemeral"). Cheap to clone; rendered fields are pre-computed so
/// the render path is pure.
#[derive(Debug, Clone)]
pub struct PostMortem {
    /// Title shown in the border-top of the card. Resolves to
    /// `model_name` when the classifier extracted one, otherwise the
    /// process name.
    pub display_name: String,
    pub duration_secs: u64,
    /// Mean CPU% across the run (one core == 100%).
    pub avg_cpu_pct: f32,
    pub peak_rss_mb: u64,
    /// 0 means "omit the row entirely" per UI Contract v2.
    pub peak_vram_mb: u64,
    /// `None` means "omit the row entirely" per UI Contract v2.
    pub tokens_per_sec: Option<f32>,
    pub exit_reason: ExitReason,
    /// Last lines of stderr captured by the runtime / exec wrapper.
    /// Empty for headless-monitored exits (the runtime can't read
    /// stderr without owning stdio). Capped at 64 by the upstream
    /// buffer; the render path further clamps to the last 3 lines.
    pub stderr_tail: Vec<String>,
    pub baseline_status: BaselineStatus,
}

impl PostMortem {
    /// Build the transient card payload from a persisted `RunRecord`
    /// and a freshly-computed `BaselineStatus`. Stderr is left empty
    /// — the headless monitor path doesn't own child stdio (the
    /// exec wrapper, when it lands, will populate this directly
    /// before constructing the card). Display name resolves to the
    /// classifier's `model_name` when present, otherwise the raw
    /// process name.
    pub fn from_run_record(record: &RunRecord, baseline_status: BaselineStatus) -> Self {
        let summary = &record.summary;
        let display_name = summary
            .model_name
            .clone()
            .unwrap_or_else(|| summary.name.clone());
        Self {
            display_name,
            duration_secs: summary.uptime_secs.max(0) as u64,
            avg_cpu_pct: summary.avg_cpu_pct,
            peak_rss_mb: summary.peak_rss_mb,
            peak_vram_mb: summary.peak_vram_mb,
            tokens_per_sec: record.metrics.tokens_per_sec_avg,
            exit_reason: record.exit_reason.clone(),
            stderr_tail: Vec::new(),
            baseline_status,
        }
    }
}

/// Color-coded baseline summary headline. Independent of
/// `RegressionConfig` thresholds — the contract pins these bands
/// directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BaselineStatus {
    /// `delta_pct >= 20.0` — current run is materially slower.
    Critical { delta_pct: f32 },
    /// `delta_pct >= 10.0` — current run is somewhat slower.
    Attention { delta_pct: f32 },
    /// `delta_pct <= -10.0` — current run is faster than baseline.
    Healthy { abs_delta_pct: f32 },
    /// Within ±10% of baseline.
    Matching,
    /// No baseline available (first run, or below
    /// `min_baseline_samples`). No headline is rendered.
    NotAvailable,
}

impl BaselineStatus {
    /// Compute the status from the run's headline metric (typically
    /// tokens/sec) and the baseline mean of the same metric. Pure;
    /// returns `NotAvailable` when either side is missing or when
    /// the baseline mean is non-finite or zero (division would lie).
    pub fn from_metric(current: Option<f32>, baseline_mean: Option<f32>) -> Self {
        let (Some(cur), Some(base)) = (current, baseline_mean) else {
            return Self::NotAvailable;
        };
        if !base.is_finite() || base.abs() < f32::EPSILON {
            return Self::NotAvailable;
        }
        // tokens/sec is "higher is better" — current LOWER than
        // baseline means SLOWER (positive delta_pct).
        let raw_pct = (cur - base) / base * 100.0;
        let delta_pct = -raw_pct;

        if delta_pct >= 20.0 {
            Self::Critical { delta_pct }
        } else if delta_pct >= 10.0 {
            Self::Attention { delta_pct }
        } else if delta_pct <= -10.0 {
            Self::Healthy {
                abs_delta_pct: delta_pct.abs(),
            }
        } else {
            Self::Matching
        }
    }
}

/// Snapshot of the most recent post-mortem-eligible exit.
///
/// Replaces any prior snapshot — latest wins, no queue. Two cards
/// in quick succession would compete for screen space; one-card-at-
/// a-time is simpler and avoids the operator missing a *newer*
/// exit because they're still reading the previous one.
#[derive(Debug, Clone)]
pub struct PostMortemCard {
    pub post_mortem: PostMortem,
    pub shown_at: Instant,
}

impl PostMortemCard {
    /// 30-second auto-dismiss window, locked by UI Contract v2.
    pub const WINDOW: Duration = Duration::from_secs(30);

    pub fn is_expired(&self) -> bool {
        self.shown_at.elapsed() >= Self::WINDOW
    }

    /// Seconds remaining, rounded UP so a freshly-shown card reads
    /// `30s` rather than `29s` for the first 999 ms.
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

/// Card width in columns, locked by UI Contract v2.
pub const CARD_WIDTH: u16 = 64;
/// Card height bounds, locked by UI Contract v2.
pub const CARD_MIN_HEIGHT: u16 = 8;
pub const CARD_MAX_HEIGHT: u16 = 22;
/// Label column width — longest label `Peak GPU memory:` is 16 chars
/// plus 2 padding to column 19 where values left-align.
const LABEL_WIDTH: usize = 18;
/// Stderr clamp — UI Contract v2 caps to "up to 3" stderr lines.
const STDERR_LINES_VISIBLE: usize = 3;

/// Render the centered post-mortem card. Called last in the panels
/// render path so the card sits above every other panel (history /
/// help / armed banner).
pub fn render(frame: &mut Frame, full: Rect, card: &PostMortemCard) {
    let lines = build_lines(card);
    let height = lines
        .len()
        .saturating_add(2) // border top + bottom
        .clamp(CARD_MIN_HEIGHT as usize, CARD_MAX_HEIGHT as usize) as u16;
    let area = centered_rect(full, CARD_WIDTH, height);
    frame.render_widget(Clear, area);

    let title = format!(" {} ", card.post_mortem.display_name);
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Pad 1 column on each side inside the border per UI Contract.
    let padded = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), padded);
}

/// Build the inner-rect line list. Public for unit-testing the field
/// labels + ordering without spinning a real frame.
pub fn build_lines(card: &PostMortemCard) -> Vec<Line<'static>> {
    let pm = &card.post_mortem;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. Duration:
    lines.push(labeled("Duration:", &format_duration(pm.duration_secs)));
    // 2. Avg CPU:
    lines.push(labeled("Avg CPU:", &format!("{:.1}%", pm.avg_cpu_pct)));
    // 3. Peak RAM:
    lines.push(labeled("Peak RAM:", &format_megabytes(pm.peak_rss_mb)));
    // 4. Peak GPU memory: (omit when zero/unavailable)
    if pm.peak_vram_mb > 0 {
        lines.push(labeled(
            "Peak GPU memory:",
            &format_megabytes(pm.peak_vram_mb),
        ));
    }
    // 5. Throughput: (omit when no tokens/sec data)
    if let Some(tps) = pm.tokens_per_sec {
        lines.push(labeled("Throughput:", &format!("{tps:.1} tokens/sec")));
    }
    // 6. Exited:
    lines.push(labeled("Exited:", &format_exit_reason(&pm.exit_reason)));

    // 7. blank
    lines.push(Line::from(""));

    // 8. color-coded baseline headline (if any)
    if let Some((text, style)) = baseline_headline(&pm.baseline_status) {
        lines.push(Line::from(Span::styled(text, style)));
    }

    // 9-11. stderr block (when we have any)
    if !pm.stderr_tail.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Last stderr lines:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        // Inner width = card width - 2 border - 2 padding = CARD_WIDTH - 4.
        let inner_width = (CARD_WIDTH as usize).saturating_sub(4);
        let tail: Vec<&String> = pm
            .stderr_tail
            .iter()
            .rev()
            .take(STDERR_LINES_VISIBLE)
            .collect();
        for line in tail.iter().rev() {
            lines.push(Line::from(Span::raw(clip(line, inner_width))));
        }
    }

    // 12-13. blank + footer
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s",
            n = card.seconds_remaining()
        ),
        Style::default().fg(Color::DarkGray),
    )));

    lines
}

/// One labeled row. Bold label padded to `LABEL_WIDTH`, then plain
/// value at column 19 per UI Contract v2 layout.
fn labeled(label: &str, value: &str) -> Line<'static> {
    let padded_label = format!("{label:<width$}", width = LABEL_WIDTH);
    Line::from(vec![
        Span::styled(
            padded_label,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

/// Color-coded baseline headline. Returns `None` when the status is
/// `NotAvailable` (no headline rendered for first runs). Public so
/// integration tests can pin the verbatim contract strings.
pub fn baseline_headline(status: &BaselineStatus) -> Option<(String, Style)> {
    match status {
        BaselineStatus::NotAvailable => None,
        BaselineStatus::Critical { delta_pct } => Some((
            format!("{:.0}% slower than baseline", delta_pct),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )),
        BaselineStatus::Attention { delta_pct } => Some((
            format!("{:.0}% slower than baseline", delta_pct),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        BaselineStatus::Healthy { abs_delta_pct } => Some((
            format!("{:.0}% faster than baseline", abs_delta_pct),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        BaselineStatus::Matching => Some((
            "matches baseline".to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    }
}

pub fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Format `mb` as a human-readable size at 1-decimal precision per
/// UI Contract v2. Uses MB up to 1024, then GB.
pub fn format_megabytes(mb: u64) -> String {
    if mb < 1024 {
        format!("{} MB", mb)
    } else {
        let gb = mb as f32 / 1024.0;
        format!("{gb:.1} GB")
    }
}

pub fn format_exit_reason(reason: &ExitReason) -> String {
    match reason {
        ExitReason::CleanExit => "cleanly".into(),
        ExitReason::UserSignal { signal } => format!("user signal ({signal})"),
        ExitReason::GovernorKill { reason } => format!("killed by governor ({reason})"),
        ExitReason::Segfault => "segfault".into(),
        ExitReason::OutOfMemory { ram, vram } => match (ram, vram) {
            (true, true) => "killed by system (out of RAM and GPU memory)".into(),
            (true, false) => "killed by system (out of RAM)".into(),
            (false, true) => "killed by system (out of GPU memory)".into(),
            (false, false) => "killed by system (out of memory)".into(),
        },
        ExitReason::CudaError { last_msg } => match last_msg {
            Some(m) => format!("CUDA error: {m}"),
            None => "CUDA error".into(),
        },
        ExitReason::Crash { exit_code } => format!("crashed (exit {exit_code})"),
        ExitReason::Unknown => "unknown".into(),
    }
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Centered rect of fixed width and computed height.
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

    fn fixture_post_mortem(with_stderr: bool, status: BaselineStatus) -> PostMortem {
        PostMortem {
            display_name: "phi3-mini".into(),
            duration_secs: 65,
            avg_cpu_pct: 38.4,
            peak_rss_mb: 1024,
            peak_vram_mb: 4096,
            tokens_per_sec: Some(38.4),
            exit_reason: ExitReason::CleanExit,
            stderr_tail: if with_stderr {
                vec![
                    "loading model weights...".into(),
                    "warmup pass complete".into(),
                    "exiting cleanly".into(),
                ]
            } else {
                Vec::new()
            },
            baseline_status: status,
        }
    }

    fn freshly_shown(pm: PostMortem) -> PostMortemCard {
        PostMortemCard {
            post_mortem: pm,
            shown_at: Instant::now(),
        }
    }

    #[test]
    fn duration_formats_at_each_band() {
        assert_eq!(format_duration(5), "5s");
        assert_eq!(format_duration(65), "1m 5s");
        assert_eq!(format_duration(3725), "1h 2m 5s");
    }

    #[test]
    fn megabytes_formats_with_one_decimal() {
        assert_eq!(format_megabytes(0), "0 MB");
        assert_eq!(format_megabytes(512), "512 MB");
        assert_eq!(format_megabytes(1024), "1.0 GB");
        assert_eq!(format_megabytes(4096), "4.0 GB");
        assert_eq!(format_megabytes(1500), "1.5 GB");
    }

    #[test]
    fn clip_leaves_short_strings_alone() {
        assert_eq!(clip("hello", 10), "hello");
    }

    #[test]
    fn clip_ellipsizes_long_strings() {
        let s = "a".repeat(100);
        let out = clip(&s, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn seconds_remaining_counts_down_from_thirty() {
        let card = freshly_shown(fixture_post_mortem(false, BaselineStatus::NotAvailable));
        // Just shown; the integer-second view rounds up from
        // ~29.999... to 30.
        assert_eq!(card.seconds_remaining(), 30);
        assert!(!card.is_expired());
    }

    #[test]
    fn expired_after_window() {
        let mut card = freshly_shown(fixture_post_mortem(false, BaselineStatus::NotAvailable));
        card.shown_at = Instant::now() - Duration::from_secs(31);
        assert_eq!(card.seconds_remaining(), 0);
        assert!(card.is_expired());
    }

    /// UI Contract v2 — six-or-fewer required field labels in the
    /// documented order. Peak GPU memory and Throughput are
    /// conditional, but when present they appear in this order.
    #[test]
    fn build_lines_emits_required_fields_in_order() {
        let card = freshly_shown(fixture_post_mortem(false, BaselineStatus::NotAvailable));
        let lines = build_lines(&card);
        let labels: Vec<String> = lines
            .iter()
            .filter_map(|l| l.spans.first().map(|s| s.content.to_string()))
            .map(|s| s.trim_end().to_string())
            .collect();

        let required_order = [
            "Duration:",
            "Avg CPU:",
            "Peak RAM:",
            "Peak GPU memory:",
            "Throughput:",
            "Exited:",
        ];
        let mut idx = 0;
        for label in labels.iter() {
            if idx < required_order.len() && label == required_order[idx] {
                idx += 1;
            }
        }
        assert_eq!(
            idx,
            required_order.len(),
            "field order violated; got labels: {labels:?}",
        );
    }

    /// `Peak GPU memory:` is omitted when peak_vram_mb is 0
    /// (UI Contract v2 conditional-omit rule).
    #[test]
    fn peak_gpu_memory_row_is_omitted_when_zero() {
        let mut pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        pm.peak_vram_mb = 0;
        let lines = build_lines(&freshly_shown(pm));
        let rendered: String = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("Peak GPU memory:"),
            "peak_vram_mb=0 must omit the row entirely:\n{rendered}",
        );
    }

    /// `Throughput:` is omitted when tokens_per_sec is None.
    #[test]
    fn throughput_row_is_omitted_when_no_tokens_per_sec() {
        let mut pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        pm.tokens_per_sec = None;
        let lines = build_lines(&freshly_shown(pm));
        let rendered: String = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("Throughput:"),
            "tokens_per_sec=None must omit the row entirely:\n{rendered}",
        );
    }

    /// Stderr block renders only when `stderr_tail` is non-empty,
    /// header reads `Last stderr lines:`, and clamps to last 3
    /// (newest at the bottom).
    #[test]
    fn stderr_block_renders_only_when_present_and_clamps_to_three() {
        let mut pm = fixture_post_mortem(true, BaselineStatus::NotAvailable);
        pm.stderr_tail = vec![
            "old line 1".into(),
            "old line 2".into(),
            "newer 3".into(),
            "newer 4".into(),
            "newest 5".into(),
        ];
        let lines = build_lines(&freshly_shown(pm));
        let rendered: String = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("Last stderr lines:"),
            "stderr header missing:\n{rendered}",
        );
        assert!(!rendered.contains("old line 1"));
        assert!(!rendered.contains("old line 2"));
        assert!(rendered.contains("newer 3"));
        assert!(rendered.contains("newer 4"));
        assert!(rendered.contains("newest 5"));
    }

    #[test]
    fn no_stderr_block_when_tail_empty() {
        let pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        let lines = build_lines(&freshly_shown(pm));
        let rendered: String = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("Last stderr lines:"));
    }

    /// UI Contract v2 — verbatim baseline headlines for each band.
    #[test]
    fn baseline_headlines_match_contract() {
        let (text, _) =
            baseline_headline(&BaselineStatus::Critical { delta_pct: 30.0 }).unwrap();
        assert_eq!(text, "30% slower than baseline");

        let (text, _) =
            baseline_headline(&BaselineStatus::Attention { delta_pct: 15.0 }).unwrap();
        assert_eq!(text, "15% slower than baseline");

        let (text, _) =
            baseline_headline(&BaselineStatus::Healthy { abs_delta_pct: 12.0 }).unwrap();
        assert_eq!(text, "12% faster than baseline");

        let (text, _) = baseline_headline(&BaselineStatus::Matching).unwrap();
        assert_eq!(text, "matches baseline");

        assert!(baseline_headline(&BaselineStatus::NotAvailable).is_none());
    }

    /// Bands derived from the metric: ≥20 critical, ≥10 attention,
    /// ≤-10 healthy, otherwise matching.
    #[test]
    fn baseline_status_bands_metric_correctly() {
        // 30% slower (current ~70% of baseline).
        assert!(matches!(
            BaselineStatus::from_metric(Some(28.0), Some(40.0)),
            BaselineStatus::Critical { .. }
        ));
        // 12% slower.
        assert!(matches!(
            BaselineStatus::from_metric(Some(35.2), Some(40.0)),
            BaselineStatus::Attention { .. }
        ));
        // 15% faster.
        assert!(matches!(
            BaselineStatus::from_metric(Some(46.0), Some(40.0)),
            BaselineStatus::Healthy { .. }
        ));
        // 5% slower — within ±10 of baseline.
        assert!(matches!(
            BaselineStatus::from_metric(Some(38.0), Some(40.0)),
            BaselineStatus::Matching
        ));
        // No baseline.
        assert!(matches!(
            BaselineStatus::from_metric(Some(40.0), None),
            BaselineStatus::NotAvailable
        ));
        // Zero baseline (division would lie).
        assert!(matches!(
            BaselineStatus::from_metric(Some(40.0), Some(0.0)),
            BaselineStatus::NotAvailable
        ));
    }

    /// Card title comes from `display_name` (UI Contract v2 — was the
    /// literal `Run summary` in v1).
    #[test]
    fn card_title_uses_display_name() {
        let card = freshly_shown(fixture_post_mortem(false, BaselineStatus::NotAvailable));
        assert_eq!(card.post_mortem.display_name, "phi3-mini");
    }
}
