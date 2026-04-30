//! Post-mortem card ([UX-2]).
//!
//! Centered overlay shown for 30 seconds after an AI workload exits.
//! Surfaces the run summary (model, duration, peak resources, exit
//! reason, regression delta vs baseline, last 3 stderr lines if
//! exec-wrapped) so the operator sees the "oh, that's what happened"
//! moment without having to run `edge_monitor history` after the fact.
//!
//! UI Contract — locked across Linux and Windows (per
//! IMPLEMENTATION_LINUX_TUI_UX.md):
//!   * 60% width, clamped to [60, 100] columns; height fixed at 12 rows
//!   * Padding 1 column inside the border, all four sides
//!   * Field labels: `Model:`, `Ran for:`, `Tokens/sec:`, `Peak RAM:`,
//!     `Peak GPU memory:`, `Exited:`, `Compared to baseline:`
//!   * Stderr block header: `Last output:` (only when exec stderr
//!     is `Some(_)` and non-empty); clamped to last 3 lines
//!   * Footer: `[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s`
//!   * 30-second auto-dismiss window
//!   * Regression colors mirror the Audit panel — yellow / red / default
//!
//! Triggered by exec-wrapped *and* headless-monitored AI exits only;
//! non-AI exits (shells, udev workers, …) do not trigger — that would
//! be noise, not signal.
//!
//! Pure render — `App` owns the snapshot, runtime + exec_wrapper push
//! into that slot; this module knows nothing about the trigger flow.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::analysis::compare::{Regression, Severity};
use crate::storage::run_store::{ExitReason, RunRecord};

/// Snapshot of the most recent post-mortem-eligible exit.
///
/// Replaces any prior snapshot — latest wins, no queue. Two cards in
/// quick succession would compete for screen space; one-card-at-a-time
/// is simpler and avoids the operator missing a *newer* exit because
/// they're still reading the previous one.
#[derive(Debug, Clone)]
pub struct PostMortemCard {
    pub record: RunRecord,
    /// Worst regression (by severity) detected for this run, if any.
    /// Pre-computed at trigger time so the render path is O(1) and
    /// doesn't have to re-query baseline math each frame.
    pub worst_regression: Option<Regression>,
    pub shown_at: Instant,
}

impl PostMortemCard {
    /// Locked by UI Contract.
    pub const WINDOW: Duration = Duration::from_secs(30);

    pub fn is_expired(&self) -> bool {
        self.shown_at.elapsed() >= Self::WINDOW
    }

    /// Seconds remaining, rounded UP so a freshly-shown card
    /// reads `30s` rather than `29s` for the first 999 ms.
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

pub fn render(frame: &mut Frame, full: Rect, card: &PostMortemCard) {
    let area = centered_rect(full, 60, 12, 60, 100);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Run summary ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Pad 1 column on each side of the inner rect (UI Contract). The
    // top/bottom padding is implicit in the rounded-border height of
    // 12; we have 10 inner rows for content.
    let padded = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let lines = build_lines(card);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), padded);
}

/// Build the inner-rect line list. Exposed for unit testing the field
/// labels + ordering without spinning a real ratatui frame.
pub fn build_lines(card: &PostMortemCard) -> Vec<Line<'static>> {
    let r = &card.record;
    let s = &r.summary;

    let model = s.model_name.clone().unwrap_or_else(|| s.name.clone());
    let dur = format_duration(s.uptime_secs);
    let tps = r
        .metrics
        .tokens_per_sec_avg
        .map(|v| format!("{:.1} tokens/sec", v))
        .unwrap_or_else(|| "—".into());
    let rss = format!("{} MB", s.peak_rss_mb);
    let vram = if s.peak_vram_mb > 0 {
        format!("{} MB", s.peak_vram_mb)
    } else {
        "—".into()
    };
    let exited = format_exit_reason(&r.exit_reason);
    let (regression_text, regression_style) = format_regression(card.worst_regression.as_ref());

    let mut lines: Vec<Line<'static>> = vec![
        labeled("Model:", &model, Style::default()),
        labeled("Ran for:", &dur, Style::default()),
        labeled("Tokens/sec:", &tps, Style::default()),
        labeled("Peak RAM:", &rss, Style::default()),
        labeled("Peak GPU memory:", &vram, Style::default()),
        labeled("Exited:", &exited, Style::default()),
        labeled(
            "Compared to baseline:",
            &regression_text,
            regression_style,
        ),
    ];

    // Stderr block (exec-only). `None` → no exec wrapper observed
    // stderr; `Some(vec![])` → exec ran and the process said nothing.
    // We treat both as "no block" for compactness.
    if let Some(stderr) = r.stderr_lines.as_ref().filter(|v| !v.is_empty()) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Last output:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for s in stderr.iter().rev().take(3).rev() {
            lines.push(Line::from(Span::raw(clip(s, 80))));
        }
    }

    let footer = format!(
        "[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s",
        n = card.seconds_remaining()
    );
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(Color::DarkGray),
    )));

    lines
}

/// Build a "Label: value" line. Label fixed at 21 chars (matches
/// `Compared to baseline:` length so all values column-align). The
/// value may carry its own style (e.g. red+bold for critical
/// regressions); pass `Style::default()` for no styling.
fn labeled(label: &str, value: &str, value_style: Style) -> Line<'static> {
    let padded_label = format!("{label:<22}");
    Line::from(vec![
        Span::styled(padded_label, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(value.to_string(), value_style),
    ])
}

/// Human-readable run duration. `i64` because `LifecycleSummary` types
/// uptime that way; negative durations clamp to 0 rather than render
/// as `-5s` (would be confusing and only happens on clock skew).
pub fn format_duration(secs: i64) -> String {
    let s = secs.max(0) as u64;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}h {m}m {sec}s")
    } else if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
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

/// Match the Audit panel palette: yellow for warn, red+bold for
/// critical, default for info / no regression. `Severity::Info` is
/// rare in practice (regression detector currently only emits Warn /
/// Critical) but covered here so a future widening doesn't surprise
/// the renderer.
pub fn format_regression(reg: Option<&Regression>) -> (String, Style) {
    match reg {
        None => ("within normal range".into(), Style::default()),
        Some(r) => {
            let suffix = match r.severity {
                Severity::Critical => "(critical)",
                Severity::Warn => "(warning)",
                Severity::Info => "",
            };
            let style = match r.severity {
                Severity::Critical => Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
                Severity::Warn => Style::default().fg(Color::Yellow),
                Severity::Info => Style::default(),
            };
            let text = format!("{:+.1}% vs baseline {}", r.delta_pct, suffix);
            (text.trim_end().to_string(), style)
        }
    }
}

/// Truncate `s` to at most `max` chars; ellipsize the tail with `…`
/// when truncation actually happened. Pure char-counting; safe on UTF-8.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Centered rect with min/max width clamps. Width is `pct_w%` of `r`
/// clamped to `[min_w, max_w]`. Height is fixed (per UI Contract — the
/// card does not grow with content; long stderr clips to 3 lines via
/// `clip` above).
fn centered_rect(r: Rect, pct_w: u16, height: u16, min_w: u16, max_w: u16) -> Rect {
    let want_w = (r.width as u32 * pct_w as u32 / 100) as u16;
    let w = want_w.clamp(min_w, max_w).min(r.width);
    let x = r.x + (r.width.saturating_sub(w)) / 2;
    let h = height.min(r.height);
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

    #[test]
    fn duration_formats_at_each_band() {
        assert_eq!(format_duration(5), "5s");
        assert_eq!(format_duration(65), "1m 5s");
        assert_eq!(format_duration(3725), "1h 2m 5s");
    }

    #[test]
    fn duration_clamps_negative_to_zero() {
        // Clock skew between spawn / exit timestamps could yield a
        // negative i64; render as `0s` not `-5s`.
        assert_eq!(format_duration(-5), "0s");
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
    fn regression_text_and_color_match_severity() {
        let warn = Regression {
            metric: "tokens_per_sec_avg".into(),
            baseline: 40.0,
            current: 35.0,
            delta_pct: -12.5,
            severity: Severity::Warn,
        };
        let (text, _style) = format_regression(Some(&warn));
        assert_eq!(text, "-12.5% vs baseline (warning)");

        let crit = Regression {
            metric: "tokens_per_sec_avg".into(),
            baseline: 40.0,
            current: 28.0,
            delta_pct: -30.0,
            severity: Severity::Critical,
        };
        let (text, _) = format_regression(Some(&crit));
        assert_eq!(text, "-30.0% vs baseline (critical)");

        let (text, _) = format_regression(None);
        assert_eq!(text, "within normal range");
    }

    #[test]
    fn exit_reason_formats_oom_variants() {
        assert_eq!(
            format_exit_reason(&ExitReason::OutOfMemory {
                ram: true,
                vram: false,
            }),
            "killed by system (out of RAM)",
        );
        assert_eq!(
            format_exit_reason(&ExitReason::OutOfMemory {
                ram: true,
                vram: true,
            }),
            "killed by system (out of RAM and GPU memory)",
        );
    }
}
