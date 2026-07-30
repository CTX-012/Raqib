//! Prometheus exporter (latest.md Tier 2.3).
//!
//! Exposes raqib's own metrics over HTTP for fleet operators who
//! already run Prometheus + Grafana. The exporter does NOT pull in
//! the `prometheus` crate — the text exposition format is small and
//! the dep would dwarf our hand-rolled equivalent. Instead we render
//! to a `String` and serve it from a tokio TCP listener.
//!
//! ## Metric-name prefix (`edge_monitor_*`) — DELIBERATELY KEPT
//!
//! Metric identifiers below (`edge_monitor_processes_total`,
//! `edge_monitor_gpu_watts`, `edge_monitor_governor_kills_total`, …)
//! are `edge_monitor_*` — NOT `raqib_*`. They pre-date the raqib
//! rename and act as an EXTERNAL CONTRACT: external Prometheus
//! scrape configs, Grafana dashboards, and alerting rules read the
//! metric names literally. Renaming them without coordination would
//! blank dashboards and stop alerts firing until every downstream
//! config was updated in lockstep — precisely the kind of
//! breaking-change-in-behavior the rename dispatch's HARD RULE 2
//! forbids ("NO behavior change").
//!
//! The internal-vs-external asymmetry (binary is `raqib`, metrics
//! stay `edge_monitor_*`) is honest and documented. A future
//! coordinated release may migrate the prefix via a dual-emit
//! deprecation window — until then, do NOT rename these identifiers.
//! Pinned by convention; a future clean-up sweep that "fixes" the
//! prefix would break every downstream monitoring config in the
//! field.
//!
//! Endpoint: `GET /metrics` returns 200 text/plain with the Prom body.
//! Anything else returns 404. No keep-alive, no compression, no
//! authentication — bind to loopback by default and put a reverse
//! proxy in front if you need any of those.
//!
//! Shape:
//! ```text
//! # HELP edge_monitor_processes_total Live processes by category.
//! # TYPE edge_monitor_processes_total gauge
//! edge_monitor_processes_total{category="inference"} 3
//! edge_monitor_processes_total{category="training"} 0
//! edge_monitor_run_tokens_per_sec{model="phi3-mini",pid="12345"} 37.4
//! edge_monitor_run_vram_bytes{model="phi3-mini",pid="12345"} 4101296128
//! edge_monitor_run_gpu_watts{model="phi3-mini",pid="12345"} 142.3
//! edge_monitor_governor_kills_total{reason="sigterm"} 12
//! edge_monitor_regressions_total{model="phi3-mini",metric="tokens_per_sec_avg"} 1
//! edge_monitor_tick_count 4321
//! ```
//!
//! Runtime updates [`MetricsSnapshot`] in place after every tick. The
//! exporter task reads the latest snapshot under a mutex per request.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// One AI process's per-PID gauges.
#[derive(Debug, Clone, Default)]
pub struct LiveAiSample {
    pub pid: u32,
    pub model: String,
    pub category: String,
    pub tokens_per_sec: Option<f32>,
    pub fps: Option<f32>,
    pub vram_bytes: Option<u64>,
    pub gpu_watts: Option<f32>,
    pub cpu_watts: Option<f32>,
    /// GPU die temperature attributed to this PID (°C). NVML reports
    /// temperature per device, not per process; this is the temp of
    /// the device that holds this PID's VRAM. `None` when no GPU.
    pub gpu_temp_celsius: Option<f32>,
}

/// Everything the exporter knows about the world. Runtime overwrites
/// it after each tick; exporter reads it on each scrape.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub tick_count: u64,
    /// `category_label -> count`. Populated for every category seen
    /// at least once so Prometheus has stable label series.
    pub processes_by_category: HashMap<String, u32>,
    /// Sum of every category bucket — exposed as a top-level gauge so
    /// dashboards can alert on "any AI process active" without summing.
    pub ai_processes_active: u32,
    pub live: Vec<LiveAiSample>,
    /// `reason -> count` — cumulative kill audit since process start.
    pub kills_by_reason: HashMap<String, u64>,
    /// `(model, metric) -> count` — cumulative regressions emitted.
    pub regressions: HashMap<(String, String), u64>,
    /// `model -> last cold-load duration (seconds)`. Populated on the
    /// tick the cold-load detector finalises a stats record. Stays at
    /// the most recent value until overwritten.
    pub cold_load_seconds: HashMap<String, f32>,
}

impl MetricsSnapshot {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Shared snapshot handle — the runtime writes, the exporter reads.
pub type SnapshotHandle = Arc<Mutex<MetricsSnapshot>>;

/// Hand-rolled Prom text-exposition rendering. Pure (no I/O), so the
/// unit test can exercise it against canned snapshots.
pub fn render(snap: &MetricsSnapshot) -> String {
    let mut out = String::with_capacity(1024);

    out.push_str("# HELP edge_monitor_processes_total Live AI processes by classifier category.\n");
    out.push_str("# TYPE edge_monitor_processes_total gauge\n");
    if snap.processes_by_category.is_empty() {
        // Always emit at least one zero series so scrapers see the
        // metric exists even on idle boxes.
        out.push_str("edge_monitor_processes_total{category=\"none\"} 0\n");
    } else {
        // Sort for deterministic output (golden tests, Grafana cache).
        let mut cats: Vec<_> = snap.processes_by_category.iter().collect();
        cats.sort_by_key(|(k, _)| k.as_str());
        for (cat, count) in cats {
            let _ = writeln!(
                out,
                "edge_monitor_processes_total{{category=\"{}\"}} {}",
                escape_label(cat),
                count
            );
        }
    }

    out.push_str("# HELP edge_monitor_run_tokens_per_sec Per-process token throughput.\n");
    out.push_str("# TYPE edge_monitor_run_tokens_per_sec gauge\n");
    let mut live = snap.live.clone();
    live.sort_by_key(|s| s.pid);
    for s in &live {
        if let Some(v) = s.tokens_per_sec {
            let _ = writeln!(
                out,
                "edge_monitor_run_tokens_per_sec{{model=\"{}\",pid=\"{}\"}} {}",
                escape_label(&s.model),
                s.pid,
                fmt_f32(v)
            );
        }
    }

    out.push_str("# HELP edge_monitor_run_fps Per-process inference frames per second.\n");
    out.push_str("# TYPE edge_monitor_run_fps gauge\n");
    for s in &live {
        if let Some(v) = s.fps {
            let _ = writeln!(
                out,
                "edge_monitor_run_fps{{model=\"{}\",pid=\"{}\"}} {}",
                escape_label(&s.model),
                s.pid,
                fmt_f32(v)
            );
        }
    }

    out.push_str("# HELP edge_monitor_run_vram_bytes Per-process VRAM footprint.\n");
    out.push_str("# TYPE edge_monitor_run_vram_bytes gauge\n");
    for s in &live {
        if let Some(v) = s.vram_bytes {
            let _ = writeln!(
                out,
                "edge_monitor_run_vram_bytes{{model=\"{}\",pid=\"{}\"}} {}",
                escape_label(&s.model),
                s.pid,
                v
            );
        }
    }

    out.push_str("# HELP edge_monitor_run_gpu_watts Per-process GPU board power (W).\n");
    out.push_str("# TYPE edge_monitor_run_gpu_watts gauge\n");
    for s in &live {
        if let Some(v) = s.gpu_watts {
            let _ = writeln!(
                out,
                "edge_monitor_run_gpu_watts{{model=\"{}\",pid=\"{}\"}} {}",
                escape_label(&s.model),
                s.pid,
                fmt_f32(v)
            );
        }
    }

    out.push_str("# HELP edge_monitor_run_cpu_watts Per-process CPU package power (W).\n");
    out.push_str("# TYPE edge_monitor_run_cpu_watts gauge\n");
    for s in &live {
        if let Some(v) = s.cpu_watts {
            let _ = writeln!(
                out,
                "edge_monitor_run_cpu_watts{{model=\"{}\",pid=\"{}\"}} {}",
                escape_label(&s.model),
                s.pid,
                fmt_f32(v)
            );
        }
    }

    // Per-PID GPU board power, exposed without the model label so
    // operators can write rules that fire purely off pid (matches the
    // Tier 2.3 metric list in the test report). Duplicates run_gpu_watts
    // intentionally: the run_* family carries model context for joined
    // queries, the gpu_* family is the simpler pid-only view.
    out.push_str("# HELP edge_monitor_gpu_watts Per-PID GPU board power (W).\n");
    out.push_str("# TYPE edge_monitor_gpu_watts gauge\n");
    for s in &live {
        if let Some(v) = s.gpu_watts {
            let _ = writeln!(
                out,
                "edge_monitor_gpu_watts{{pid=\"{}\"}} {}",
                s.pid,
                fmt_f32(v)
            );
        }
    }

    out.push_str(
        "# HELP edge_monitor_gpu_temp_celsius GPU die temperature attributed to PID (°C).\n",
    );
    out.push_str("# TYPE edge_monitor_gpu_temp_celsius gauge\n");
    for s in &live {
        if let Some(v) = s.gpu_temp_celsius {
            let _ = writeln!(
                out,
                "edge_monitor_gpu_temp_celsius{{pid=\"{}\"}} {}",
                s.pid,
                fmt_f32(v)
            );
        }
    }

    out.push_str(
        "# HELP edge_monitor_cold_load_seconds Wall-clock duration of the model cold-load phase.\n",
    );
    out.push_str("# TYPE edge_monitor_cold_load_seconds gauge\n");
    let mut cold: Vec<_> = snap.cold_load_seconds.iter().collect();
    cold.sort_by_key(|(m, _)| m.as_str());
    for (model, secs) in cold {
        let _ = writeln!(
            out,
            "edge_monitor_cold_load_seconds{{model=\"{}\"}} {}",
            escape_label(model),
            fmt_f32(*secs)
        );
    }

    out.push_str(
        "# HELP edge_monitor_ai_processes_active Total count of live AI-classified processes.\n",
    );
    out.push_str("# TYPE edge_monitor_ai_processes_active gauge\n");
    let _ = writeln!(
        out,
        "edge_monitor_ai_processes_active {}",
        snap.ai_processes_active
    );

    out.push_str("# HELP edge_monitor_governor_kills_total Cumulative kill decisions by reason.\n");
    out.push_str("# TYPE edge_monitor_governor_kills_total counter\n");
    let mut kills: Vec<_> = snap.kills_by_reason.iter().collect();
    kills.sort_by_key(|(k, _)| k.as_str());
    for (reason, count) in kills {
        let _ = writeln!(
            out,
            "edge_monitor_governor_kills_total{{reason=\"{}\"}} {}",
            escape_label(reason),
            count
        );
    }

    out.push_str(
        "# HELP edge_monitor_regressions_total Cumulative regression alerts by (model, metric).\n",
    );
    out.push_str("# TYPE edge_monitor_regressions_total counter\n");
    let mut regs: Vec<_> = snap.regressions.iter().collect();
    regs.sort_by(|a, b| a.0.cmp(b.0));
    for ((model, metric), count) in regs {
        let _ = writeln!(
            out,
            "edge_monitor_regressions_total{{model=\"{}\",metric=\"{}\"}} {}",
            escape_label(model),
            escape_label(metric),
            count
        );
    }

    // Counter: tick cycles since process start. `_total` suffix matches
    // Prometheus convention so promtool/Grafana auto-detect it as a
    // counter. The unsuffixed `edge_monitor_tick_count` was renamed for
    // Tier 2.3 — promtool flagged the missing suffix.
    out.push_str("# HELP edge_monitor_tick_count_total Number of completed tick cycles.\n");
    out.push_str("# TYPE edge_monitor_tick_count_total counter\n");
    let _ = writeln!(out, "edge_monitor_tick_count_total {}", snap.tick_count);

    out
}

/// Backslash-escape `\\`, `\"`, and `\n` per Prom exposition rules.
/// Defensive against model names like `..\..\windows\system32` even
/// though we don't expect them on a Linux box.
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Prom rendering for a finite f32. NaN/Inf → "0" (we drop them
/// upstream too but a defensive emit keeps the output parseable).
fn fmt_f32(v: f32) -> String {
    if v.is_finite() {
        format!("{:.4}", v)
    } else {
        "0".into()
    }
}

/// Spawn the exporter task on `runtime`. Returns a `JoinHandle` the
/// caller can keep around for shutdown; if `bind` is empty the task
/// is not spawned and `None` is returned.
pub fn spawn(
    runtime: &tokio::runtime::Runtime,
    bind: &str,
    snapshot: SnapshotHandle,
) -> std::io::Result<Option<tokio::task::JoinHandle<()>>> {
    if bind.is_empty() {
        return Ok(None);
    }
    let addr = SocketAddr::from_str(bind).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid prometheus_bind {:?}: {}", bind, e),
        )
    })?;
    let handle = runtime.spawn(async move {
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "prometheus exporter listening");
                serve_loop(listener, snapshot).await;
            }
            Err(e) => {
                tracing::error!(%addr, error = %e, "prometheus exporter failed to bind");
            }
        }
    });
    Ok(Some(handle))
}

async fn serve_loop(listener: TcpListener, snapshot: SnapshotHandle) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let snap = snapshot.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_one(stream, snap).await {
                        tracing::debug!(error = %e, "prometheus exporter connection error");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "prometheus exporter accept failed");
                // Brief back-off so we don't spin on a permanent error.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_one(
    mut stream: tokio::net::TcpStream,
    snapshot: SnapshotHandle,
) -> std::io::Result<()> {
    // Tiny request reader: enough to find the request line, ignore
    // the rest of the headers. Cap at 8 KiB so a malicious peer
    // can't make us buffer forever (TEST.md X.3.4 / X.3.5).
    let mut buf = [0u8; 8192];
    let mut total = 0;
    while total < buf.len() {
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read(&mut buf[total..]),
        )
        .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                total += n;
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                // Read timeout — slowloris-style. Bail.
                return Ok(());
            }
        }
    }

    let req = String::from_utf8_lossy(&buf[..total]);
    let path = req
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1));
    let body = match path {
        Some("/metrics") => {
            let snap = snapshot.lock().await.clone();
            render(&snap)
        }
        _ => {
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    };

    let resp = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; version=0.0.4\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_snapshot() -> MetricsSnapshot {
        let mut snap = MetricsSnapshot::new();
        snap.tick_count = 4321;
        snap.processes_by_category.insert("inference".into(), 3);
        snap.processes_by_category.insert("training".into(), 0);
        snap.ai_processes_active = 3;
        snap.live.push(LiveAiSample {
            pid: 12345,
            model: "phi3-mini".into(),
            category: "inference".into(),
            tokens_per_sec: Some(37.4),
            fps: None,
            vram_bytes: Some(4_101_296_128),
            gpu_watts: Some(142.3),
            cpu_watts: Some(35.0),
            gpu_temp_celsius: Some(67.0),
        });
        snap.kills_by_reason.insert("sigterm".into(), 12);
        snap.regressions
            .insert(("phi3-mini".into(), "tokens_per_sec_avg".into()), 1);
        snap.cold_load_seconds.insert("phi3-mini".into(), 1.75);
        snap
    }

    #[test]
    fn render_emits_expected_families() {
        let body = render(&fixture_snapshot());
        for line in [
            "# TYPE edge_monitor_processes_total gauge",
            "edge_monitor_processes_total{category=\"inference\"} 3",
            "edge_monitor_processes_total{category=\"training\"} 0",
            "edge_monitor_run_tokens_per_sec{model=\"phi3-mini\",pid=\"12345\"} 37.4000",
            "edge_monitor_run_vram_bytes{model=\"phi3-mini\",pid=\"12345\"} 4101296128",
            "edge_monitor_run_gpu_watts{model=\"phi3-mini\",pid=\"12345\"} 142.3000",
            "edge_monitor_gpu_watts{pid=\"12345\"} 142.3000",
            "edge_monitor_gpu_temp_celsius{pid=\"12345\"} 67.0000",
            "edge_monitor_ai_processes_active 3",
            "edge_monitor_cold_load_seconds{model=\"phi3-mini\"} 1.7500",
            "edge_monitor_governor_kills_total{reason=\"sigterm\"} 12",
            "edge_monitor_regressions_total{model=\"phi3-mini\",metric=\"tokens_per_sec_avg\"} 1",
            "edge_monitor_tick_count_total 4321",
        ] {
            assert!(body.contains(line), "missing line: {line}\nbody:\n{body}");
        }
    }

    #[test]
    fn empty_snapshot_still_emits_zero_processes_series() {
        let body = render(&MetricsSnapshot::new());
        assert!(body.contains("edge_monitor_processes_total{category=\"none\"} 0"));
        assert!(body.contains("edge_monitor_tick_count_total 0"));
        assert!(body.contains("edge_monitor_ai_processes_active 0"));
    }

    /// Lint the rendered output against the Prometheus exposition rules
    /// promtool checks for: every metric line must be preceded by a
    /// matching `# TYPE` directive, and every metric name has at most
    /// one `# TYPE` declaration. Doubles as a regression guard if a new
    /// family ever lands without the `# HELP`/`# TYPE` preamble.
    #[test]
    fn rendered_output_passes_prometheus_lint() {
        let body = render(&fixture_snapshot());
        let mut declared_types: HashMap<String, String> = HashMap::new();
        let mut seen_metric_names: std::collections::HashSet<String> = Default::default();

        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                // "# TYPE <name> <kind>"
                let mut parts = rest.split_whitespace();
                let name = parts.next().expect("# TYPE without name");
                let kind = parts.next().expect("# TYPE without kind");
                assert!(
                    declared_types
                        .insert(name.to_string(), kind.to_string())
                        .is_none(),
                    "duplicate # TYPE declaration for {name}"
                );
                continue;
            }
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            // metric line: <name>{labels} <value> OR <name> <value>
            let head = line.split_whitespace().next().unwrap_or("");
            let name = head.split('{').next().unwrap_or(head);
            assert!(
                declared_types.contains_key(name),
                "metric {name} emitted without a # TYPE declaration"
            );
            seen_metric_names.insert(name.to_string());
        }
        // A declared TYPE with zero samples is legal under the
        // exposition format (Prometheus treats it as "metric exists,
        // currently empty"), so we don't require a sample for every
        // family — only that every emitted sample has a matching TYPE,
        // which the loop above already enforced.
        let _ = seen_metric_names;
    }

    #[test]
    fn label_escape_handles_quotes_and_backslashes() {
        assert_eq!(escape_label("phi-3"), "phi-3");
        assert_eq!(escape_label("a\"b"), "a\\\"b");
        assert_eq!(escape_label("c\\d"), "c\\\\d");
        assert_eq!(escape_label("e\nf"), "e\\nf");
    }

    #[test]
    fn fmt_f32_handles_non_finite() {
        assert_eq!(fmt_f32(f32::NAN), "0");
        assert_eq!(fmt_f32(f32::INFINITY), "0");
        assert_eq!(fmt_f32(42.5), "42.5000");
    }

    /// End-to-end: bind to ephemeral port, GET /metrics, parse the
    /// 200 response body. Spec test from TEST.md R.2.3 / latest.md.
    #[tokio::test]
    async fn end_to_end_serve_metrics() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let snap: SnapshotHandle = Arc::new(Mutex::new(fixture_snapshot()));

        let snap2 = snap.clone();
        let server = tokio::spawn(async move { serve_loop(listener, snap2).await });

        // Crude client.
        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();
        client
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_to_end(&mut response),
        )
        .await;
        let s = String::from_utf8_lossy(&response);
        assert!(s.starts_with("HTTP/1.1 200 OK"), "got: {s}");
        assert!(s.contains("edge_monitor_tick_count_total 4321"), "got: {s}");
        server.abort();
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let snap: SnapshotHandle = Arc::new(Mutex::new(MetricsSnapshot::new()));
        let snap2 = snap.clone();
        let server = tokio::spawn(async move { serve_loop(listener, snap2).await });

        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_to_end(&mut response),
        )
        .await;
        let s = String::from_utf8_lossy(&response);
        assert!(s.starts_with("HTTP/1.1 404"), "got: {s}");
        server.abort();
    }
}
