use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use crate::runtime::RuntimeState;

use super::super::app::App;
use super::panel_block;

/// Tier 3.3 — KV-cache pressure threshold. At or above this, the
/// registry row colours the `KV ..%` segment red. Below, it stays
/// cyan with the rest of the line. The number is the same warning
/// line vLLM operators draw at, and it's what `latest.md` 3.3 calls
/// out explicitly.
const KV_HOT_PCT: f32 = 80.0;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    // L2b — focus cycling is gone (panels/mod.rs no longer renders
    // multiple list panels); the registry is always the focused
    // selection target in v0.3 §1, so `focused = true` is locked here.
    let focused = true;
    let visible_pids = app.visible(state);

    let items: Vec<ListItem> = visible_pids
        .iter()
        .filter_map(|pid| state.annotated.iter().find(|p| p.pid == *pid))
        .map(|p| {
            // Show model when extracted; leave the column blank rather than
            // a dash when nothing's resolved — a dash invites the reader to
            // think there is a special "unknown" model called "-".
            let model = p.model_name.as_deref().unwrap_or("");
            // Hide GPU memory when the host has no GPU or the process has
            // no GPU allocation — printing "0M" implies a measurement we
            // didn't actually take. An empty column communicates absence.
            let vram = match p.vram_bytes {
                Some(b) if b > 0 => format!("{:>4}M", b / (1024 * 1024)),
                _ => "    ".into(),
            };
            let head = format!(
                "{:>6} {:<9?} {:>5.1}% {:>5}M {} {:<18} {}",
                p.pid,
                p.category,
                p.cpu_pct,
                p.rss_mb,
                vram,
                truncate(&p.name, 18),
                model,
            );
            let cyan = Style::default().fg(Color::Cyan);
            let mut spans: Vec<Span> = vec![Span::styled(head, cyan)];

            // Tier 3.3 — append `  KV NN%` when the dispatcher has a
            // reading. Red when >=80%, cyan otherwise. Skipped entirely
            // for processes with no KV reading so non-LLM workloads
            // don't carry a misleading "KV -%" column.
            if let Some(live) = state.live_telemetry.get(&p.pid)
                && let Some(kv) = live.kv_cache_peak_pct
            {
                let kv_text = format!("  KV {:>4.0}%", kv);
                let style = if kv >= KV_HOT_PCT {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    cyan
                };
                spans.push(Span::styled(kv_text, style));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = panel_block("AI Workloads", focused);

    // DESIGN_HANDOFF Principle 6 — empty states teach the product.
    // When no AI workloads are running we used to render an empty
    // List inside the panel border, leaving a confused user staring
    // at a blank box. A short Paragraph with concrete launch
    // examples turns the void into a "press here next" moment. We
    // distinguish two cases: the registry is genuinely empty
    // (`state.ai_processes()` empty), and a filter has just hidden
    // every row (state has AI procs but `visible_pids` is empty —
    // the filter substring didn't match). The latter teaches a
    // different lesson: "your filter excluded everything, press Esc
    // or `/` to clear."
    if items.is_empty() {
        let any_ai_at_all = state.ai_processes().next().is_some();
        let lines: Vec<Line> = if any_ai_at_all {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No workloads match the current filter.",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::raw(
                    "  Press Esc to clear the filter, or `/` to edit it.",
                )),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No AI workloads detected yet.",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::raw(
                    "  Try one of these in another terminal — edge_monitor",
                )),
                Line::from(Span::raw(
                    "  will detect the workload on the next tick:",
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "      ollama run llama3 'hello'",
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(Span::styled(
                    "      vllm serve <model>",
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(Span::styled(
                    "      yolo predict model=yolov8n.pt source=...",
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(""),
                Line::from(Span::raw("  Or wrap an existing command:")),
                Line::from(Span::styled(
                    "      edge_monitor exec -- <your command>",
                    Style::default().fg(Color::Cyan),
                )),
            ]
        };
        let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    if focused && !visible_pids.is_empty() {
        list_state.select(Some(app.selected_index().min(visible_pids.len() - 1)));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
