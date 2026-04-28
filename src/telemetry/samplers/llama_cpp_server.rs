//! llama.cpp server Prometheus scraper (latest.md Tier 1.2b).
//!
//! Same architecture as the vLLM scraper but watches for the
//! `llama-server` binary and consumes the `llama_server_*` metric
//! namespace. llama.cpp doesn't expose a direct tokens/sec gauge —
//! we derive it from the `llama_server_n_decode_total` counter divided
//! by elapsed wall time (the spec calls this out).
//!
//! Endpoint: llama-server defaults to `--port 8080`, distinct from
//! vLLM's 8000. We re-use the parser from `vllm_prometheus` because
//! Prometheus exposition is format-stable across emitters.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;

use crate::telemetry::samplers::vllm_prometheus::parse_metrics;
use crate::telemetry::source::{
    ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
};

const DEFAULT_PORT: u16 = 8080;
const SCRAPE_TIMEOUT: Duration = Duration::from_millis(500);

/// Per-PID state needed to derive a tokens/sec rate from the
/// `n_decode_total` counter (which is monotonic).
#[derive(Debug, Clone)]
struct LastSample {
    decode_total: f64,
    /// Wall-clock instant at which `decode_total` was read. Tokio
    /// runs are not real-time but `Instant` is monotonic, which is
    /// what we need for delta arithmetic.
    when: Instant,
    /// Endpoint URL pinned after first successful scrape.
    url: Option<String>,
}

pub struct LlamaCppServerSource {
    client: reqwest::Client,
    state: HashMap<u32, LastSample>,
}

impl LlamaCppServerSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(SCRAPE_TIMEOUT)
            .build()
            .expect("default reqwest client must build");
        Self {
            client,
            state: HashMap::new(),
        }
    }

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

    pub fn endpoint_for(cmdline: &[String]) -> String {
        format!("http://127.0.0.1:{}/metrics", Self::discover_port(cmdline))
    }
}

impl Default for LlamaCppServerSource {
    fn default() -> Self {
        Self::new()
    }
}

fn re_llama_server_cmdline() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // The canonical names: `llama-server` (current builds) and
        // `server` (older). Bare-token match avoids bouncing on
        // unrelated paths that contain "server".
        Regex::new(r"(^|\s|/)(llama-server|llama_server)(\s|$)")
            .expect("llama_server cmdline regex")
    })
}

#[async_trait]
impl TelemetrySource for LlamaCppServerSource {
    fn name(&self) -> &str {
        "llama-cpp-server"
    }

    fn applies_to(&self, proc: &ProcessSnapshot) -> bool {
        let joined = proc.cmdline.join(" ");
        re_llama_server_cmdline().is_match(&joined)
    }

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        let url = match self.state.get(&proc.pid).and_then(|s| s.url.clone()) {
            Some(u) => u,
            None => Self::endpoint_for(&proc.cmdline),
        };

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
        let now = Instant::now();
        let frame = compute_frame(proc.pid, &metrics, self.state.get(&proc.pid), now);

        // Persist last sample for next call's rate calculation.
        if let Some(decode_total) = metrics.get("llama_server_n_decode_total") {
            self.state.insert(
                proc.pid,
                LastSample {
                    decode_total: *decode_total,
                    when: now,
                    url: Some(url),
                },
            );
        }
        Ok(frame)
    }
}

fn compute_frame(
    pid: u32,
    m: &HashMap<String, f64>,
    last: Option<&LastSample>,
    now: Instant,
) -> TelemetryFrame {
    let mut frame = TelemetryFrame::new(pid);

    // Direct gauges first (some llama.cpp builds expose the rate).
    if let Some(v) = m.get("llama_server_tokens_per_sec_avg") {
        frame.tokens_per_sec = Some(*v as f32);
    }
    if let Some(v) = m.get("llama_server_n_busy_slots") {
        frame.concurrent_requests = Some(*v as u32);
    }
    if let Some(v) = m.get("llama_server_kv_cache_usage") {
        frame.kv_cache_pct = Some((*v as f32) * 100.0);
    }

    // Derive tokens/sec from the n_decode_total counter when no direct
    // gauge is present. Spec calls this out for older llama.cpp builds.
    if frame.tokens_per_sec.is_none()
        && let Some(decode_now) = m.get("llama_server_n_decode_total")
        && let Some(prev) = last
    {
        let dt = now.saturating_duration_since(prev.when).as_secs_f32();
        let dn = (*decode_now - prev.decode_total) as f32;
        if dt > 0.0 && dn >= 0.0 {
            frame.tokens_per_sec = Some(dn / dt);
        }
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
            name: "llama-server".into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            environ: StdMap::new(),
            model_name: None,
        }
    }

    #[test]
    fn applies_to_recognises_llama_server() {
        let s = LlamaCppServerSource::new();
        assert!(s.applies_to(&snap(&["./llama-server", "-m", "phi3.gguf"])));
        assert!(s.applies_to(&snap(&[
            "/opt/llama.cpp/build/bin/llama-server",
            "--port",
            "9000"
        ])));
    }

    #[test]
    fn applies_to_rejects_non_llama_server() {
        let s = LlamaCppServerSource::new();
        assert!(!s.applies_to(&snap(&["llama-cli", "-m", "x.gguf"])));
        assert!(!s.applies_to(&snap(&["python", "server.py"])));
    }

    #[test]
    fn discover_port_default_is_8080() {
        assert_eq!(
            LlamaCppServerSource::discover_port(&["llama-server".into()]),
            DEFAULT_PORT
        );
    }

    #[test]
    fn discover_port_handles_both_flag_forms() {
        assert_eq!(
            LlamaCppServerSource::discover_port(&[
                "llama-server".into(),
                "--port".into(),
                "9000".into(),
            ]),
            9000
        );
        assert_eq!(
            LlamaCppServerSource::discover_port(&["llama-server".into(), "--port=9090".into(),]),
            9090
        );
    }

    #[test]
    fn compute_frame_uses_direct_gauge_when_present() {
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llama_server_tokens_per_sec_avg".into(), 42.0);
        m.insert("llama_server_n_busy_slots".into(), 3.0);
        m.insert("llama_server_kv_cache_usage".into(), 0.5);
        let f = compute_frame(7, &m, None, Instant::now());
        assert!((f.tokens_per_sec.unwrap() - 42.0).abs() < 1e-3);
        assert_eq!(f.concurrent_requests, Some(3));
        assert!((f.kv_cache_pct.unwrap() - 50.0).abs() < 1e-3);
    }

    #[test]
    fn compute_frame_derives_tps_from_counter_delta() {
        let t0 = Instant::now();
        // Simulate a 1s window during which 100 tokens decoded.
        let prev = LastSample {
            decode_total: 1000.0,
            when: t0,
            url: None,
        };
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llama_server_n_decode_total".into(), 1100.0);
        let now = t0 + Duration::from_secs(1);
        let f = compute_frame(7, &m, Some(&prev), now);
        // dt=1s, dn=100 → 100 tps.
        assert!((f.tokens_per_sec.unwrap() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn compute_frame_no_tps_without_prior_sample() {
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llama_server_n_decode_total".into(), 500.0);
        let f = compute_frame(7, &m, None, Instant::now());
        assert!(f.tokens_per_sec.is_none());
    }

    #[test]
    fn compute_frame_no_tps_when_counter_unchanged() {
        let t0 = Instant::now();
        let prev = LastSample {
            decode_total: 500.0,
            when: t0,
            url: None,
        };
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llama_server_n_decode_total".into(), 500.0);
        let now = t0 + Duration::from_millis(500);
        let f = compute_frame(7, &m, Some(&prev), now);
        // dn=0, dt>0 → tps=0.0 (idle server). Acceptable to surface 0
        // because the counter was actually read; an unread counter
        // would have produced None via the previous test.
        assert_eq!(f.tokens_per_sec, Some(0.0));
    }

    #[tokio::test]
    async fn end_to_end_scrape_through_local_server() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let body = "llama_server_tokens_per_sec_avg 99.5\nllama_server_n_busy_slots 2\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let mut s = LlamaCppServerSource::new();
        s.state.insert(
            7,
            LastSample {
                decode_total: 0.0,
                when: Instant::now(),
                url: Some(format!("http://127.0.0.1:{}/metrics", port)),
            },
        );

        let mut p = snap(&["llama-server"]);
        p.pid = 7;
        let frame = s.sample(&p).await.expect("scrape should succeed");
        assert!((frame.tokens_per_sec.unwrap() - 99.5).abs() < 1e-3);
        assert_eq!(frame.concurrent_requests, Some(2));
    }
}
