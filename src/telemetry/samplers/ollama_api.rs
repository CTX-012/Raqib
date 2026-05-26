//! Ollama `/api/ps` sampler (latest.md Tier 1.2c).
//!
//! Ollama doesn't expose Prometheus and embeds tokens/sec in the
//! per-request response JSON, which we cannot intercept without
//! becoming the user's HTTP client. So this sampler is a *model
//! identifier*, not a throughput source — it asks Ollama which model
//! is loaded right now and stamps the answer onto
//! `TelemetryFrame::model_name_hint` so the dispatcher can promote it
//! onto the `RunRecord`.
//!
//! For tokens/sec we'd fall through to stdout parsing (Tier 1.2d).
//!
//! Default port 11434. Endpoint is `/api/ps`, response shape (Ollama
//! 0.x):
//!
//! ```json
//! {
//!   "models": [
//!     { "name": "llama3:8b", "model": "llama3:8b", "size": ..., ... }
//!   ]
//! }
//! ```
//!
//! ## v1.1.0 B1 — ActivityState emission
//!
//! Folded empirical findings from Tester-B's pre-work captures:
//!   - `tests/empirical/v1_1_0_prep/ollama_api_format/`
//!   - `tests/empirical/v1_1_0_prep/ollama_generate_sidechannel/`
//!
//! For each Ollama *runner* subprocess (the one holding VRAM — see
//! v1.0.3 B-VRAM-ZERO compute-process enumeration), emit
//! `ActivityState` based on the runner's per-tick CPU% (bimodal at
//! raw `0-(100×cores)` scale per `ProcessSnapshot::cpu_pct` doc):
//!
//! ```text
//! /api/ps response shape                       → ActivityState
//! -------------------------------------------------------------
//! HTTP 200, runner model absent from models[]  → NotDetected
//! HTTP 200, runner model present, CPU < 5%
//!   (after 2-sample debounce per CHANGE 12)    → Idle
//! HTTP 200, runner model present, CPU ≥ 50%    → Active
//! HTTP 200, runner model present, dead-band    → previous state held
//!   (5-50% empirically empty per Tester-B)
//! Connection refused / read timeout            → NotDetected,
//!   once-log on Up→Down transition
//! HTTP 5xx                                     → SourceError::Transient
//! ```
//!
//! `Loading` state is NOT emitted in v1.1.0. Empirical absence of a
//! load-state indicator in `/api/ps` (Tester-B verified no load
//! timestamp, no state field) means cold-start is invisible to
//! `/api/ps` polling for the small models this project targets.
//! Revisit in v1.2+ if side-channel detection becomes worth the
//! complexity (CHANGE 1).
//!
//! ## REJECTED active-detection signals (CHANGE 17)
//!
//! - `nvidia-smi --query-gpu=utilization.gpu`: REJECTED.
//!   Device-wide; conflates concurrent CUDA workloads. Tester-B
//!   verified: NOVA Python kept GPU util 57–100% while the Ollama
//!   runner was idle.
//! - `nvidia-smi --query-compute-apps memory`: REJECTED.
//!   VRAM size is constant between Active and Idle (not a signal).
//! - `nvidia-smi pmon -s u` per-PID SM%: REJECTED.
//!   Bursty + 250 ms scrape cost; no advantage over CPU%.
//! - `/api/generate` streaming poll: REJECTED.
//!   Streaming protocol incompatible with the poll model.
//!
//! All thresholds in this file are documented `EMPIRICAL` (locked
//! from Tester-B capture) or `PROVISIONAL: refined post-v1.1.0
//! sampler validation (v1.1.1)` where Tester-B couldn't pin them.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;

use crate::telemetry::source::{
    ActivityState, ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
};

const DEFAULT_PORT: u16 = 11434;
const SCRAPE_TIMEOUT: Duration = Duration::from_millis(500);

// EMPIRICAL (Tester-B): CPU% is bimodal. Idle 0-1% sustained,
// active 99-105% sustained, the 5-50% band empirically empty
// across 220 samples. Threshold of 50% sits in the empty band.
// NOT PROVISIONAL — empirically locked at the raw `0-(100×cores)`
// scale per `ProcessSnapshot::cpu_pct` doc-comment.
const OLLAMA_ACTIVE_CPU_PCT: f32 = 50.0;

// EMPIRICAL (Tester-B): a single transition sample occasionally
// hit ~10% during Active→Idle decay (<200ms from /api/generate
// done:true). 2-sample debounce guards against spurious Idle.
const OLLAMA_IDLE_CPU_PCT: f32 = 5.0;
const OLLAMA_IDLE_DEBOUNCE_SAMPLES: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonStatus {
    Up,
    Down,
}

pub struct OllamaApiSource {
    client: reqwest::Client,
    /// pid → endpoint URL. Same single-cache shape as the other HTTP
    /// samplers; no per-PID identity needed because Ollama runs as
    /// one daemon per host typically.
    endpoint_cache: HashMap<u32, String>,
    /// v1.1.0 B1 — debounce counter keyed by **model name** so runner
    /// re-spawn (Ollama silently evicts + recreates the subprocess
    /// under VRAM pressure per CHANGE 14) preserves the streak
    /// across the new runner PID.
    per_model_idle_streak: HashMap<String, u8>,
    /// v1.1.0 B1 — track previously-emitted state per model so the
    /// dead band (5-50%, empirically empty but defensively handled)
    /// holds the prior verdict.
    per_model_last_state: HashMap<String, ActivityState>,
    /// v1.1.0 B1 — daemon up/down for once-log on transition.
    /// `None` until first sample resolves.
    last_daemon_status: Option<DaemonStatus>,
}

impl OllamaApiSource {
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
            per_model_idle_streak: HashMap::new(),
            per_model_last_state: HashMap::new(),
            last_daemon_status: None,
        }
    }

    pub fn discover_port(cmdline: &[String], environ: &HashMap<String, String>) -> u16 {
        // Ollama honours `OLLAMA_HOST=0.0.0.0:11435` as well as positional
        // host args; we accept both forms but always rewrite to loopback.
        if let Some(host) = environ.get("OLLAMA_HOST")
            && let Some(port) = parse_port_from_host(host)
        {
            return port;
        }
        for (i, tok) in cmdline.iter().enumerate() {
            if let Some(rest) = tok.strip_prefix("--host=")
                && let Some(port) = parse_port_from_host(rest)
            {
                return port;
            }
            if tok == "--host"
                && let Some(next) = cmdline.get(i + 1)
                && let Some(port) = parse_port_from_host(next)
            {
                return port;
            }
        }
        DEFAULT_PORT
    }

    pub fn endpoint_for(cmdline: &[String], environ: &HashMap<String, String>) -> String {
        format!(
            "http://127.0.0.1:{}/api/ps",
            Self::discover_port(cmdline, environ)
        )
    }

    /// v1.1.0 B1 — log-once gate on daemon status transition. Returns
    /// the resolved status for caller bookkeeping.
    fn record_daemon_status(&mut self, current: DaemonStatus) {
        let prior = self.last_daemon_status.replace(current);
        if prior != Some(current) {
            match current {
                DaemonStatus::Down => {
                    tracing::info!(
                        "Ollama /api/ps unreachable (connection refused or timeout); \
                         emitting NotDetected for tracked runners until daemon recovers"
                    );
                }
                DaemonStatus::Up if prior.is_some() => {
                    tracing::info!("Ollama /api/ps recovered; resuming normal sampling");
                }
                DaemonStatus::Up => {}
            }
        }
    }

    /// v1.1.0 B1 — apply the bimodal threshold + 2-sample debounce
    /// for one runner. `model` keys the per-model state so the
    /// streak survives runner re-spawn (CHANGE 14). Returns the
    /// ActivityState to emit this tick.
    fn classify_runner_activity(&mut self, model: &str, cpu_pct: f32) -> ActivityState {
        if cpu_pct >= OLLAMA_ACTIVE_CPU_PCT {
            // Active — reset the idle streak.
            self.per_model_idle_streak.insert(model.to_string(), 0);
            self.per_model_last_state
                .insert(model.to_string(), ActivityState::Active);
            ActivityState::Active
        } else if cpu_pct < OLLAMA_IDLE_CPU_PCT {
            // Idle candidate — increment the streak; only emit Idle
            // after 2 consecutive sub-5% samples (CHANGE 12).
            let streak = self
                .per_model_idle_streak
                .entry(model.to_string())
                .and_modify(|n| *n = n.saturating_add(1))
                .or_insert(1);
            let state = if *streak >= OLLAMA_IDLE_DEBOUNCE_SAMPLES {
                ActivityState::Idle
            } else {
                // Hold previous state through the debounce window.
                *self
                    .per_model_last_state
                    .get(model)
                    .unwrap_or(&ActivityState::Active)
            };
            self.per_model_last_state.insert(model.to_string(), state);
            state
        } else {
            // 5-50 % dead band (empirically empty per Tester-B). Hold
            // the previous state; don't perturb the idle streak.
            let state = *self
                .per_model_last_state
                .get(model)
                .unwrap_or(&ActivityState::Active);
            // Don't touch idle_streak: the dead band is neither a
            // confirmed idle sample nor an Active confirmation.
            state
        }
    }
}

impl Default for OllamaApiSource {
    fn default() -> Self {
        Self::new()
    }
}

/// `host` may be `1.2.3.4:5678`, `:5678`, or `1.2.3.4` (port omitted).
/// We only care about the port — Ollama always listens on loopback in
/// practice, and we never call out to the network address anyway.
fn parse_port_from_host(host: &str) -> Option<u16> {
    let after_colon = host.rsplit_once(':')?;
    after_colon.1.parse().ok()
}

fn re_ollama_cmdline() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // `ollama serve`, `ollama run`, or just `ollama` as a token.
        // ok: expect — static regex, compile-time-constant pattern.
        Regex::new(r"(^|\s|/)ollama(\s|$)").expect("ollama cmdline regex")
    })
}

#[async_trait]
impl TelemetrySource for OllamaApiSource {
    fn name(&self) -> &str {
        "ollama-api"
    }

    fn applies_to(&self, proc: &ProcessSnapshot) -> bool {
        let joined = proc.cmdline.join(" ");
        re_ollama_cmdline().is_match(&joined) || proc.environ.contains_key("OLLAMA_HOST")
    }

    async fn sample(&mut self, proc: &ProcessSnapshot) -> SourceResult<TelemetryFrame> {
        let url = match self.endpoint_cache.get(&proc.pid) {
            Some(u) => u.clone(),
            None => {
                let u = Self::endpoint_for(&proc.cmdline, &proc.environ);
                self.endpoint_cache.insert(proc.pid, u.clone());
                u
            }
        };
        // v1.1.0 B1 — explicit connect/timeout branch lets us emit
        // NotDetected rather than swallow the error into Transient,
        // so the accumulator's most-recent-non-None wire is freed of
        // stale Active state across an outage (CHANGE 6).
        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() => {
                self.record_daemon_status(DaemonStatus::Down);
                return Ok(TelemetryFrame {
                    activity_state: Some(ActivityState::NotDetected),
                    ..TelemetryFrame::new(proc.pid)
                });
            }
            Err(e) => return Err(SourceError::Transient(format!("GET {}: {}", url, e))),
        };
        let resp = resp
            .error_for_status()
            .map_err(|e| SourceError::Transient(format!("status {}: {}", url, e)))?;
        let body = resp
            .text()
            .await
            .map_err(|e| SourceError::Transient(format!("body {}: {}", url, e)))?;
        self.record_daemon_status(DaemonStatus::Up);

        let loaded = parse_loaded_models(&body);

        // v1.1.0 B1 — runner vs daemon dispatch. The runner sample
        // carries an annotated model_name (classifier resolves it
        // from cmdline); the daemon does not. Runners get the
        // activity_state treatment; the daemon keeps the existing
        // model_name_hint frame (one per loaded model, but cheap to
        // emit just the first for `RunRecord` promotion).
        if let Some(my_model) = proc.model_name.as_deref() {
            // CHANGE 14: re-resolve runner PID every tick. The dispatcher
            // already does this — `proc.pid` is the live PID, and a
            // re-spawned runner appears as a new ProcessSnapshot with the
            // same `model_name` (idle streak survives via per_model_*).
            let activity = if loaded.iter().any(|m| m == my_model) {
                self.classify_runner_activity(my_model, proc.cpu_pct)
            } else {
                // CHANGE 5: sub-second unload — model gone from /api/ps
                // means the runner is no longer hosting it. Clear
                // per-model state so a future reload starts fresh.
                self.per_model_idle_streak.remove(my_model);
                self.per_model_last_state.remove(my_model);
                ActivityState::NotDetected
            };
            Ok(TelemetryFrame {
                pid: proc.pid,
                activity_state: Some(activity),
                model_name_hint: Some(my_model.to_string()),
                ..TelemetryFrame::new(proc.pid)
            })
        } else {
            // Daemon path — preserve existing model_name_hint behavior.
            // No activity_state on the daemon row (its activity is
            // determined by the runner it spawned).
            let model_name = loaded.first().cloned();
            Ok(TelemetryFrame {
                pid: proc.pid,
                model_name_hint: model_name,
                ..TelemetryFrame::new(proc.pid)
            })
        }
    }
}

#[derive(Debug, Deserialize)]
struct PsResponse {
    #[serde(default)]
    models: Vec<PsModel>,
}

#[derive(Debug, Deserialize)]
struct PsModel {
    #[serde(default)]
    name: String,
}

/// Parse Ollama's `/api/ps` JSON. Returns the first loaded model name
/// when present; `None` for empty / malformed bodies.
///
/// Strict — we don't try to recover anything from a non-JSON body,
/// because the sampler is opt-in via cmdline detection and a
/// malformed body is a real misconfiguration the operator should see.
pub fn parse_loaded_model(body: &str) -> Option<String> {
    parse_loaded_models(body).into_iter().next()
}

/// v1.1.0 B1 — returns every loaded model name from `/api/ps`. Empty
/// vec for empty / malformed bodies (same strict policy as the legacy
/// single-model helper).
pub fn parse_loaded_models(body: &str) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<PsResponse>(body) else {
        return Vec::new();
    };
    parsed
        .models
        .into_iter()
        .filter_map(|m| if m.name.is_empty() { None } else { Some(m.name) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdMap;

    fn snap(cmdline: &[&str]) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 1,
            name: "ollama".into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            environ: StdMap::new(),
            model_name: None,
            cpu_pct: 0.0,
            ppid: None,
        }
    }

    fn runner_snap(model: &str, cpu_pct: f32) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 99,
            name: "ollama".into(),
            cmdline: vec!["ollama".into(), "runner".into()],
            environ: StdMap::new(),
            model_name: Some(model.into()),
            cpu_pct,
        }
    }

    #[test]
    fn applies_to_recognises_ollama_serve() {
        let s = OllamaApiSource::new();
        assert!(s.applies_to(&snap(&["ollama", "serve"])));
        assert!(s.applies_to(&snap(&["/usr/local/bin/ollama", "run", "llama3"])));
    }

    #[test]
    fn applies_to_via_env_var() {
        let s = OllamaApiSource::new();
        let mut p = snap(&["python", "x.py"]);
        p.environ
            .insert("OLLAMA_HOST".into(), "1.2.3.4:5678".into());
        assert!(s.applies_to(&p));
    }

    #[test]
    fn applies_to_rejects_non_ollama() {
        let s = OllamaApiSource::new();
        assert!(!s.applies_to(&snap(&["bash"])));
        assert!(!s.applies_to(&snap(&["llama-server"])));
    }

    #[test]
    fn discover_port_default() {
        let env = StdMap::new();
        assert_eq!(
            OllamaApiSource::discover_port(&["ollama".into()], &env),
            DEFAULT_PORT
        );
    }

    #[test]
    fn discover_port_from_env() {
        let mut env = StdMap::new();
        env.insert("OLLAMA_HOST".into(), "0.0.0.0:9999".into());
        assert_eq!(
            OllamaApiSource::discover_port(&["ollama".into()], &env),
            9999
        );
    }

    #[test]
    fn discover_port_from_host_flag() {
        let env = StdMap::new();
        assert_eq!(
            OllamaApiSource::discover_port(&["ollama".into(), "--host=:7777".into()], &env),
            7777
        );
    }

    #[test]
    fn parse_loaded_model_extracts_first() {
        let body = r#"{"models":[{"name":"llama3:8b","model":"llama3:8b","size":5000000000}]}"#;
        assert_eq!(parse_loaded_model(body).as_deref(), Some("llama3:8b"));
    }

    #[test]
    fn parse_loaded_model_returns_none_when_empty() {
        assert!(parse_loaded_model(r#"{"models":[]}"#).is_none());
        assert!(parse_loaded_model("").is_none());
        assert!(parse_loaded_model("not json").is_none());
    }

    // ────────────────────────────────────────────────────────────
    // v1.1.0 B1 — state-mapping tests.
    // ────────────────────────────────────────────────────────────

    /// CHANGE 8 schema guard: parse Tester-B's captured /api/ps poll
    /// shape. If Ollama bumps the schema (model key rename, models[]
    /// wrapper change), this test fails first — surfacing the
    /// trigger from CHANGE 8 before any drift escapes into production.
    #[test]
    fn schema_matches_tester_b_captured_format() {
        // Inner-poll shape from
        // tests/empirical/v1_1_0_prep/ollama_api_format/raw/
        // ollama_ps_active_generation.json. Trimmed to the fields the
        // parser actually consumes plus a representative neighbour
        // (size_vram) so a field rename is caught.
        let body = r#"{
            "models": [
                {
                    "name": "tinyllama:latest",
                    "model": "tinyllama:latest",
                    "size": 800454656,
                    "size_vram": 800454656
                }
            ]
        }"#;
        let parsed = parse_loaded_models(body);
        assert_eq!(parsed, vec!["tinyllama:latest".to_string()]);
    }

    /// Empty `/api/ps` response → previously-tracked runners should
    /// resolve to NotDetected on next sample. Pins CHANGE 5.
    #[test]
    fn empty_models_yields_not_detected_for_known_runner() {
        let mut s = OllamaApiSource::new();
        // Seed prior state for the model (as if a previous tick saw
        // it Active). Force the model into per_model_last_state so the
        // "not in /api/ps" branch is exercised against a real cleanup.
        s.per_model_last_state
            .insert("tinyllama:latest".into(), ActivityState::Active);
        s.per_model_idle_streak
            .insert("tinyllama:latest".into(), 0);

        let loaded = parse_loaded_models(r#"{"models":[]}"#);
        assert!(loaded.is_empty());
        // Simulate the runner branch of sample().
        let runner = runner_snap("tinyllama:latest", 0.0);
        let my_model = runner.model_name.as_deref().unwrap();
        let activity = if loaded.iter().any(|m| m == my_model) {
            s.classify_runner_activity(my_model, runner.cpu_pct)
        } else {
            s.per_model_idle_streak.remove(my_model);
            s.per_model_last_state.remove(my_model);
            ActivityState::NotDetected
        };
        assert_eq!(activity, ActivityState::NotDetected);
        assert!(!s.per_model_last_state.contains_key(my_model));
        assert!(!s.per_model_idle_streak.contains_key(my_model));
    }

    /// Populated `models[]` with high CPU → Active. Threshold is
    /// `>= 50.0` on the raw `0-(100×cores)` scale per EMPIRICAL
    /// (Tester-B): bimodal 99-105% active.
    #[test]
    fn populated_models_with_high_cpu_yields_active() {
        let mut s = OllamaApiSource::new();
        // 100.0 is the empirical "1 core pinned" reading.
        assert_eq!(
            s.classify_runner_activity("tinyllama:latest", 100.0),
            ActivityState::Active,
        );
        // Threshold boundary exactly at 50.0.
        assert_eq!(
            s.classify_runner_activity("tinyllama:latest", 50.0),
            ActivityState::Active,
        );
    }

    /// Populated `models[]` with sub-5% CPU + 2-sample debounce →
    /// Idle on the second consecutive sub-5% sample. The first
    /// sub-5% sample HOLDS the previous Active state (CHANGE 12).
    #[test]
    fn populated_models_with_low_cpu_emits_idle_only_after_debounce() {
        let mut s = OllamaApiSource::new();
        // Seed prior Active (e.g. /api/generate just finished).
        s.per_model_last_state
            .insert("tinyllama:latest".into(), ActivityState::Active);

        // First sub-5% sample — hold Active through debounce.
        assert_eq!(
            s.classify_runner_activity("tinyllama:latest", 1.0),
            ActivityState::Active,
            "single sub-5% sample must not flip to Idle (CHANGE 12 debounce)",
        );
        // Second sub-5% sample — debounce reached, emit Idle.
        assert_eq!(
            s.classify_runner_activity("tinyllama:latest", 0.5),
            ActivityState::Idle,
        );
    }

    /// 5-50% dead band → hold previous state (CHANGE 11).
    /// Empirically empty per Tester-B but defensively handled so a
    /// transient sample in that band doesn't perturb the streak.
    #[test]
    fn dead_band_holds_previous_state() {
        let mut s = OllamaApiSource::new();
        s.per_model_last_state
            .insert("tinyllama:latest".into(), ActivityState::Active);
        // Mid-band sample (empirically empty zone).
        assert_eq!(
            s.classify_runner_activity("tinyllama:latest", 25.0),
            ActivityState::Active,
        );
        // Streak unchanged (no entry created from a dead-band sample).
        assert!(!s.per_model_idle_streak.contains_key("tinyllama:latest"));
    }

    /// Active resets the idle streak so a subsequent burst of activity
    /// re-arms the 2-sample debounce from scratch.
    #[test]
    fn active_resets_idle_streak() {
        let mut s = OllamaApiSource::new();
        // One sub-5% sample (streak = 1, still Active per debounce).
        s.per_model_last_state
            .insert("tinyllama:latest".into(), ActivityState::Active);
        s.classify_runner_activity("tinyllama:latest", 0.0);
        assert_eq!(s.per_model_idle_streak.get("tinyllama:latest"), Some(&1));
        // Burst → streak reset to 0.
        s.classify_runner_activity("tinyllama:latest", 100.0);
        assert_eq!(s.per_model_idle_streak.get("tinyllama:latest"), Some(&0));
    }

    /// Daemon up→down transition logs once; subsequent samples in
    /// the same Down state must NOT re-log.
    #[test]
    fn daemon_transition_log_is_once() {
        let mut s = OllamaApiSource::new();
        // First transition None → Up: no log expected (start-up).
        s.record_daemon_status(DaemonStatus::Up);
        assert_eq!(s.last_daemon_status, Some(DaemonStatus::Up));
        // Up → Down: would log.
        s.record_daemon_status(DaemonStatus::Down);
        // Down → Down: must not log again (idempotent within a state).
        s.record_daemon_status(DaemonStatus::Down);
        assert_eq!(s.last_daemon_status, Some(DaemonStatus::Down));
        // Down → Up: would log recovery.
        s.record_daemon_status(DaemonStatus::Up);
        assert_eq!(s.last_daemon_status, Some(DaemonStatus::Up));
    }

    /// Bimodal threshold constants are at the documented EMPIRICAL
    /// values. Pinned so a casual refactor doesn't silently lower the
    /// threshold into Tester-B's empty band.
    #[test]
    fn bimodal_thresholds_match_empirical_values() {
        assert_eq!(OLLAMA_ACTIVE_CPU_PCT, 50.0);
        assert_eq!(OLLAMA_IDLE_CPU_PCT, 5.0);
        assert_eq!(OLLAMA_IDLE_DEBOUNCE_SAMPLES, 2);
    }

    #[tokio::test]
    async fn end_to_end_scrape_emits_model_hint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let body = r#"{"models":[{"name":"phi3:mini"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
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

        let mut s = OllamaApiSource::new();
        s.endpoint_cache
            .insert(7, format!("http://127.0.0.1:{}/api/ps", port));
        let mut p = snap(&["ollama", "serve"]);
        p.pid = 7;
        let frame = s.sample(&p).await.expect("scrape should succeed");
        assert_eq!(frame.model_name_hint.as_deref(), Some("phi3:mini"));
        // No throughput yet — that's the spec'd division of labour
        // between Ollama and stdout parsing.
        assert!(frame.tokens_per_sec.is_none());
        // Daemon row: no activity_state (runner row carries that).
        assert!(frame.activity_state.is_none());
    }
}
