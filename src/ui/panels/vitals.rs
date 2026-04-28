use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Gauge, Paragraph};

use crate::runtime::RuntimeState;

use super::panel_block;

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState) {
    let block = panel_block("Vitals", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(snap) = state.last_snapshot.as_ref() else {
        let p = Paragraph::new("waiting for first sample...");
        f.render_widget(p, inner);
        return;
    };

    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let mem_pct = snap.system.memory_usage_percent().clamp(0.0, 100.0);
    let mem_used_mb = snap.system.used_memory / (1024 * 1024);
    let mem_total_mb = snap.system.total_memory / (1024 * 1024);
    let mem_gauge = Gauge::default()
        .label(format!("RAM {}/{} MB", mem_used_mb, mem_total_mb))
        .gauge_style(gauge_color(mem_pct))
        .ratio((mem_pct / 100.0).clamp(0.0, 1.0));
    f.render_widget(mem_gauge, cols[0]);

    let load_line = Paragraph::new(format!(
        "load avg: {:.2} {:.2} {:.2}    cpus: {}",
        snap.system.load_average[0],
        snap.system.load_average[1],
        snap.system.load_average[2],
        snap.system.cpu_count
    ));
    f.render_widget(load_line, cols[1]);

    if snap.gpu.has_gpu() {
        let total = snap.gpu.total_vram_all_devices();
        let used = snap.gpu.used_vram_all_devices();
        let pct = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        let gauge = Gauge::default()
            .label(format!(
                "VRAM {}/{} MB ({} devices)",
                used / (1024 * 1024),
                total / (1024 * 1024),
                snap.gpu.devices.len()
            ))
            .gauge_style(gauge_color(pct))
            .ratio((pct / 100.0).clamp(0.0, 1.0));
        f.render_widget(gauge, cols[2]);
    } else {
        let p = Paragraph::new("GPU: not available (NVML uninitialized)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, cols[2]);
    }

    let proc_line = Paragraph::new(format!(
        "processes: {}   AI-classified: {}",
        snap.processes.len(),
        state.ai_processes().count()
    ));
    f.render_widget(proc_line, cols[3]);
}

fn gauge_color(pct: f64) -> Style {
    let color = if pct >= 90.0 {
        Color::Red
    } else if pct >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    };
    Style::default().fg(color)
}
