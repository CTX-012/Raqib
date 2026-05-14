//! L16 / UX_CONTRACT.md §5 — live-detail card integration tests.
//!
//! Mirrors `tests/postmortem_e2e.rs` for the running-workload half of
//! the card split. Pins:
//!   - construction from a `LiveDetail` snapshot
//!   - 30s window lifetime + seconds-remaining counter
//!   - dimension parity with the post-mortem card (the two cards must
//!     occupy the same overlay slot, so divergence here would let one
//!     kind clip while the other doesn't)
//!   - render shape — title contains the workload name + `(live)`
//!     marker, body carries the per-metric labels, and the L17
//!     sparkline placeholder is present so future rows have a known
//!     swap target
//!   - title fg picks up the active theme (L20 plumbing invariant)

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;

use edge_monitor::ui::panels::live_detail::{
    self, LiveDetail, LiveDetailCard,
};
use edge_monitor::ui::panels::postmortem;
use edge_monitor::ui::theme::current_theme;

fn fixture(name: &str) -> LiveDetail {
    LiveDetail {
        display_name: name.to_string(),
        pid: 4242,
        cpu_pct: 47.3,
        rss_mb: 2048,
        vram_mb: Some(4096),
        tokens_per_sec: Some(38.4),
    }
}

#[test]
fn live_card_new_seconds_remaining_starts_at_thirty() {
    let card = LiveDetailCard::new(fixture("phi3-mini"));
    assert_eq!(card.seconds_remaining(), 30);
    assert!(!card.is_expired());
}

#[test]
fn live_card_expires_after_window() {
    let mut card = LiveDetailCard::new(fixture("phi3-mini"));
    card.shown_at = std::time::Instant::now() - Duration::from_secs(31);
    assert!(card.is_expired());
    assert_eq!(card.seconds_remaining(), 0);
}

#[test]
fn live_card_and_postmortem_share_dimensions() {
    // §5 split locks the two cards to identical dimensions so the
    // overlay slot rendering doesn't shift between kinds. Re-checked
    // at the integration level (the unit test in `live_detail.rs`
    // pins the same invariant) because a future contract amendment
    // could shift one without the other.
    use postmortem::{CARD_MAX_HEIGHT, CARD_MIN_HEIGHT, CARD_WIDTH};
    assert_eq!(live_detail::LiveDetailCard::WINDOW, postmortem::PostMortemCard::WINDOW);
    assert_eq!(CARD_WIDTH, 64);
    assert_eq!(CARD_MIN_HEIGHT, 8);
    assert_eq!(CARD_MAX_HEIGHT, 22);
}

#[test]
fn live_card_title_marks_kind_as_live() {
    // The card's title contains the workload's display name AND a
    // `(live)` marker so an operator who has both card kinds in
    // visual memory can tell at a glance which renderer is up. The
    // card is centered inside the frame, so its title row lands
    // somewhere above the middle — scan every row for the title
    // pattern rather than guessing the exact y.
    let theme = current_theme("dark");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let card = LiveDetailCard::new(fixture("phi3-mini"));

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 24);
            live_detail::render(f, area, &card, &theme, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut all = String::new();
    for y in 0..24 {
        for x in 0..80 {
            all.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        all.push('\n');
    }
    assert!(
        all.contains("phi3-mini"),
        "rendered card must surface display_name in title:\n{all}"
    );
    assert!(
        all.contains("(live)"),
        "title must mark live kind:\n{all}"
    );
}

#[test]
fn live_card_body_contains_pid_cpu_ram() {
    let theme = current_theme("dark");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let card = LiveDetailCard::new(fixture("phi3-mini"));

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 24);
            live_detail::render(f, area, &card, &theme, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut all = String::new();
    for y in 0..24 {
        for x in 0..80 {
            all.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        all.push('\n');
    }
    assert!(all.contains("PID:"), "expected PID label: {all}");
    assert!(all.contains("4242"), "expected pid value: {all}");
    assert!(all.contains("CPU:"), "expected CPU label: {all}");
    assert!(all.contains("RAM:"), "expected RAM label: {all}");
    assert!(
        all.contains("GPU memory:"),
        "expected GPU memory label when vram is present: {all}"
    );
    assert!(
        all.contains("Tokens/sec:"),
        "expected tokens/sec label when telemetry is present: {all}"
    );
}

#[test]
fn live_card_with_no_buffers_renders_collecting_rows() {
    // L17 / §5 — card open but no samples yet (first tick after
    // Enter). Each of the four metric rows renders a muted
    // `(collecting…)` placeholder so the height matches the
    // post-collection layout from the first frame.
    let theme = current_theme("dark");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let card = LiveDetailCard::new(fixture("phi3-mini"));

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 24);
            live_detail::render(f, area, &card, &theme, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut all = String::new();
    for y in 0..24 {
        for x in 0..80 {
            all.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        all.push('\n');
    }
    assert!(
        all.contains("(collecting…)"),
        "expected (collecting…) placeholder when buffers are None:\n{all}"
    );
    // Each metric label must still appear so the row scaffold is in
    // place — render path can't omit rows or the card height would
    // shift between the pre-data and post-data state.
    for label in ["CPU", "RAM", "VRAM", "Tokens/s"] {
        assert!(
            all.contains(label),
            "expected sparkline label {label} in collecting state:\n{all}"
        );
    }
    assert!(
        !all.contains("pending L17"),
        "pre-L17 placeholder must not leak into the themed render path:\n{all}"
    );
}

#[test]
fn live_card_with_filled_buffers_renders_sparkline_glyphs() {
    // L17 / §5 — once samples arrive, the `(collecting…)` text
    // disappears and the row carries block-character cells. Push
    // five known values into each buffer and assert at least one
    // block glyph (▁▂▃▄▅▆▇█) renders inside the card area.
    use edge_monitor::ui::panels::live_detail::LiveDetailBuffers;

    let theme = current_theme("dark");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let card = LiveDetailCard::new(fixture("phi3-mini"));
    let mut buffers = LiveDetailBuffers::new(4242);
    for v in [10.0, 30.0, 50.0, 70.0, 90.0] {
        buffers.cpu.push(v);
        buffers.ram_pct.push(v);
        buffers.vram_pct.push(v);
        buffers.tokens_per_sec.push(v);
    }

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 24);
            live_detail::render(f, area, &card, &theme, Some(&buffers));
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut all = String::new();
    for y in 0..24 {
        for x in 0..80 {
            all.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        all.push('\n');
    }
    assert!(
        !all.contains("(collecting…)"),
        "filled buffers must replace the collecting placeholder:\n{all}"
    );
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    assert!(
        blocks.iter().any(|c| all.contains(*c)),
        "expected at least one block-character glyph in rendered card:\n{all}"
    );
    // Trailing instantaneous-value column should show the most
    // recent sample (90.0) for the metrics with threshold range.
    assert!(
        all.contains("90.0%"),
        "expected most-recent percentage in trailing column:\n{all}"
    );
}

#[test]
fn live_card_sparkline_critical_cell_uses_theme_critical_fg() {
    // §14 + §5 — when the latest buffered CPU sample is ≥95%, the
    // corresponding sparkline cell must render in theme.critical
    // (not theme.foreground). This is the regression guard for
    // L17's threshold coloring path.
    use edge_monitor::ui::panels::live_detail::LiveDetailBuffers;
    use ratatui::style::Color;

    let theme = current_theme("dark");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let card = LiveDetailCard::new(fixture("phi3-mini"));
    let mut buffers = LiveDetailBuffers::new(4242);
    // Single high sample so we can find a single critical cell.
    buffers.cpu.push(97.0);
    buffers.ram_pct.push(10.0);
    buffers.vram_pct.push(10.0);
    buffers.tokens_per_sec.push(10.0);

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 24);
            live_detail::render(f, area, &card, &theme, Some(&buffers));
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut found_critical = false;
    for y in 0..24 {
        for x in 0..80 {
            let cell = buffer.cell((x, y)).expect("cell");
            let sym = cell.symbol();
            // A block glyph rendered in theme.critical proves the
            // threshold coloring fired. There's exactly one CPU
            // sample at 97% — finding the cell anywhere in the
            // card region is enough.
            if blocks.iter().any(|b| sym.starts_with(*b))
                && cell.style().fg == Some(theme.critical)
            {
                found_critical = true;
                break;
            }
        }
        if found_critical {
            break;
        }
    }
    assert!(
        found_critical,
        "expected at least one critical-colored sparkline cell for CPU=97%"
    );
    // Sanity check: the critical color we're matching is actually
    // the contract's dark-palette critical, not some accident from
    // an unrelated panel.
    assert_eq!(theme.critical, Color::Rgb(0xf7, 0x76, 0x8e));
}

#[test]
fn live_card_title_fg_tracks_theme_accent() {
    // L20 plumbing invariant: the card's title carries
    // `theme.accent` so theme switches are observable on the card
    // overlay just as they are on the mission line. Cell (1, ?)
    // — the title sits on row 0 of the card's rendered area, which
    // (because the card is centered inside the 80x24 frame) lands
    // somewhere in the middle. We scan row 0..N for the 'p' of the
    // workload name and assert the fg matches.
    let theme = current_theme("high-contrast");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let card = LiveDetailCard::new(fixture("phi3-mini"));

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 24);
            live_detail::render(f, area, &card, &theme, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    // Find the first cell whose symbol starts with 'p' (the 'p' of
    // 'phi3-mini') somewhere in the top half of the buffer.
    let mut hit = None;
    'outer: for y in 0..12 {
        for x in 0..80 {
            let cell = buffer.cell((x, y)).expect("cell");
            if cell.symbol() == "p" {
                hit = Some((x, y, cell.style().fg));
                break 'outer;
            }
        }
    }
    let (_, _, fg) = hit.expect("expected 'p' from 'phi3-mini' in title region");
    assert_eq!(
        fg,
        Some(Color::Rgb(0x00, 0xff, 0xff)),
        "high-contrast accent is #00ffff per ux_contract; title fg must match"
    );
}

#[test]
fn from_focused_returns_none_when_pid_not_in_state() {
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;

    let runtime = Runtime::new(Config::default());
    let detail = LiveDetail::from_focused(runtime.state(), 99_999);
    assert!(detail.is_none(), "unknown PID must not produce a snapshot");
}
