use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph};

use crate::platform::GpuDeviceMetrics;
use crate::runtime::RuntimeState;
use crate::thresholds::EffectiveThresholds;
use crate::ui::theme::UiTheme;
use ux_contract::host_vitals::ThermalZone;

use super::panel_block;

/// Number of hottest thermal zones to show inline. Sized to cover
/// every currently-known real-world host in full: x86 dev boxes
/// (3-5 zones typical), Jetson Orin AGX (~9 zones), most
/// workstations (2-6). Only truly-exotic hosts with more than 10
/// zones fall back to the "N of M zones shown" honest truncation.
///
/// Original CAR-22 cap was 3 — operator-visible on any host with
/// more than 3 zones (~40% of dev machines). Raised to 10 so the
/// common case (all realistic zone counts) shows every zone in
/// full; the truncation nag only appears when it's genuinely
/// warranted. Wide terminals (100+ cols) fit ~5 zones per line
/// comfortably; narrower terminals rely on Paragraph's default
/// truncation, which drops the coldest (least interesting) zones
/// off the right edge because the list is sorted hottest-first.
const TUI_TOP_THERMAL_ZONES: usize = 10;

/// v1.1.12 / DISPATCH 107 FIX 4 — vitals-row label column width.
/// Covers the longest tag ("Processes ", 10 chars) plus trailing
/// padding so numeric slots on every row (RAM / CPU load / VRAM /
/// GPU / Processes / Thermal) line up under one another. If a new
/// row is added with a longer label, bump this in lockstep with the
/// pinning test `label_width_fits_every_row_label`.
const LABEL_WIDTH: usize = 12;

/// v1.3.2 / DISPATCH 109 — build the GPU temp+power line for the
/// Vitals panel's row 3.
///
/// Aggregation invariants (pinned by tests):
/// * Temperature aggregates as MAX across all devices — the hottest
///   device drives the row, matching the thermal-panel convention.
/// * Power aggregates as SUM across all devices — total board draw.
///
/// Honesty invariants (the VRAM honesty rule extended to GPU
/// signals — pinned by tests):
/// * When NO device reports a temperature, the temp field renders
///   as `—`, NEVER `0°C`. A giant `0°C` on a kiosk would read as
///   "GPU is at freezing" instead of "we don't know."
/// * Same for power: no device reports → `—`, NEVER `0W`.
/// * A device with a genuine measured `0.0` value (rare — power
///   floor / off state) does render as `0°C` / `0W`, because that
///   IS a measurement.
///
/// Extracted from the `render` fn so the honesty rule can be
/// exercised without a `Frame`. The label prefix uses `LABEL_WIDTH`
/// so this row lines up with the other Vitals rows on the same
/// column grid.
fn format_gpu_vitals_line(devices: &[GpuDeviceMetrics]) -> String {
    let temp_c = devices
        .iter()
        .filter_map(|d| d.temp_c)
        .fold(None::<f32>, |acc, t| Some(acc.map_or(t, |a| a.max(t))));
    let (power_w_sum, any_power) = {
        let mut sum: f32 = 0.0;
        let mut any = false;
        for d in devices {
            if let Some(w) = d.power_watts {
                sum += w;
                any = true;
            }
        }
        (sum, any)
    };
    let temp_str = temp_c.map_or_else(|| "—".to_string(), |t| format!("{t:.0}°C"));
    let power_str = if any_power {
        format!("{power_w_sum:.0}W")
    } else {
        "—".to_string()
    };
    format!(
        "{:<width$}{} · {}",
        "GPU",
        temp_str,
        power_str,
        width = LABEL_WIDTH,
    )
}

/// v1.1.12 / CAR-22 — map a raw zone temperature to a TUI color.
/// Mirrors the web wire's `classify_thermal` semantics with the
/// SAME `ux_contract::thresholds` constants — no drift mode where
/// the TUI uses one cutoff and the web uses another. The wire and
/// the TUI each read the contract directly rather than sharing a
/// helper so neither side accidentally caches a stale classification.
fn thermal_color(theme: &UiTheme, temp_celsius: f32, thresholds: &EffectiveThresholds) -> Color {
    // v1.3.1 — read from the resolved EffectiveThresholds so an
    // operator's [thresholds] override reaches the TUI render path,
    // identical to the wire's `classify_thermal` and the runtime's
    // ThermalPressure observe call. No drift between the wire JSON
    // and the TUI banner.
    let c = f64::from(temp_celsius);
    if c >= thresholds.thermal_red_c {
        theme.critical
    } else if c >= thresholds.thermal_amber_c {
        theme.attention
    } else {
        theme.foreground
    }
}

/// Pick the `TUI_TOP_THERMAL_ZONES` hottest zones from the
/// snapshot, sorted descending by temperature. Stable on ties
/// (preserves the producer's label sort). Returns
/// `(top_N, total_zone_count)` so the renderer can show
/// "N of M zones shown" when there are more.
///
/// Public-in-module so the unit test can drive it directly without
/// instantiating a `Frame`.
fn top_hottest(zones: &[ThermalZone]) -> (Vec<&ThermalZone>, usize) {
    let total = zones.len();
    let mut by_temp: Vec<&ThermalZone> = zones.iter().collect();
    // `partial_cmp` is fine because thermal temps are never NaN in
    // practice (kernel returns integer millidegrees); on the off
    // chance one slips through, treat it as the coldest reading so
    // it sorts to the back of the top-N list.
    by_temp.sort_by(|a, b| {
        b.temp_celsius
            .partial_cmp(&a.temp_celsius)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    by_temp.truncate(TUI_TOP_THERMAL_ZONES);
    (by_temp, total)
}

/// v1.1.12 / CAR-22 — render the thermal summary into `area`.
/// Hidden when `zones` is empty (the contract semantic — empty
/// means "no zones discovered", so we suppress the section). When
/// `zones.len() <= TUI_TOP_THERMAL_ZONES` we show every zone with
/// no count line; otherwise we show the top-N and append
/// `"N of M zones shown"` so the operator knows there's more.
fn render_thermal_summary(
    f: &mut Frame,
    area: Rect,
    theme: &UiTheme,
    zones: &[ThermalZone],
    thresholds: &EffectiveThresholds,
) {
    if zones.is_empty() {
        return;
    }
    let (top, total) = top_hottest(zones);
    // Derive friendlies for the FULL zone list so positional
    // disambiguation (`System Zone 1` / `System Zone 2` for duplicate
    // `acpitz`) matches the wire's ordering exactly — the top-N slice
    // is then indexed against the full-list friendlies so no drift
    // between what the operator sees on TUI vs web.
    let all_raw: Vec<String> = zones.iter().map(|z| z.label.clone()).collect();
    let friendlies =
        crate::platform::humanize_thermal_labels(&all_raw);
    let inline: String = top
        .iter()
        .map(|z| {
            // Find this zone's original position in `zones` (linear
            // scan; `zones` is bounded by the number of thermal
            // zones on the host — typically 3-9, at most low-tens on
            // Jetson) to look up its friendly. Pointer identity
            // match: `top` holds `&ThermalZone` borrowed from
            // `zones`, so ptr equality is well-defined and cheap.
            let idx = zones
                .iter()
                .position(|z2| std::ptr::eq(z2, *z))
                .unwrap_or(0);
            let friendly = friendlies
                .get(idx)
                .cloned()
                .unwrap_or_else(|| z.label.clone());
            // Show friendly + raw label muted-shape (via ASCII
            // parens; per-span styling would require a `Line`
            // rewrite of the inline string — deferred to keep this
            // fix cosmetic-scope).
            format!("{}: {:.1}°C ({})", friendly, z.temp_celsius, z.label)
        })
        .collect::<Vec<_>>()
        .join("  ");
    // Color the LINE by the hottest zone — the operator's eye gets
    // dragged to the worst signal first. Per-zone color would
    // require building a `Line` with per-span styles, which is
    // workable but reads as visual noise alongside RAM / VRAM
    // gauges. Use the hottest-tier color as the dominant signal.
    let dominant = top
        .first()
        .map(|z| thermal_color(theme, z.temp_celsius, thresholds))
        .unwrap_or(theme.foreground);
    // v1.3.2 / DISPATCH 107 FIX 4 — align "Thermal" tag with the
    // 12-char label grid the rest of the vitals panel uses so the
    // thermal row sits under the same column axis as RAM/CPU/VRAM/
    // Processes. Shares the same `LABEL_WIDTH` module const so the
    // grid can never drift between the vitals rows and this thermal
    // header.
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(
            format!("{:<width$}", "Thermal", width = LABEL_WIDTH),
            Style::default().fg(theme.muted),
        ),
        Span::styled(inline, Style::default().fg(dominant)),
    ];
    if total > TUI_TOP_THERMAL_ZONES {
        spans.push(Span::styled(
            format!("    {} of {} zones shown", top.len(), total),
            Style::default().fg(theme.muted),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn render(f: &mut Frame, area: Rect, state: &RuntimeState, theme: &UiTheme) {
    let block = panel_block("Vitals", false, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(snap) = state.last_snapshot.as_ref() else {
        let p = Paragraph::new("waiting for first sample...")
            .style(Style::default().fg(theme.muted));
        f.render_widget(p, inner);
        return;
    };

    // v1.1.12 / CAR-22 — the thermal row is a `Constraint::Length(1)`
    // row, hidden (rendered as a no-op) when no zones are
    // discovered. The constraint stays the same so the layout is
    // identical whether thermal is hidden or shown — the alternative
    // (conditional row count) would make the panel resize between
    // ticks if thermal discovery raced the first sample.
    //
    // v1.3.2 / DISPATCH 109 — extended from 5 to 6 rows. New row 3
    // is the GPU temp+power tile, inserted between VRAM and
    // Processes so the two GPU-device signals sit adjacent. Same
    // "always allocate; hide-empty via a stable no-op" pattern as
    // the thermal row.
    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // [0] RAM gauge
            Constraint::Length(1), // [1] CPU load
            Constraint::Length(1), // [2] VRAM gauge / "No GPU"
            Constraint::Length(1), // [3] D109 — GPU temp+power line
            Constraint::Length(1), // [4] Processes
            Constraint::Length(1), // [5] Thermal (hidden when empty)
        ])
        .split(inner);

    // v1.3.2 / DISPATCH 107 FIX 4 — align every row to a common
    // left-margin label column (12-char wide "TAG:" prefix) so the
    // Vitals rows read as a coherent grid rather than freely-placed
    // widgets. Closes the BOARD_AUDIT §2.1 "no column grid /
    // stranded RAM" observation. Gauges keep their visual bar (they
    // fill the area to the right of the label); text rows get the
    // same label width so the values line up under one another.
    //
    // The 12-char label width covers the longest tag ("Processes ")
    // plus a trailing space. If a new row is added with a longer
    // tag, bump `LABEL_WIDTH` (module scope) — pinned by the
    // `label_width_fits_every_row_label` test.

    let mem_pct = snap.system.memory_usage_percent().clamp(0.0, 100.0);
    let mem_used_mb = snap.system.used_memory / (1024 * 1024);
    let mem_total_mb = snap.system.total_memory / (1024 * 1024);
    // L21 / §14 — bars stay on foreground until 85%, shift to
    // attention at 85% and critical at 95%. `theme.bar_color` is
    // the single source of truth for the threshold mapping.
    let mem_gauge = Gauge::default()
        .label(format!(
            "{:<width$}{}/{} MB",
            "RAM", mem_used_mb, mem_total_mb, width = LABEL_WIDTH,
        ))
        .gauge_style(Style::default().fg(theme.bar_color(mem_pct)))
        .ratio((mem_pct / 100.0).clamp(0.0, 1.0));
    f.render_widget(mem_gauge, cols[0]);

    let load_line = Paragraph::new(format!(
        "{:<width$}{:.2} {:.2} {:.2}    cpus: {}",
        "CPU load",
        snap.system.load_average[0],
        snap.system.load_average[1],
        snap.system.load_average[2],
        snap.system.cpu_count,
        width = LABEL_WIDTH,
    ))
    .style(Style::default().fg(theme.foreground));
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
                "{:<width$}{}/{} MB ({} devices)",
                "VRAM",
                used / (1024 * 1024),
                total / (1024 * 1024),
                snap.gpu.devices.len(),
                width = LABEL_WIDTH,
            ))
            .gauge_style(Style::default().fg(theme.bar_color(pct)))
            .ratio((pct / 100.0).clamp(0.0, 1.0));
        f.render_widget(gauge, cols[2]);
    } else {
        let p = Paragraph::new(format!(
            "{:<width$}No GPU detected",
            "VRAM",
            width = LABEL_WIDTH,
        ))
        .style(Style::default().fg(theme.muted));
        f.render_widget(p, cols[2]);
    }

    // v1.3.2 / DISPATCH 109 — GPU temp+power row. See
    // `format_gpu_vitals_line` for the aggregation + honesty rule.
    let gpu_line = if snap.gpu.has_gpu() {
        Paragraph::new(format_gpu_vitals_line(&snap.gpu.devices))
            .style(Style::default().fg(theme.foreground))
    } else {
        // No GPU → the row is empty. Same "hide via no-op paragraph"
        // pattern the thermal row uses when no zones are discovered.
        Paragraph::new("")
    };
    f.render_widget(gpu_line, cols[3]);

    let ai_count = state.ai_processes().count();
    let proc_line = Paragraph::new(format!(
        "{:<width$}{} total   {} AI workloads",
        "Processes",
        snap.processes.len(),
        ai_count,
        width = LABEL_WIDTH,
    ))
    .style(Style::default().fg(theme.foreground));
    f.render_widget(proc_line, cols[4]);

    // v1.1.12 / CAR-22 — thermal summary row (hidden when no zones).
    // AUTHORITY LOCK: this is display only. No alert fires from this
    // path; the renderer reads `snap.vitals.thermal_zones` (populated
    // by `platform::host_vitals::collect_host_vitals`) and shows
    // values + color. Alert firing on thermal is v1.2.0+ scope.
    // v1.3.2 / DISPATCH 109 — thermal row moved from cols[4] to
    // cols[5] to make room for the new GPU row at cols[3]. The
    // renderer's own hide-when-empty semantic is unchanged.
    render_thermal_summary(f, cols[5], theme, &snap.vitals.thermal_zones, &state.thresholds);
}

#[cfg(test)]
mod tests {
    use super::*;
    // v1.3.1 — top-of-file `use` for these moved out alongside the
    // resolved-threshold refactor; the boundary-case tests below
    // still pin against the contract defaults, so re-import here
    // for the cfg(test) block only.
    use ux_contract::thresholds::{THERMAL_AMBER_C, THERMAL_RED_C};

    fn z(label: &str, temp: f32) -> ThermalZone {
        ThermalZone {
            label: label.to_string(),
            temp_celsius: temp,
        }
    }

    /// `top_hottest` picks the N HIGHEST temperatures from a
    /// zone set exceeding the cap, and reports the total count so
    /// the renderer can show "N of M zones shown". Pinned because
    /// the descending-sort + truncate ordering is the property
    /// operators rely on to spot the worst thermal signal at a
    /// glance. Fixture size (12) is chosen to exceed the current
    /// cap so the truncation path is exercised — if the cap ever
    /// rises above 12, bump this fixture in lockstep.
    #[test]
    fn thermal_summary_top_hottest_truncates_and_reports_total() {
        let zones = vec![
            z("acpitz", 48.0),
            z("TCPU", 38.5),
            z("x86_pkg_temp", 71.2),
            z("nvme_composite", 52.0),
            z("iwlwifi_1", 41.0),
            z("pch_skylake", 47.0),
            z("gpu_therm", 65.0),
            z("cpu_therm_zone_a", 42.0),
            z("cpu_therm_zone_b", 43.0),
            z("chipset", 40.0),
            z("battery", 35.0),
            z("ambient", 22.0),
        ];
        let (top, total) = top_hottest(&zones);

        assert_eq!(total, 12, "total reports the full zone count");
        assert_eq!(
            top.len(),
            TUI_TOP_THERMAL_ZONES,
            "truncated to exactly TUI_TOP_THERMAL_ZONES entries",
        );
        // Hottest-first ordering: 71.2 (x86_pkg_temp) is the
        // hottest; 22.0 (ambient) is the coldest and should be
        // dropped by the truncate.
        assert_eq!(top[0].label, "x86_pkg_temp");
        assert!((top[0].temp_celsius - 71.2).abs() < 0.01);
        // The two coldest (battery @35, ambient @22) fall off the
        // top-N slice — verify neither appears.
        assert!(top.iter().all(|z| z.label != "ambient"));
        assert!(top.iter().all(|z| z.label != "battery"));
    }

    /// Common-case regression: a host with ≤ cap zones (e.g. the
    /// 4-zone x86 dev box that originally triggered this fix) must
    /// show every zone, no truncation, no "N of M shown" nag. This
    /// pins the operator-facing behavior of the raised cap.
    #[test]
    fn thermal_summary_common_case_four_zones_all_shown() {
        // Exactly the shape of the operator's dev box that surfaced
        // the "3 of 4 zones shown" bug pre-fix.
        let zones = vec![
            z("acpitz", 43.0),
            z("acpitz", 45.0),
            z("x86_pkg_temp", 40.0),
            z("iwlwifi_1", 30.0),
        ];
        let (top, total) = top_hottest(&zones);
        assert_eq!(total, 4);
        assert_eq!(
            top.len(),
            4,
            "4 zones ≤ cap ⇒ every zone shown, no truncation (the fix)",
        );
        // The renderer's `if total > TUI_TOP_THERMAL_ZONES` guard
        // means the "N of M" nag is suppressed in this case — pin
        // that here since it's the whole point of the fix.
        assert!(
            total <= TUI_TOP_THERMAL_ZONES,
            "common-case 4-zone host must be ≤ cap so the 'N of M' \
             message is hidden by the renderer's guard",
        );
    }

    /// When `zones.len() <= TUI_TOP_THERMAL_ZONES`, `top_hottest`
    /// returns every zone (no truncation) and `total == top.len()`
    /// so the renderer can suppress the "N of M shown" count line.
    #[test]
    fn thermal_summary_under_cap_returns_all_zones() {
        let zones = vec![z("acpitz", 48.0), z("x86_pkg_temp", 71.2)];
        let (top, total) = top_hottest(&zones);
        assert_eq!(total, 2);
        assert_eq!(top.len(), 2, "no truncation under the cap");
        assert_eq!(top[0].label, "x86_pkg_temp"); // still hottest-first
    }

    /// Color mapping uses the contract thresholds (85/95). Boundary
    /// semantics mirror the wire's `classify_thermal` and the
    /// contract's `reference_classification_uses_thresholds`. Drives
    /// the dark theme's color set (the colors themselves don't
    /// matter — what matters is that we map to `attention` at the
    /// amber threshold and `critical` at the red one).
    #[test]
    fn thermal_color_maps_to_contract_thresholds() {
        let theme = UiTheme::default();
        // Below amber → foreground.
        assert_eq!(thermal_color(&theme, 45.0, &EffectiveThresholds::default()), theme.foreground);
        assert_eq!(
            thermal_color(&theme, THERMAL_AMBER_C as f32 - 0.1, &EffectiveThresholds::default()),
            theme.foreground,
            "84.9 °C must color as foreground (just below amber)",
        );
        // Amber boundary: `>=` so 85.0 is attention.
        assert_eq!(
            thermal_color(&theme, THERMAL_AMBER_C as f32, &EffectiveThresholds::default()),
            theme.attention,
        );
        assert_eq!(
            thermal_color(&theme, THERMAL_RED_C as f32 - 0.1, &EffectiveThresholds::default()),
            theme.attention,
            "94.9 °C must color as attention (just below red)",
        );
        // Red boundary: `>=` so 95.0 is critical.
        assert_eq!(
            thermal_color(&theme, THERMAL_RED_C as f32, &EffectiveThresholds::default()),
            theme.critical,
        );
        assert_eq!(thermal_color(&theme, 105.0, &EffectiveThresholds::default()), theme.critical);
    }

    fn gpu_dev(id: u32, temp_c: Option<f32>, power_w: Option<f32>) -> GpuDeviceMetrics {
        GpuDeviceMetrics {
            device_id: id,
            device_name: format!("test-gpu-{id}"),
            total_vram: 0,
            used_vram: 0,
            free_vram: 0,
            per_process_vram: std::collections::HashMap::new(),
            power_watts: power_w,
            temp_c,
        }
    }

    /// DISPATCH 109 — the honesty invariant for the TUI GPU row:
    /// when the driver isn't loaded (device.temp_c and .power_watts
    /// both `None`), the row must render `—` for both fields, NEVER
    /// `0°C` / `0W`. A giant `0°C` on a wall-monitor kiosk reads as
    /// "GPU is at freezing" — a lie. This is the VRAM honesty rule
    /// applied to GPU temp+power (BOARD_AUDIT §2.1).
    #[test]
    fn gpu_line_unmeasured_renders_dash_not_zero() {
        let devices = vec![gpu_dev(0, None, None)];
        let line = format_gpu_vitals_line(&devices);
        assert!(
            line.contains("—"),
            "unmeasured GPU row must render `—` for both fields; got {line:?}",
        );
        assert!(
            !line.contains("0°C") && !line.contains("0W"),
            "unmeasured GPU row must NEVER show `0°C` / `0W` (lying-zero); got {line:?}",
        );
        // The label + separator " · " shape stays stable so the row
        // remains recognisable even fully unmeasured.
        assert!(line.starts_with("GPU"));
        assert!(line.contains(" · "));
    }

    /// DISPATCH 109 — measured single-device path renders both
    /// fields with `°C` / `W` suffix, integer precision. Sanity
    /// check on the format-string (a drift in `format!("{t:.0}°C")`
    /// or the surrounding template would fail here).
    #[test]
    fn gpu_line_single_device_measured_renders_both_fields() {
        let devices = vec![gpu_dev(0, Some(62.0), Some(45.0))];
        let line = format_gpu_vitals_line(&devices);
        assert!(line.contains("62°C"), "temp missing/mis-formatted; got {line:?}");
        assert!(line.contains("45W"), "power missing/mis-formatted; got {line:?}");
        assert!(line.contains(" · "), "combining separator missing; got {line:?}");
    }

    /// DISPATCH 109 — aggregation invariant. Two GPUs at (60°C, 40W)
    /// and (75°C, 55W) must render as 75°C (MAX; hottest device
    /// drives the thermal row) and 95W (SUM; total board draw). A
    /// regression that flips the aggregation to MIN or AVG would
    /// silently lie about the hottest device / total draw.
    #[test]
    fn gpu_line_two_devices_max_temp_sum_watts() {
        let devices = vec![
            gpu_dev(0, Some(60.0), Some(40.0)),
            gpu_dev(1, Some(75.0), Some(55.0)),
        ];
        let line = format_gpu_vitals_line(&devices);
        assert!(
            line.contains("75°C"),
            "temp must be MAX across devices (hottest drives the row); got {line:?}",
        );
        assert!(
            line.contains("95W"),
            "power must be SUM across devices (total draw); got {line:?}",
        );
    }

    /// DISPATCH 109 — asymmetric coverage: one device measured, one
    /// unmeasured. The measured device's values still surface (temp
    /// = MAX of what's present, power = SUM of what's present);
    /// unmeasured devices don't drag the aggregate to `0` or `—`
    /// (as they would under a naive `iter().min()/sum()` on an
    /// `Option::unwrap_or(0.0)` fold).
    #[test]
    fn gpu_line_partial_measurement_aggregates_over_measured_only() {
        let devices = vec![
            gpu_dev(0, Some(60.0), None),
            gpu_dev(1, None, Some(50.0)),
        ];
        let line = format_gpu_vitals_line(&devices);
        assert!(line.contains("60°C"), "measured temp must surface; got {line:?}");
        assert!(line.contains("50W"), "measured power must surface; got {line:?}");
    }

    /// DISPATCH 109 — measured genuine `0.0` renders as `0°C` /
    /// `0W`, NOT `—`. This is the flip side of the honesty rule:
    /// a real "device idle at 0 W" IS a measurement and must be
    /// distinguishable from "no measurement." The tests above pin
    /// `None → —`; this test pins `Some(0.0) → 0°C / 0W`.
    #[test]
    fn gpu_line_measured_zero_serializes_as_zero_not_dash() {
        let devices = vec![gpu_dev(0, Some(0.0), Some(0.0))];
        let line = format_gpu_vitals_line(&devices);
        assert!(
            line.contains("0°C"),
            "Some(0.0) is a valid measurement and must render as `0°C`, not `—`; got {line:?}",
        );
        assert!(
            line.contains("0W"),
            "Some(0.0) is a valid measurement and must render as `0W`, not `—`; got {line:?}",
        );
    }

    /// DISPATCH 107 FIX 4 — LABEL_WIDTH pins the vitals-panel column
    /// grid: every row's numeric slot sits at the SAME horizontal
    /// offset because every row's label is left-padded to
    /// `LABEL_WIDTH` chars. If a new row is added with a label
    /// longer than the width (or if `LABEL_WIDTH` is shrunk below
    /// the longest label + one space of padding), the grid breaks
    /// silently at render — the RAM row shows aligned numbers but
    /// the new row's numbers hang off to the right. This test fails
    /// early, before the next release binary ships mis-aligned.
    #[test]
    fn label_width_fits_every_row_label() {
        // Every label rendered in `render_vitals_panel` — kept in
        // lockstep with the row `format!` calls above. "Processes"
        // is the longest at 9 chars; LABEL_WIDTH must leave at
        // least one trailing space for readability.
        let labels = ["RAM", "CPU load", "VRAM", "GPU", "Processes", "Thermal"];
        let longest = labels.iter().map(|l| l.len()).max().unwrap();
        assert!(
            LABEL_WIDTH > longest,
            "LABEL_WIDTH ({LABEL_WIDTH}) must be > longest label ({longest}) so every row has at least one space of padding before its value; adjust in lockstep if a new row is added",
        );
    }
}
