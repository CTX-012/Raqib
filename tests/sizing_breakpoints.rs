//! L22 / UX_CONTRACT.md §12 — terminal-size breakpoint behavior.
//!
//! Pins the §12 layout gate at the four spec-defined sample sizes:
//!
//! | Size  | Tier      | Expected behavior                                  |
//! |-------|-----------|----------------------------------------------------|
//! | 70×20 | TooSmall  | `errors::TERMINAL_TOO_SMALL` rendered, no panels   |
//! | 80×24 | Narrow    | Top processes panel hidden; Workloads + Activity   |
//! | 120×40| Standard  | Full §1 layout (Workloads + Top + Activity)        |
//! | 160×50| Wide      | At 4+ workloads, Workloads renders in two columns  |
//!
//! Asserts on the rendered buffer text (Frame → TestBackend → Buffer
//! → flattened string). Panel-internal sizing (bar graph cell counts,
//! Activity 3-row cap, sparkline cells) is **not** asserted here — it
//! belongs to a follow-up that L21's panel audit may absorb.

use std::time::Instant;

use edge_monitor::model::{AICategory, WorkloadCategory};
use edge_monitor::runtime::{AnnotatedProcess, RuntimeState};
use edge_monitor::ui::SizeTier;
use edge_monitor::ui::app::App;
use edge_monitor::ui::panels;
use edge_monitor::ui::theme::UiTheme;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

fn ai_process(pid: u32, name: &str, category: WorkloadCategory) -> AnnotatedProcess {
    AnnotatedProcess {
        pid,
        name: name.into(),
        category: AICategory::Inference,
        workload_category: category,
        evidence: String::new(),
        model_name: None,
        cpu_pct: 0.0,
        rss_mb: 0,
        vram_bytes: None,
        first_observed_at: Instant::now(),
            probe_endpoint: None,
    }
}

fn state_with_workloads(count: usize) -> RuntimeState {
    let mut state = RuntimeState::default();
    for i in 0..count {
        // Alternate categories so the two-col partition at Wide tier
        // doesn't collapse to a single group on one side.
        let category = if i % 2 == 0 {
            WorkloadCategory::LLM
        } else {
            WorkloadCategory::Vision
        };
        state
            .annotated
            .push(ai_process((i as u32) + 1, &format!("wl-{i}"), category));
    }
    state
}

fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area;
    let mut s = String::with_capacity(((area.width + 1) * area.height) as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(""));
        }
        s.push('\n');
    }
    s
}

fn render_at(width: u16, height: u16, state: &RuntimeState, app: &App) -> String {
    // L22 merge — post-Linux-sweep-1, `panels::render` takes the
    // L21 theme + L16/L17 live-detail card/buffer slots alongside
    // the original (state, app). Pin to the default dark theme and
    // no live-detail card; the breakpoint tests only assert on
    // layout shape, not theme colors.
    let theme = UiTheme::default();
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend init");
    terminal
        .draw(|f| panels::render(f, state, app, &theme, None, None))
        .expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

#[test]
fn at_70x20_tier_is_too_small_and_buffer_shows_contract_template() {
    assert_eq!(SizeTier::classify(70, 20), SizeTier::TooSmall);

    let state = state_with_workloads(2);
    let app = App::default();
    let buf = render_at(70, 20, &state, &app);

    // §12: "render `ERR_TERMINAL_TOO_SMALL` message and wait for
    // resize." The contract template carries the literal text we
    // expect to find on screen.
    assert!(
        buf.contains("edge_monitor needs at least 80"),
        "expected TERMINAL_TOO_SMALL template literal in buffer, got:\n{buf}"
    );
    assert!(
        buf.contains("Current size: 70"),
        "expected current-size substitution with width=70, got:\n{buf}"
    );
    // No panel borders should render under TooSmall — the contract
    // forbids a degraded view.
    assert!(
        !buf.contains("AI Workloads"),
        "AI Workloads panel must NOT render at TooSmall, got:\n{buf}"
    );
}

#[test]
fn at_80x24_tier_is_narrow_and_top_processes_panel_is_hidden() {
    assert_eq!(SizeTier::classify(80, 24), SizeTier::Narrow);

    let state = state_with_workloads(2);
    let app = App::default();
    let buf = render_at(80, 24, &state, &app);

    // Workloads panel still renders (§12: "Workloads panel is
    // sacred — never drops").
    assert!(
        buf.contains("AI Workloads"),
        "AI Workloads panel must render at Narrow tier, got:\n{buf}"
    );
    // Activity panel still renders.
    assert!(
        buf.contains("Activity"),
        "Activity panel must render at Narrow tier, got:\n{buf}"
    );
    // Top processes panel is the first to drop on narrow screens
    // per §12.
    assert!(
        !buf.contains("Top processes"),
        "Top processes panel must be hidden at Narrow tier, got:\n{buf}"
    );
}

#[test]
fn at_120x40_tier_is_standard_and_all_default_panels_render() {
    assert_eq!(SizeTier::classify(120, 40), SizeTier::Standard);

    let state = state_with_workloads(2);
    let app = App::default();
    let buf = render_at(120, 40, &state, &app);

    assert!(
        buf.contains("AI Workloads"),
        "AI Workloads panel must render at Standard tier"
    );
    assert!(
        buf.contains("Top processes"),
        "Top processes panel must render at Standard tier"
    );
    assert!(
        buf.contains("Activity"),
        "Activity panel must render at Standard tier"
    );
}

#[test]
fn at_160x50_tier_is_wide_and_renders_without_panic() {
    assert_eq!(SizeTier::classify(160, 50), SizeTier::Wide);

    // v1.3.2 / DISPATCH 107 FIX 1 — the pre-D107 test expected
    // ≥2 occurrences of "AI Workloads" because `render_workloads_two_col`
    // split the workloads area into two panels at Wide tier + 4+
    // workloads, each rendering its own titled block. FIX 1 removed
    // that duplication (BOARD_AUDIT §2.2 "duplicate 'AI Workloads'
    // panel" closed): Wide tier now renders a SINGLE panel titled
    // once, matching the operator's mental model. This test flipped
    // in D107 to pin the single-render property.
    let state = state_with_workloads(4);
    let app = App::default();
    let buf = render_at(160, 50, &state, &app);

    let title_occurrences = buf.matches("AI Workloads").count();
    assert_eq!(
        title_occurrences, 1,
        "expected the AI Workloads panel title to appear EXACTLY ONCE \
         at Wide tier with 4+ workloads (post-D107 single-column render); \
         got {title_occurrences}:\n{buf}"
    );
    // The §1 region map below the workloads area is unchanged from
    // Standard tier.
    assert!(buf.contains("Top processes"));
    assert!(buf.contains("Activity"));
}

#[test]
fn at_160x50_with_fewer_than_four_workloads_renders_single_column_workloads() {
    // §12: "Workloads may show two columns side-by-side when 4+
    // workloads." Below that threshold the Wide tier behaves like
    // Standard for the workloads panel.
    let state = state_with_workloads(3);
    let app = App::default();
    let buf = render_at(160, 50, &state, &app);

    let title_occurrences = buf.matches("AI Workloads").count();
    assert_eq!(
        title_occurrences, 1,
        "expected exactly one AI Workloads title at Wide tier with \
         <4 workloads, got {title_occurrences}:\n{buf}"
    );
}
