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

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;

use crate::telemetry::source::{
    ProcessSnapshot, SourceError, SourceResult, TelemetryFrame, TelemetrySource,
};

const DEFAULT_PORT: u16 = 11434;
const SCRAPE_TIMEOUT: Duration = Duration::from_millis(500);

pub struct OllamaApiSource {
    client: reqwest::Client,
    /// pid → endpoint URL. Same single-cache shape as the other HTTP
    /// samplers; no per-PID identity needed because Ollama runs as
    /// one daemon per host typically.
    endpoint_cache: HashMap<u32, String>,
}

impl OllamaApiSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(SCRAPE_TIMEOUT)
            .build()
            .expect("default reqwest client must build");
        Self {
            client,
            endpoint_cache: HashMap::new(),
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

        let model_name = parse_loaded_model(&body);
        Ok(TelemetryFrame {
            pid: proc.pid,
            model_name_hint: model_name,
            ..TelemetryFrame::new(proc.pid)
        })
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
    let parsed: PsResponse = serde_json::from_str(body).ok()?;
    parsed.models.first().and_then(|m| {
        if m.name.is_empty() {
            None
        } else {
            Some(m.name.clone())
        }
    })
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
    }
}
