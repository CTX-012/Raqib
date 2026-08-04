//! llama.cpp server Prometheus scraper (latest.md Tier 1.2b).
//!
//! Same architecture as the vLLM scraper but watches for the
//! `llama-server` binary. Consumes BOTH metric-name schemes emitted
//! by upstream builds:
//!
//! * NEW (current llama.cpp / BitNet) — `llamacpp:*` prefix, with a
//!   direct rate gauge `llamacpp:predicted_tokens_seconds` that
//!   avoids the two-sample warmup entirely, plus a matched pair of
//!   counters `llamacpp:tokens_predicted_total` /
//!   `llamacpp:tokens_predicted_seconds_total` (tokens per
//!   generation-second — excludes idle wall time).
//! * OLD (pre-rename builds) — `llama_server_*` prefix, no direct
//!   rate gauge; we derive tokens/sec from
//!   `llama_server_n_decode_total` divided by elapsed wall time.
//!
//! Reads are prefer-new-fall-back-to-old at every metric site, so a
//! mixed fleet (some hosts on new upstream, some pinned to old) still
//! populates the same `TelemetryFrame` fields.
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

/// Per-PID state needed to derive a tokens/sec rate from monotonic
/// counters. Holds the last-scraped tokens-count counter (either
/// old-format `llama_server_n_decode_total` or new-format
/// `llamacpp:tokens_predicted_total` — semantically equivalent), plus
/// the optional new-format cumulative generation-seconds counter
/// which enables delta-over-delta (tokens per generation-second)
/// when both samples have it.
#[derive(Debug, Clone)]
struct LastSample {
    /// Monotonic count of tokens predicted so far.
    tokens_total: f64,
    /// New-format cumulative generation-seconds counter. `None` when
    /// upstream is on the old metric names (or the field wasn't in
    /// the last scrape). Used for the delta-over-delta path.
    tokens_seconds_total: Option<f64>,
    /// Wall-clock instant at which the counters were read. Tokio
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
        // ok: expect — reqwest's default builder only fails when the
        // host's TLS / DNS resolver stack is broken; if that's true we
        // cannot run regardless.
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
        // ok: expect — static regex, compile-time-constant pattern.
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
        // Prefer the new-format counter name; fall back to old for
        // pre-rename builds. Also snapshot the optional generation-
        // seconds counter to enable delta-over-delta derivation.
        let tokens_total = metrics
            .get("llamacpp:tokens_predicted_total")
            .or_else(|| metrics.get("llama_server_n_decode_total"))
            .copied();
        if let Some(tt) = tokens_total {
            let tokens_seconds_total = metrics
                .get("llamacpp:tokens_predicted_seconds_total")
                .copied();
            self.state.insert(
                proc.pid,
                LastSample {
                    tokens_total: tt,
                    tokens_seconds_total,
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

    // Direct rate gauge — NEW format only. `llamacpp:predicted_tokens_
    // seconds` is a per-scrape rate (tokens/sec), so no two-sample
    // warmup is needed. Old-format builds have no rate-gauge
    // equivalent (the old `llama_server_tokens_per_sec_avg` was never
    // shipped in mainline; it lives here for pre-rename local
    // patches). Prefer new; fall back to the historical name for any
    // fleet still emitting it.
    if let Some(v) = m
        .get("llamacpp:predicted_tokens_seconds")
        .or_else(|| m.get("llama_server_tokens_per_sec_avg"))
    {
        frame.tokens_per_sec = Some(*v as f32);
    }

    // Busy slots — NEW `llamacpp:requests_processing` → OLD
    // `llama_server_n_busy_slots`. Semantics identical (in-flight
    // request count).
    if let Some(v) = m
        .get("llamacpp:requests_processing")
        .or_else(|| m.get("llama_server_n_busy_slots"))
    {
        frame.concurrent_requests = Some(*v as u32);
    }

    // KV cache — NEW `llamacpp:kv_cache_usage_ratio` → OLD
    // `llama_server_kv_cache_usage`. Both are 0..1 fractions;
    // normalise to %.
    if let Some(v) = m
        .get("llamacpp:kv_cache_usage_ratio")
        .or_else(|| m.get("llama_server_kv_cache_usage"))
    {
        frame.kv_cache_pct = Some((*v as f32) * 100.0);
    }

    // Counter-derived tokens/sec — only used if the direct rate gauge
    // was absent (old builds, or the newer gauge is momentarily
    // missing). Requires a prior sample.
    if frame.tokens_per_sec.is_none()
        && let Some(prev) = last
    {
        // Path A: NEW counter pair. `tokens_predicted_total /
        // tokens_predicted_seconds_total` — dividing the deltas gives
        // tokens per *generation*-second, excluding idle wall time
        // (more accurate than wall-time division when the server is
        // mostly idle between requests). Requires both current
        // readings AND a prior seconds-counter reading.
        if let (Some(t_now), Some(s_now), Some(s_prev)) = (
            m.get("llamacpp:tokens_predicted_total"),
            m.get("llamacpp:tokens_predicted_seconds_total"),
            prev.tokens_seconds_total,
        ) {
            let dn = (*t_now - prev.tokens_total) as f32;
            let dt_gen = (*s_now - s_prev) as f32;
            if dt_gen > 0.0 && dn >= 0.0 {
                frame.tokens_per_sec = Some(dn / dt_gen);
            }
        }

        // Path B: NEW tokens counter, no seconds pair — fall back to
        // wall-time delta. Handles the transitional case where
        // upstream emits the tokens counter but the run started
        // before we captured a matching seconds-counter sample.
        if frame.tokens_per_sec.is_none()
            && let Some(t_now) = m.get("llamacpp:tokens_predicted_total")
        {
            let dt = now.saturating_duration_since(prev.when).as_secs_f32();
            let dn = (*t_now - prev.tokens_total) as f32;
            if dt > 0.0 && dn >= 0.0 {
                frame.tokens_per_sec = Some(dn / dt);
            }
        }

        // Path C: OLD counter — the pre-rename
        // `llama_server_n_decode_total` divided by wall time. Kept
        // for backward compatibility with older llama.cpp builds.
        if frame.tokens_per_sec.is_none()
            && let Some(t_now) = m.get("llama_server_n_decode_total")
        {
            let dt = now.saturating_duration_since(prev.when).as_secs_f32();
            let dn = (*t_now - prev.tokens_total) as f32;
            if dt > 0.0 && dn >= 0.0 {
                frame.tokens_per_sec = Some(dn / dt);
            }
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
            cpu_pct: 0.0,
            ppid: None,
            workload_category: None,
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
        // Backward-compat path: OLD counter name only, no seconds
        // pair — uses wall-time division (Path C).
        let prev = LastSample {
            tokens_total: 1000.0,
            tokens_seconds_total: None,
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
            tokens_total: 500.0,
            tokens_seconds_total: None,
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
                tokens_total: 0.0,
                tokens_seconds_total: None,
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

    // -----------------------------------------------------------------
    // NEW metric-name scheme tests (`llamacpp:*` prefix).
    //
    // Upstream llama.cpp renamed its Prometheus metrics from
    // `llama_server_*` to `llamacpp:*` — raqib was reading only the
    // old names, so tok/s / kv_cache / activity were ALWAYS null on
    // current builds. These pin the fix: the sampler MUST populate
    // frame fields from the new names too, and the direct rate gauge
    // `llamacpp:predicted_tokens_seconds` MUST let tok/s populate from
    // a single scrape (no two-sample warmup).
    // -----------------------------------------------------------------

    #[test]
    fn compute_frame_new_scheme_direct_rate_gauge_populates_tps_from_single_scrape() {
        // The bug: current llama.cpp emits `llamacpp:predicted_tokens_
        // seconds` (a direct rate gauge). Prior code only read
        // `llama_server_tokens_per_sec_avg`, so tok/s stayed null even
        // when the server was actively generating.
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llamacpp:predicted_tokens_seconds".into(), 14.9);
        m.insert("llamacpp:kv_cache_usage_ratio".into(), 0.42);
        m.insert("llamacpp:requests_processing".into(), 1.0);
        // No prior sample — the direct gauge must be enough.
        let f = compute_frame(11, &m, None, Instant::now());
        assert!(
            (f.tokens_per_sec.unwrap() - 14.9).abs() < 1e-3,
            "tokens_per_sec must populate from the direct rate gauge on a single scrape"
        );
        assert!((f.kv_cache_pct.unwrap() - 42.0).abs() < 1e-3);
        assert_eq!(f.concurrent_requests, Some(1));
    }

    #[test]
    fn compute_frame_new_scheme_counter_pair_uses_delta_over_generation_seconds() {
        // Delta-over-delta: (tokens_predicted_total delta) /
        // (tokens_predicted_seconds_total delta) = tokens per
        // generation-second (excludes idle wall time). Prior sample
        // holds both counters; current sample supplies both.
        let t0 = Instant::now();
        let prev = LastSample {
            tokens_total: 500.0,
            tokens_seconds_total: Some(20.0),
            when: t0,
            url: None,
        };
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llamacpp:tokens_predicted_total".into(), 650.0);
        m.insert("llamacpp:tokens_predicted_seconds_total".into(), 30.0);
        // Wall time between samples is 100 s (mostly idle), but the
        // server spent 10 s actually generating, producing 150 tokens.
        // Correct rate is 150 / 10 = 15.0 tok/generation-sec — the
        // naive wall-time divide would give 1.5, so this discriminates.
        let now = t0 + Duration::from_secs(100);
        let f = compute_frame(7, &m, Some(&prev), now);
        assert!(
            (f.tokens_per_sec.unwrap() - 15.0).abs() < 1e-3,
            "delta-over-delta path must use generation-seconds, not wall time"
        );
    }

    #[test]
    fn compute_frame_new_scheme_falls_back_to_wall_time_when_no_seconds_prior() {
        // New tokens counter present, but no prior seconds-counter
        // reading (first scrape after upstream upgrade caught the
        // seconds counter mid-run). Path B: wall-time division.
        let t0 = Instant::now();
        let prev = LastSample {
            tokens_total: 1000.0,
            tokens_seconds_total: None,
            when: t0,
            url: None,
        };
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llamacpp:tokens_predicted_total".into(), 1200.0);
        // No `llamacpp:tokens_predicted_seconds_total` in this scrape
        // either — should still yield tps from wall-time.
        let now = t0 + Duration::from_secs(2);
        let f = compute_frame(7, &m, Some(&prev), now);
        // dt=2s, dn=200 → 100 tps.
        assert!((f.tokens_per_sec.unwrap() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn compute_frame_new_scheme_wins_when_both_schemes_present() {
        // Prefer-new: if a mixed-emitter build somehow reports both,
        // the new-format value must be authoritative for every field.
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llamacpp:predicted_tokens_seconds".into(), 20.0);
        m.insert("llama_server_tokens_per_sec_avg".into(), 5.0);
        m.insert("llamacpp:kv_cache_usage_ratio".into(), 0.9);
        m.insert("llama_server_kv_cache_usage".into(), 0.1);
        m.insert("llamacpp:requests_processing".into(), 4.0);
        m.insert("llama_server_n_busy_slots".into(), 1.0);
        let f = compute_frame(0, &m, None, Instant::now());
        assert!((f.tokens_per_sec.unwrap() - 20.0).abs() < 1e-3);
        assert!((f.kv_cache_pct.unwrap() - 90.0).abs() < 1e-3);
        assert_eq!(f.concurrent_requests, Some(4));
    }

    #[tokio::test]
    async fn end_to_end_scrape_new_scheme_populates_all_fields() {
        // Fixture using the REAL current llama.cpp metric names from
        // the live-validation finding. Reproduces what raqib sees
        // against a running llama-server / BitNet today; asserts the
        // fields the old code left null now populate.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            // Metric names are verbatim from the upstream rename +
            // observed on live `/metrics`. Values are illustrative
            // but include the ~14.9 tok/s the operator flagged.
            let body = "\
# HELP llamacpp:predicted_tokens_seconds tokens/sec (direct rate)
# TYPE llamacpp:predicted_tokens_seconds gauge
llamacpp:predicted_tokens_seconds 14.9
# HELP llamacpp:tokens_predicted_total total tokens predicted
# TYPE llamacpp:tokens_predicted_total counter
llamacpp:tokens_predicted_total 8421
# HELP llamacpp:tokens_predicted_seconds_total seconds spent predicting
# TYPE llamacpp:tokens_predicted_seconds_total counter
llamacpp:tokens_predicted_seconds_total 564.3
# HELP llamacpp:kv_cache_usage_ratio 0..1
# TYPE llamacpp:kv_cache_usage_ratio gauge
llamacpp:kv_cache_usage_ratio 0.31
# HELP llamacpp:requests_processing in-flight requests
# TYPE llamacpp:requests_processing gauge
llamacpp:requests_processing 2
";
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
        // Pin the URL so we hit the local test listener.
        s.state.insert(
            9,
            LastSample {
                tokens_total: 0.0,
                tokens_seconds_total: None,
                when: Instant::now(),
                url: Some(format!("http://127.0.0.1:{}/metrics", port)),
            },
        );

        let mut p = snap(&["llama-server"]);
        p.pid = 9;
        let frame = s.sample(&p).await.expect("scrape should succeed");
        assert!(
            (frame.tokens_per_sec.unwrap() - 14.9).abs() < 1e-3,
            "tok/s must populate from llamacpp:predicted_tokens_seconds"
        );
        assert!(
            (frame.kv_cache_pct.unwrap() - 31.0).abs() < 1e-3,
            "kv_cache_pct must populate from llamacpp:kv_cache_usage_ratio"
        );
        assert_eq!(
            frame.concurrent_requests,
            Some(2),
            "concurrent_requests must populate from llamacpp:requests_processing"
        );
    }

    #[test]
    fn compute_frame_new_scheme_kv_cache_only_populates_from_ratio() {
        // Regression guard: if only the new kv_cache_usage_ratio is
        // present, we must NOT leave kv_cache_pct null (that was the
        // live-observed bug for the KV field specifically).
        let mut m: HashMap<String, f64> = HashMap::new();
        m.insert("llamacpp:kv_cache_usage_ratio".into(), 0.75);
        let f = compute_frame(1, &m, None, Instant::now());
        assert!((f.kv_cache_pct.unwrap() - 75.0).abs() < 1e-3);
    }
}
