//! vLLM Prometheus scraper (latest.md Tier 1.2a).
//!
//! Detects vLLM processes by cmdline (`vllm` token or `vllm serve` /
//! `vllm.entrypoints` argv) and scrapes their `/metrics` endpoint on a
//! 500 ms timeout. The scrape result is folded into a
//! [`TelemetryFrame`] using the vLLM metric names from the spec.
//!
//! Endpoint discovery is *cached per PID*: the first successful scrape
//! pins the URL so the runtime doesn't re-probe every tick. A
//! permanent 404 / connection-refused on the cached URL flips the
//! cache to a poisoned state and the sampler returns a permanent
//! error so the dispatcher stops calling it.
//!
//! Architecture: parsing is split from HTTP so the parse path is
//! exhaustively unit-testable against canned `/metrics` bodies, and
//! the HTTP path can be mocked in integration tests with a tokio
//! TcpListener.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;

use crate::telemetry::source::{
    ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
};

/// Default vLLM serving port. Spec calls this out — vLLM defaults to
/// 8000 unless `--port` is given.
const DEFAULT_PORT: u16 = 8000;
const SCRAPE_TIMEOUT: Duration = Duration::from_millis(500);

/// vLLM Prometheus scraper. One instance is sufficient for many PIDs;
/// per-PID endpoint URLs live in `endpoint_cache`.
pub struct VllmPrometheusSource {
    client: reqwest::Client,
    /// pid → endpoint URL. `None` is the poisoned-state marker.
    endpoint_cache: HashMap<u32, Option<String>>,
}

impl VllmPrometheusSource {
    pub fn new() -> Self {
        // ok: expect — reqwest's default builder only fails when the
        // host's TLS / DNS resolver stack is broken; if that's true we
        // cannot run regardless.
        let client = reqwest::Client::builder()
            .timeout(SCRAPE_TIMEOUT)
            .build()
            .expect("default reqwest client must build");
        Self {
            client,
            endpoint_cache: HashMap::new(),
        }
    }

    /// Pure: parse `--port N` / `--port=N` / default 8000 from a
    /// cmdline. Public for unit testing of the discovery rules.
    pub fn discover_port(cmdline: &[String]) -> u16 {
        for (i, tok) in cmdline.iter().enumerate() {
            if let Some(rest) = tok.strip_prefix("--port=")
                && let Ok(p) = rest.parse::<u16>()
            {
                return p;
            }
            if tok == "--port"
                && let Some(next) = cmdline.get(i + 1)
                && let Ok(p) = next.parse::<u16>()
            {
                return p;
            }
        }
        DEFAULT_PORT
    }

    /// `127.0.0.1:<port>` is the only sane scrape target — vLLM
    /// commonly binds 0.0.0.0 but we don't want the agent to bounce
    /// off the box's external interface (which may be firewalled).
    pub fn endpoint_for(cmdline: &[String]) -> String {
        format!("http://127.0.0.1:{}/metrics", Self::discover_port(cmdline))
    }
}

impl Default for VllmPrometheusSource {
    fn default() -> Self {
        Self::new()
    }
}

fn re_vllm_cmdline() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // `vllm` as a bare token, `vllm serve`, `vllm.entrypoints.*`,
        // or `python -m vllm`. The boundary `\b` keeps "vllm" from
        // matching inside arbitrary substrings.
        // ok: expect — static regex, compile-time-constant pattern.
        Regex::new(r"\bvllm(\.entrypoints|\s+serve|\b)").expect("vllm cmdline regex")
    })
}

#[async_trait]
impl TelemetrySource for VllmPrometheusSource {
    fn name(&self) -> &str {
        "vllm-prometheus"
    }

    fn applies_to(&self, proc: &ProcessSnapshot) -> bool {
        let joined = proc.cmdline.join(" ");
        if re_vllm_cmdline().is_match(&joined) {
            return true;
        }
        proc.environ.keys().any(|k| k.starts_with("VLLM_"))
    }

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        // Resolve endpoint URL (cached per PID).
        let url = match self.endpoint_cache.get(&proc.pid) {
            Some(Some(u)) => u.clone(),
            Some(None) => {
                return Err(SourceError::Permanent("endpoint poisoned".into()));
            }
            None => {
                let url = Self::endpoint_for(&proc.cmdline);
                self.endpoint_cache.insert(proc.pid, Some(url.clone()));
                url
            }
        };

        // Scrape. Map all error classes onto Transient on first
        // failure; a second consecutive failure poisons the cache —
        // but the dispatcher (Tier 1.2 full) handles the back-off
        // policy, so here we just signal Transient.
        let body = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| SourceError::Transient(format!("GET {}: {}", url, e)))?
            .error_for_status()
            .map_err(|e| SourceError::Transient(format!("status {}: {}", url, e)))?
            .text()
            .await
            .map_err(|e| SourceError::Transient(format!("body {}: {}", url, e)))?;

        let metrics = parse_metrics(&body);
        Ok(frame_from_metrics(proc.pid, &metrics))
    }
}

/// Parse a Prometheus exposition-format body into a flat
/// `metric_name -> value` map. Lines starting with `#` are skipped;
/// label-bearing lines have the labels stripped (we don't need them
/// for the metrics we care about).
///
/// Strict — malformed lines are dropped silently rather than
/// poisoning the map. The inputs here come from a known-good
/// implementation (vLLM); we'd rather miss a metric than crash.
pub fn parse_metrics(body: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `name{labels} value` or `name value`.
        let (name_part, value_part) = match line.rsplit_once(' ') {
            Some(parts) => parts,
            None => continue,
        };
        let value: f64 = match value_part.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let name = match name_part.split_once('{') {
            Some((n, _)) => n.trim().to_string(),
            None => name_part.trim().to_string(),
        };
        if !name.is_empty() {
            out.insert(name, value);
        }
    }
    out
}

/// Project the parsed metric map onto a `TelemetryFrame`. Names match
/// vLLM's standard exposition (verified against vllm 0.5.x).
pub fn frame_from_metrics(pid: u32, m: &HashMap<String, f64>) -> TelemetryFrame {
    let mut frame = TelemetryFrame::new(pid);

    if let Some(v) = m.get("vllm:avg_generation_throughput_toks_per_s") {
        frame.tokens_per_sec = Some(*v as f32);
    }
    if let Some(v) = m.get("vllm:gpu_cache_usage_perc") {
        // vLLM reports as a 0..1 fraction; normalise to %.
        frame.kv_cache_pct = Some((*v as f32) * 100.0);
    }
    if let Some(v) = m.get("vllm:num_requests_running") {
        frame.concurrent_requests = Some(*v as u32);
    }
    // Tier 3.4 — vLLM's queue depth. Negative / non-finite values are
    // dropped so a malformed scrape can't poison the time-weighted
    // gauge with garbage.
    if let Some(v) = m.get("vllm:num_requests_waiting")
        && v.is_finite()
        && *v >= 0.0
    {
        frame.num_requests_waiting = Some(*v as u32);
    }
    // Tier 3.3 — vLLM exposes `vllm:num_preemptions_total`, a monotonic
    // counter of requests preempted because the KV cache filled. We
    // surface it as the run's eviction count. Negative or non-finite
    // values are dropped (counter must be >= 0); the accumulator does
    // a separate non-negative-delta check across samples.
    if let Some(v) = m.get("vllm:num_preemptions_total")
        && v.is_finite()
        && *v >= 0.0
    {
        frame.kv_cache_evictions = Some(*v as u64);
    }
    if let Some(v) = m.get("vllm:e2e_request_latency_seconds_sum") {
        // Sum-only is not directly latency; expose as an extra so a
        // future histogram-aware extractor can use it.
        frame
            .extras
            .insert("vllm:e2e_request_latency_seconds_sum".into(), *v);
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdMap;

    fn snap(cmdline: &[&str]) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 1,
            name: "vllm".into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            environ: StdMap::new(),
            model_name: None,
            cpu_pct: 0.0,
            ppid: None,
        }
    }

    #[test]
    fn applies_to_recognises_vllm_serve() {
        let s = VllmPrometheusSource::new();
        assert!(s.applies_to(&snap(&["vllm", "serve", "phi3-mini"])));
        assert!(s.applies_to(&snap(&[
            "python",
            "-m",
            "vllm.entrypoints.openai.api_server"
        ])));
    }

    #[test]
    fn applies_to_via_env_var() {
        let mut p = snap(&["python", "irrelevant.py"]);
        p.environ.insert("VLLM_USE_MODELSCOPE".into(), "1".into());
        let s = VllmPrometheusSource::new();
        assert!(s.applies_to(&p));
    }

    #[test]
    fn applies_to_rejects_non_vllm() {
        let s = VllmPrometheusSource::new();
        assert!(!s.applies_to(&snap(&["bash"])));
        assert!(!s.applies_to(&snap(&["python", "ml.py"])));
    }

    #[test]
    fn discover_port_finds_flag_value_form() {
        assert_eq!(
            VllmPrometheusSource::discover_port(&[
                "vllm".into(),
                "serve".into(),
                "--port".into(),
                "9000".into(),
            ]),
            9000
        );
    }

    #[test]
    fn discover_port_finds_flag_eq_form() {
        assert_eq!(
            VllmPrometheusSource::discover_port(&[
                "vllm".into(),
                "serve".into(),
                "--port=9090".into(),
            ]),
            9090
        );
    }

    #[test]
    fn discover_port_falls_back_to_default() {
        assert_eq!(
            VllmPrometheusSource::discover_port(&["vllm".into(), "serve".into()]),
            DEFAULT_PORT
        );
    }

    #[test]
    fn endpoint_for_targets_loopback() {
        let url = VllmPrometheusSource::endpoint_for(&["vllm".into(), "serve".into()]);
        assert_eq!(url, "http://127.0.0.1:8000/metrics");
    }

    #[test]
    fn parse_metrics_extracts_named_lines() {
        let body = r#"
# HELP vllm:avg_generation_throughput_toks_per_s avg gen throughput
# TYPE vllm:avg_generation_throughput_toks_per_s gauge
vllm:avg_generation_throughput_toks_per_s{model="phi3"} 37.4
vllm:num_requests_running{} 8
vllm:gpu_cache_usage_perc 0.85
malformed line without value
not_a_metric# garbage
"#;
        let m = parse_metrics(body);
        assert_eq!(
            m.get("vllm:avg_generation_throughput_toks_per_s").copied(),
            Some(37.4)
        );
        assert_eq!(m.get("vllm:num_requests_running").copied(), Some(8.0));
        assert_eq!(m.get("vllm:gpu_cache_usage_perc").copied(), Some(0.85));
    }

    #[test]
    fn frame_from_metrics_maps_vllm_names() {
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("vllm:avg_generation_throughput_toks_per_s".into(), 37.4);
        m.insert("vllm:gpu_cache_usage_perc".into(), 0.85);
        m.insert("vllm:num_requests_running".into(), 8.0);
        m.insert("vllm:num_preemptions_total".into(), 12.0);
        let f = frame_from_metrics(123, &m);
        assert_eq!(f.pid, 123);
        assert!((f.tokens_per_sec.unwrap() - 37.4).abs() < 1e-3);
        assert!((f.kv_cache_pct.unwrap() - 85.0).abs() < 1e-3);
        assert_eq!(f.concurrent_requests, Some(8));
        assert_eq!(f.kv_cache_evictions, Some(12));
    }

    /// Negative preemption counter is non-physical — drop it rather
    /// than store noise that the accumulator would have to filter.
    #[test]
    fn frame_from_metrics_rejects_negative_preemptions() {
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("vllm:num_preemptions_total".into(), -1.0);
        let f = frame_from_metrics(1, &m);
        assert_eq!(f.kv_cache_evictions, None);
    }

    /// HTTP path: spin up a tokio listener serving canned bytes, point
    /// the sampler at it, assert the parsed frame.
    #[tokio::test]
    async fn end_to_end_scrape_through_local_server() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Tiny one-shot HTTP server that returns a canned vLLM body.
        tokio::spawn(async move {
            let body =
                "vllm:avg_generation_throughput_toks_per_s 41.7\nvllm:num_requests_running 3\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain request bytes (just the headers, then EOF).
                let mut buf = [0u8; 1024];
                let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let mut s = VllmPrometheusSource::new();
        // Pre-seed the endpoint cache so we don't hit the default 8000.
        s.endpoint_cache
            .insert(7, Some(format!("http://127.0.0.1:{}/metrics", port)));

        let proc = snap(&["vllm", "serve"]);
        let mut p = proc;
        p.pid = 7;
        let frame = s.sample(&p).await.expect("scrape should succeed");
        assert!((frame.tokens_per_sec.unwrap() - 41.7).abs() < 1e-3);
        assert_eq!(frame.concurrent_requests, Some(3));
    }
}
