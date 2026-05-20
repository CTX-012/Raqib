//! Post-mortem card ([UX-2], UI Contract v2).
//!
//! L16 / UX_CONTRACT.md §5 — this module is the **exited-workload**
//! half of the detail-card split. The running-workload half lives at
//! [`super::live_detail`]; both share dimensions, lifetime semantics,
//! and the z-order slot but render different content. Triggered when
//! `Enter` lands on a row that has run history but no live PID; the
//! Workloads-panel `Enter` path now routes to live_detail for running
//! workloads and reserves this card for the retrospective view.
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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use ux_contract::postmortem_labels;

use crate::model::{WorkloadCategory, workload_category_from_model_path};
use crate::storage::RunRecord;
use crate::storage::run_store::ExitReason;
use crate::ui::theme::UiTheme;

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
    /// Sprint-4 B14 — peak CPU% sampled across the run. Together with
    /// `avg_cpu_pct` this is the per-run detail the history overlay
    /// USED to surface as a column; B14 moves it into the card body so
    /// the history overlay can shrink to a clean chronological list.
    pub peak_cpu_pct: f32,
    pub peak_rss_mb: u64,
    /// 0 means "omit the row entirely" per UI Contract v2.
    pub peak_vram_mb: u64,
    /// `None` means "omit the row entirely" per UI Contract v2.
    pub tokens_per_sec: Option<f32>,
    /// B6 — workload taxonomy used to gate category-specific metric
    /// rows (Throughput is LLM-only; FPS would be Vision-only, etc.).
    /// `None` means "no taxonomy resolved" and category-gated rows
    /// stay hidden — safer default than rendering an LLM metric over
    /// a YOLO/ROS2 workload. Derived from `summary.model_name` via
    /// [`workload_category_from_model_path`] in `from_run_record_*`.
    pub workload_category: Option<WorkloadCategory>,
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
    /// and a freshly-computed `BaselineStatus`, with no stderr
    /// captured. Use when the runtime has no
    /// `Runtime::stderr_tail(pid)` buffer for this run (e.g. the
    /// process exited before the buffer was populated, or 30 s have
    /// passed since exit and the entry was swept).
    ///
    /// Display name resolves to the classifier's `model_name` when
    /// present, otherwise the raw process name.
    pub fn from_run_record(record: &RunRecord, baseline_status: BaselineStatus) -> Self {
        Self::from_run_record_with_stderr(record, baseline_status, Vec::new())
    }

    /// L19 — build the transient card payload, attaching the
    /// transient stderr tail captured by `Runtime`'s buffer for the
    /// exiting PID. Pure: the buffer's lifecycle (capture, 30 s
    /// expiry, dismiss-clear) lives in `Runtime`; the card just
    /// renders what it's handed.
    ///
    /// `stderr_tail` is normally `Runtime::stderr_tail(pid)` at the
    /// moment the card is constructed. Empty when no entry exists,
    /// when the entry has expired, or when no sampler has populated
    /// the buffer for this PID — in any of those cases the render
    /// path omits the stderr block entirely (no "(no stderr)" empty
    /// state per UI Contract v2).
    pub fn from_run_record_with_stderr(
        record: &RunRecord,
        baseline_status: BaselineStatus,
        stderr_tail: Vec<String>,
    ) -> Self {
        let summary = &record.summary;
        let display_name = summary
            .model_name
            .clone()
            .unwrap_or_else(|| summary.name.clone());
        // B6 — taxonomy lookup uses the model name (path-style) so
        // `phi3-mini.gguf` resolves to LLM, `yolov8n.pt` to Vision,
        // etc. When the classifier never extracted a model name we
        // stay `None` and the LLM-only Throughput row is suppressed.
        let workload_category = summary
            .model_name
            .as_deref()
            .map(|m| workload_category_from_model_path(std::path::Path::new(m)));
        Self {
            display_name,
            duration_secs: summary.uptime_secs.max(0) as u64,
            avg_cpu_pct: summary.avg_cpu_pct,
            // Sprint-4 B14 — peak CPU plumbed from the summary so the
            // card body carries both avg and peak (was history-only).
            peak_cpu_pct: summary.peak_cpu_pct,
            peak_rss_mb: summary.peak_rss_mb,
            peak_vram_mb: summary.peak_vram_mb,
            tokens_per_sec: record.metrics.tokens_per_sec_avg,
            workload_category,
            exit_reason: record.exit_reason.clone(),
            stderr_tail,
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
    /// L19 — PID of the exited process this card is showing, when
    /// known. Used by the L24 Esc cascade to clear the matching
    /// transient stderr buffer in `Runtime` on dismiss (so the
    /// buffer doesn't outlive the card's visibility). `None` for
    /// cards built without PID context — fixtures in unit tests
    /// take that path.
    pub pid: Option<u32>,
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
/// help). CAR-17 — the kill_confirm card sits at the same z-slot
/// with higher priority; dispatch ensures the two are never both
/// rendered.
///
/// L21 / §14 — title bar uses `theme.accent`; baseline headline
/// resolves through the semantic palette
/// (`critical`/`attention`/`healthy`/`muted`); the dismiss-hint
/// footer is muted.
pub fn render(frame: &mut Frame, full: Rect, card: &PostMortemCard, theme: &UiTheme) {
    let lines = build_lines_themed(card, theme);
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
                .fg(theme.accent)
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

/// Pre-L21 build path: returns plain (un-themed) lines for callers
/// that don't have a UiTheme handy. Kept for the existing unit tests
/// that pin label ordering and conditional row omission. The themed
/// version `build_lines_themed` is what `render` uses in the live
/// path; the two share their structural logic.
pub fn build_lines(card: &PostMortemCard) -> Vec<Line<'static>> {
    build_lines_with(card, None)
}

/// Themed build path — same content shape as `build_lines` but with
/// baseline-headline / dismiss-hint colors sourced from the active
/// theme.
pub fn build_lines_themed(card: &PostMortemCard, theme: &UiTheme) -> Vec<Line<'static>> {
    build_lines_with(card, Some(theme))
}

fn build_lines_with(card: &PostMortemCard, theme: Option<&UiTheme>) -> Vec<Line<'static>> {
    let pm = &card.post_mortem;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. Duration:
    lines.push(labeled("Duration:", &format_duration(pm.duration_secs)));
    // 2. Avg CPU:
    lines.push(labeled("Avg CPU:", &format!("{:.1}%", pm.avg_cpu_pct)));
    // 3. Peak CPU: (Sprint-4 B14 — per-run detail moved from the
    // history overlay column into the card body)
    lines.push(labeled("Peak CPU:", &format!("{:.1}%", pm.peak_cpu_pct)));
    // 4. Peak RAM:
    lines.push(labeled("Peak RAM:", &format_megabytes(pm.peak_rss_mb)));
    // 4. Peak GPU memory: (omit when zero/unavailable)
    if pm.peak_vram_mb > 0 {
        lines.push(labeled(
            "Peak GPU memory:",
            &format_megabytes(pm.peak_vram_mb),
        ));
    }
    // 5. Throughput: (LLM-only per B6; also omit when no tokens/sec
    // data). Non-LLM exits never had a tokens-per-sec sampler firing
    // so the value is normally `None` already — the category gate is
    // a defence-in-depth check for the case where a future sampler
    // misclassifies and populates the field for a Vision/ROS2 run.
    if pm.workload_category == Some(WorkloadCategory::LLM)
        && let Some(tps) = pm.tokens_per_sec
    {
        lines.push(labeled("Throughput:", &format!("{tps:.1} tokens/sec")));
    }
    // 6. Exited:
    lines.push(labeled("Exited:", &format_exit_reason(&pm.exit_reason)));

    // 7. blank
    lines.push(Line::from(""));

    // 8. color-coded baseline headline (if any). Themed path uses
    // the active palette; legacy un-themed callers fall back to the
    // pre-L21 ratatui named-color mapping for backward compatibility
    // with `tests/postmortem.rs::baseline_headlines_match_contract`.
    let headline = match theme {
        Some(t) => baseline_headline_themed(&pm.baseline_status, t),
        None => baseline_headline(&pm.baseline_status),
    };
    if let Some((text, style)) = headline {
        lines.push(Line::from(Span::styled(text, style)));
    }

    // 9-11. stderr block (when we have any). D6 — header text comes
    // from `postmortem_labels::LAST_STDERR` so the Linux and Windows
    // consumers share the same locked spelling ("Last stderr:");
    // amend the contract to retitle it.
    if !pm.stderr_tail.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            postmortem_labels::LAST_STDERR,
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
    let footer_fg = match theme {
        Some(t) => t.muted,
        None => ratatui::style::Color::DarkGray,
    };
    lines.push(Line::from(Span::styled(
        format!(
            "[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s",
            n = card.seconds_remaining()
        ),
        Style::default().fg(footer_fg),
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

/// Color-coded baseline headline using ratatui's named-color
/// palette. Pre-L21 default; preserved so the contract-text
/// assertions in this file's own tests stay decoupled from the
/// theme system. New render sites should call
/// `baseline_headline_themed` to pick up the active palette.
pub fn baseline_headline(status: &BaselineStatus) -> Option<(String, Style)> {
    use ratatui::style::Color;
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

/// L21 / §14 — themed baseline headline. Critical/Attention/Healthy
/// route through the semantic palette
/// (`critical`/`attention`/`healthy`); Matching reads as muted so it
/// stays a low-contrast acknowledgement rather than a celebratory
/// banner.
pub fn baseline_headline_themed(
    status: &BaselineStatus,
    theme: &UiTheme,
) -> Option<(String, Style)> {
    match status {
        BaselineStatus::NotAvailable => None,
        BaselineStatus::Critical { delta_pct } => Some((
            format!("{:.0}% slower than baseline", delta_pct),
            Style::default()
                .fg(theme.critical)
                .add_modifier(Modifier::BOLD),
        )),
        BaselineStatus::Attention { delta_pct } => Some((
            format!("{:.0}% slower than baseline", delta_pct),
            Style::default()
                .fg(theme.attention)
                .add_modifier(Modifier::BOLD),
        )),
        BaselineStatus::Healthy { abs_delta_pct } => Some((
            format!("{:.0}% faster than baseline", abs_delta_pct),
            Style::default()
                .fg(theme.healthy)
                .add_modifier(Modifier::BOLD),
        )),
        BaselineStatus::Matching => Some((
            "matches baseline".to_string(),
            Style::default().fg(theme.muted),
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
            peak_cpu_pct: 52.1,
            peak_rss_mb: 1024,
            peak_vram_mb: 4096,
            tokens_per_sec: Some(38.4),
            workload_category: Some(WorkloadCategory::LLM),
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
            pid: None,
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

    fn rendered_lines(pm: PostMortem) -> String {
        let lines = build_lines(&freshly_shown(pm));
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// B6 — LLM workloads with tokens/sec data render the Throughput
    /// row (the existing happy path stays green after the category
    /// gate landed).
    #[test]
    fn postmortem_llm_shows_tokens_per_sec() {
        let pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        assert_eq!(pm.workload_category, Some(WorkloadCategory::LLM));
        let rendered = rendered_lines(pm);
        assert!(
            rendered.contains("Throughput:"),
            "LLM postmortem with tokens_per_sec=Some must show Throughput:\n{rendered}",
        );
    }

    /// B6 — Vision workloads suppress the Throughput row even if a
    /// stale tokens_per_sec value leaked in from a misclassified
    /// sampler. The metric is meaningless for YOLO/diffusion runs.
    #[test]
    fn postmortem_vision_hides_tokens_per_sec() {
        let mut pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        pm.workload_category = Some(WorkloadCategory::Vision);
        let rendered = rendered_lines(pm);
        assert!(
            !rendered.contains("Throughput:"),
            "Vision postmortem must not render Throughput row:\n{rendered}",
        );
    }

    /// B6 — ROS2 workloads suppress the Throughput row for the same
    /// reason as Vision; the metric doesn't apply to ROS2 nodes.
    #[test]
    fn postmortem_ros2_hides_tokens_per_sec() {
        let mut pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        pm.workload_category = Some(WorkloadCategory::ROS2);
        let rendered = rendered_lines(pm);
        assert!(
            !rendered.contains("Throughput:"),
            "ROS2 postmortem must not render Throughput row:\n{rendered}",
        );
    }

    /// B6 — Unknown / not-yet-classified workloads also suppress
    /// the Throughput row. Safer to under-report than render an
    /// LLM metric over a non-LLM workload.
    #[test]
    fn postmortem_unknown_category_hides_tokens_per_sec() {
        let mut pm = fixture_post_mortem(false, BaselineStatus::NotAvailable);
        pm.workload_category = None;
        let rendered = rendered_lines(pm);
        assert!(
            !rendered.contains("Throughput:"),
            "Unknown-category postmortem must not render Throughput row:\n{rendered}",
        );
    }

    /// B6 — derivation from model name: `phi3-mini.gguf` → LLM,
    /// `yolov8n.pt` → Vision. Confirms `from_run_record_with_stderr`
    /// uses [`workload_category_from_model_path`] on the model name.
    #[test]
    fn workload_category_derives_from_model_name() {
        use crate::lifecycle::LifecycleSummary;
        use chrono::Utc;

        let mk = |model_name: Option<&str>| -> RunRecord {
            let now = Utc::now();
            let summary = LifecycleSummary {
                pid: 1,
                name: "proc".into(),
                category: None,
                model_name: model_name.map(str::to_owned),
                spawn_time: now,
                exit_time: now,
                uptime_secs: 0,
                exit_code: Some(0),
                signal: None,
                avg_cpu_pct: 0.0,
                peak_cpu_pct: 0.0,
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                samples: 0,
            };
            RunRecord::from_summary(summary)
        };

        let llm = PostMortem::from_run_record(&mk(Some("phi3-mini.gguf")), BaselineStatus::NotAvailable);
        assert_eq!(llm.workload_category, Some(WorkloadCategory::LLM));

        let vision = PostMortem::from_run_record(&mk(Some("yolov8n.pt")), BaselineStatus::NotAvailable);
        assert_eq!(vision.workload_category, Some(WorkloadCategory::Vision));

        let none = PostMortem::from_run_record(&mk(None), BaselineStatus::NotAvailable);
        assert_eq!(none.workload_category, None);
    }

    /// Stderr block renders only when `stderr_tail` is non-empty,
    /// header reads `postmortem_labels::LAST_STDERR`, and clamps to
    /// last 3 (newest at the bottom).
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
            rendered.contains(postmortem_labels::LAST_STDERR),
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
        assert!(!rendered.contains(postmortem_labels::LAST_STDERR));
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
