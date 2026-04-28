use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use crate::runtime::RuntimeState;

use super::super::app::{App, FocusedPanel};
use super::panel_block;

/// Tier 3.3 — KV-cache pressure threshold. At or above this, the
/// registry row colours the `KV ..%` segment red. Below, it stays
/// cyan with the rest of the line. The number is the same warning
/// line vLLM operators draw at, and it's what `latest.md` 3.3 calls
/// out explicitly.
const KV_HOT_PCT: f32 = 80.0;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, app: &App) {
    let focused = app.focus() == FocusedPanel::Registry;
    let visible_pids = if focused {
        app.visible(state)
    } else {
        // When unfocused, still list AI procs so the user sees them; just no cursor.
        state.ai_processes().map(|p| p.pid).collect::<Vec<_>>()
    };

    let items: Vec<ListItem> = visible_pids
        .iter()
        .filter_map(|pid| state.annotated.iter().find(|p| p.pid == *pid))
        .map(|p| {
            // Prefer the extracted model name when present; fall back to "-"
            // so rogue-style matches (keyword / script-sniff) still render in
            // the same column layout.
            let model = p.model_name.as_deref().unwrap_or("-");
            let vram = p
                .vram_bytes
                .map(|b| format!("{:>4}M", b / (1024 * 1024)))
                .unwrap_or_else(|| "   -".into());
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
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD)
                } else {
                    cyan
                };
                spans.push(Span::styled(kv_text, style));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = panel_block("Registry (AI workloads)", focused);

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
