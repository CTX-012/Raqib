//! Armed-kill banner ([UX-1]).
//!
//! Top-of-screen red strip shown when the user has pressed `k` once and
//! a second `k` press would dispatch a real signal. Includes a 5-second
//! auto-disarm countdown so the armed state doesn't linger silently.
//!
//! UI Contract — locked across Linux and Windows (per
//! IMPLEMENTATION_LINUX_TUI_UX.md):
//!   * 5 second window, integer-second countdown.
//!   * Red background, white bold foreground.
//!   * Two body shapes — normal and ALLOWLISTED — verbatim strings.
//!
//! Pure render: snapshot data is owned by `App`; this module knows
//! nothing about the kill flow. Mutation paths (arm, disarm, confirm)
//! live in `app.rs` so the banner stays a thin presentation layer.
//!
//! Why a separate module instead of inlining into `panels::mod.rs`:
//! the banner has its own contract (timing, colors, two textual
//! variants) that's easier to test in isolation. Mirrors how
//! `history_overlay.rs` is split out.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;

/// Snapshot of the armed-kill state. Owned by `App` and replaced
/// wholesale on each arm; never mutated in place.
#[derive(Debug, Clone)]
pub struct ArmedKill {
    pub pid: u32,
    pub name: String,
    pub allowlisted: bool,
    pub armed_at: Instant,
}

impl ArmedKill {
    /// Locked by UI Contract. Do not lower without filing a
    /// cross-platform conflict in BUILDER_STATUS.md.
    pub const WINDOW: Duration = Duration::from_secs(5);

    pub fn is_expired(&self) -> bool {
        self.armed_at.elapsed() >= Self::WINDOW
    }

    /// Seconds remaining for the body's `{n}s` slot, rounded UP so
    /// a freshly-armed kill shows `5s` rather than `4s` for the first
    /// 999 ms (saturating_sub + `.as_secs()` would truncate). Saturates
    /// at 0 once expired so callers don't observe negative values
    /// during the brief window between expiry and the next
    /// `tick_overlays` sweep.
    pub fn seconds_remaining(&self) -> u64 {
        let remaining = Self::WINDOW.saturating_sub(self.armed_at.elapsed());
        let secs = remaining.as_secs();
        if remaining.subsec_nanos() > 0 {
            secs + 1
        } else {
            secs
        }
    }
}

/// Render the banner into `area`. Caller is responsible for only
/// allocating the row when an `ArmedKill` is present — otherwise an
/// empty red strip would render whenever no kill is armed.
pub fn render(frame: &mut Frame, area: Rect, armed: &ArmedKill) {
    let body = format_body(armed);
    let style = Style::default()
        .bg(Color::Red)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(Paragraph::new(body).style(style), area);
}

/// Pure formatter — exposed so the cascading-priority unit tests can
/// pin the exact UI Contract strings without spinning a `Frame`.
pub fn format_body(armed: &ArmedKill) -> String {
    if armed.allowlisted {
        format!(
            "ARMED kill PID={pid} ({name}) — ALLOWLISTED, press k to override — {n}s",
            pid = armed.pid,
            name = armed.name,
            n = armed.seconds_remaining(),
        )
    } else {
        format!(
            "ARMED kill PID={pid} ({name}) — press k to confirm, Esc/5s to disarm — {n}s",
            pid = armed.pid,
            name = armed.name,
            n = armed.seconds_remaining(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn freshly_armed(allowlisted: bool) -> ArmedKill {
        ArmedKill {
            pid: 4242,
            name: "ollama".into(),
            allowlisted,
            armed_at: Instant::now(),
        }
    }

    #[test]
    fn seconds_remaining_counts_down() {
        let armed = freshly_armed(false);
        assert_eq!(armed.seconds_remaining(), 5);
        assert!(!armed.is_expired());
    }

    #[test]
    fn expired_after_window() {
        let armed = ArmedKill {
            pid: 4242,
            name: "ollama".into(),
            allowlisted: false,
            armed_at: Instant::now() - Duration::from_secs(6),
        };
        assert_eq!(armed.seconds_remaining(), 0);
        assert!(armed.is_expired());
    }

    /// UI Contract: the normal body is exactly this format, including
    /// em-dash separators. Pin it so a casual cleanup pass can't
    /// silently rewrite the operator's muscle-memory cue.
    #[test]
    fn normal_body_matches_ui_contract() {
        let body = format_body(&freshly_armed(false));
        assert_eq!(
            body,
            "ARMED kill PID=4242 (ollama) — press k to confirm, Esc/5s to disarm — 5s",
        );
    }

    /// UI Contract: allowlisted variant uses the ALLOWLISTED token
    /// and "press k to override" phrasing.
    #[test]
    fn allowlisted_body_matches_ui_contract() {
        let body = format_body(&freshly_armed(true));
        assert_eq!(
            body,
            "ARMED kill PID=4242 (ollama) — ALLOWLISTED, press k to override — 5s",
        );
    }
}
