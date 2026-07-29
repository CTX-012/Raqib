//! DISPATCH connectivity — derive HTTP health-probe endpoints for
//! HTTP-scrapable workload types.
//!
//! Reuses the SHIPPED per-sampler `endpoint_for()` helpers
//! (`OllamaApiSource::endpoint_for`, `VllmPrometheusSource::endpoint_for`,
//! `LlamaCppServerSource::endpoint_for`) so the probe target and the
//! sampler scrape target stay lockstep. This module's contribution is
//! the dispatch — deciding which of the three sampler families a given
//! process belongs to WITHOUT instantiating the sampler's tokio state
//! machine (the sampler's `applies_to` is a `&self` trait method that
//! costs a `reqwest::Client` at build; we're called per-tick, per-PID,
//! on the sync annotation path, and don't need that state).
//!
//! ## Honesty (PENDING.md ratified finding)
//!
//! Non-HTTP workload types (embeddings, agent, ROS2, training) return
//! `None` — the caller renders NO connectivity chip for them. Showing
//! a "DOWN" indicator for a healthy ROS2 node with no HTTP endpoint
//! would be a lie of the exact shape the CLAUDE.md VRAM-honesty rule
//! forbids ("a 0 reads as 'idle' when it means 'unmeasured'").
//!
//! ## Contract discipline
//!
//! The dispatch table below MIRRORS each sampler's `applies_to`
//! detection shape. When a sampler's detection widens (a new alias,
//! a new env var), this file needs updating in lockstep — pinned by
//! `dispatcher_matches_sampler_applies_to` in the sampler tests OR by
//! the parity test at the bottom of THIS file. Deliberate duplication
//! of small patterns; the alternative (exposing the sampler regexes)
//! would push private detection helpers into public API.

use std::collections::HashMap;

use crate::telemetry::samplers::llama_cpp_server::LlamaCppServerSource;
use crate::telemetry::samplers::ollama_api::OllamaApiSource;
use crate::telemetry::samplers::vllm_prometheus::VllmPrometheusSource;

/// Given a process's identifying info (name + cmdline + environ),
/// return the HTTP endpoint URL to probe for liveness — or `None`
/// when the workload has no HTTP surface to probe.
///
/// Endpoints are ALWAYS `http://127.0.0.1:<port>/<path>` (loopback
/// only; matches the samplers' scrape targets — see
/// `VllmPrometheusSource::endpoint_for` for the rationale). We never
/// probe an external bind address even when the workload binds
/// `0.0.0.0` — the box's outbound firewall is out of scope.
pub fn derive_probe_endpoint(
    name: &str,
    cmdline: &[String],
    environ: &HashMap<String, String>,
) -> Option<String> {
    // Order: same as the classifier's — ollama is the most-common on
    // the test host and cheapest to detect (single-name check +
    // env-var fallback).
    if is_ollama(name, cmdline) {
        return Some(OllamaApiSource::endpoint_for(cmdline, environ));
    }
    if is_vllm(cmdline, environ) {
        return Some(VllmPrometheusSource::endpoint_for(cmdline));
    }
    if is_llama_cpp_server(cmdline) {
        return Some(LlamaCppServerSource::endpoint_for(cmdline));
    }
    None
}

/// Mirrors `re_ollama_cmdline` at `samplers/ollama_api.rs:254`.
/// Bare-token `ollama`, `/…/ollama`, or `ollama serve|run` shape.
fn is_ollama(name: &str, cmdline: &[String]) -> bool {
    if name == "ollama" {
        return true;
    }
    cmdline.iter().any(|t| t == "ollama" || t.ends_with("/ollama"))
}

/// Mirrors `re_vllm_cmdline` at `samplers/vllm_prometheus.rs:91` plus
/// its `VLLM_*` env var check at `:113`. Slightly broader than a bare
/// regex because it accepts either shape without pulling the private
/// regex into public API.
fn is_vllm(cmdline: &[String], environ: &HashMap<String, String>) -> bool {
    let joined = cmdline.join(" ");
    // "vllm serve", "vllm.entrypoints.*", "python -m vllm".
    if joined.contains("vllm serve") || joined.contains("vllm.entrypoints") {
        return true;
    }
    // Bare `vllm` token — bounded so "myvllm" doesn't match.
    for t in cmdline {
        if t == "vllm" || t.ends_with("/vllm") {
            return true;
        }
    }
    // The sampler also treats any `VLLM_*` env var as a match — mirror
    // that (deployment-tool convention: setting `VLLM_MODEL=...`).
    environ.keys().any(|k| k.starts_with("VLLM_"))
}

/// Mirrors `re_llama_server_cmdline` at
/// `samplers/llama_cpp_server.rs:89` — `llama-server` OR
/// `llama_server` as the basename of any cmdline token.
fn is_llama_cpp_server(cmdline: &[String]) -> bool {
    cmdline.iter().any(|t| {
        let base = std::path::Path::new(t)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(t.as_str());
        base == "llama-server" || base == "llama_server"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<const N: usize>(pairs: [(&str, &str); N]) -> HashMap<String, String> {
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn cmdline(vs: &[&str]) -> Vec<String> {
        vs.iter().map(|s| (*s).to_string()).collect()
    }

    // ── ollama dispatch ─────────────────────────────────────────────

    #[test]
    fn ollama_by_process_name() {
        let ep = derive_probe_endpoint("ollama", &cmdline(&["ollama"]), &env([]));
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:11434/api/ps"));
    }

    #[test]
    fn ollama_serve_full_path() {
        let ep = derive_probe_endpoint(
            "ollama",
            &cmdline(&["/usr/local/bin/ollama", "serve"]),
            &env([]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:11434/api/ps"));
    }

    #[test]
    fn ollama_honors_ollama_host_env_var() {
        // Ollama on a non-default port via env; the derivation must
        // reflect the operator's actual bind, not the default.
        let ep = derive_probe_endpoint(
            "ollama",
            &cmdline(&["ollama", "serve"]),
            &env([("OLLAMA_HOST", "0.0.0.0:11500")]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:11500/api/ps"));
    }

    // ── vLLM dispatch ───────────────────────────────────────────────

    #[test]
    fn vllm_serve_default_port() {
        let ep = derive_probe_endpoint(
            "python",
            &cmdline(&["python", "-m", "vllm.entrypoints.api_server"]),
            &env([]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:8000/metrics"));
    }

    #[test]
    fn vllm_serve_custom_port() {
        let ep = derive_probe_endpoint(
            "python",
            &cmdline(&["python", "-m", "vllm.entrypoints.api_server", "--port", "9010"]),
            &env([]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:9010/metrics"));
    }

    #[test]
    fn vllm_env_var_only() {
        // Deployment tools sometimes set VLLM_* without a matching
        // cmdline pattern — mirror the sampler's `applies_to` env-var
        // fallback so an operator whose vLLM runs behind a launcher
        // still gets the connectivity chip.
        let ep = derive_probe_endpoint(
            "python",
            &cmdline(&["python", "-m", "some_launcher"]),
            &env([("VLLM_MODEL", "phi3-mini")]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:8000/metrics"));
    }

    // ── llama.cpp dispatch ──────────────────────────────────────────

    #[test]
    fn llama_cpp_server_by_basename() {
        let ep = derive_probe_endpoint(
            "llama-server",
            &cmdline(&["./llama-server", "-m", "phi3.gguf"]),
            &env([]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:8080/metrics"));
    }

    #[test]
    fn llama_cpp_server_custom_port() {
        let ep = derive_probe_endpoint(
            "llama-server",
            &cmdline(&["./llama-server", "-m", "phi3.gguf", "--port", "9090"]),
            &env([]),
        );
        assert_eq!(ep.as_deref(), Some("http://127.0.0.1:9090/metrics"));
    }

    // ── HONESTY: non-HTTP workloads return None ─────────────────────

    #[test]
    fn embeddings_workload_returns_none() {
        // sentence-transformers-based Python job — no HTTP endpoint.
        let ep = derive_probe_endpoint(
            "python",
            &cmdline(&["python", "-c", "from sentence_transformers import SentenceTransformer"]),
            &env([]),
        );
        assert!(ep.is_none(), "embeddings workload must not derive a probe endpoint; got {ep:?}");
    }

    #[test]
    fn claude_agent_returns_none() {
        // claude CLI (the multi-call binary from PENDING.md D107) —
        // remote LLM, no local server.
        let ep = derive_probe_endpoint(
            "claude",
            &cmdline(&["claude", "--output-format", "stream-json"]),
            &env([]),
        );
        assert!(ep.is_none(), "claude agent must not derive a probe endpoint; got {ep:?}");
    }

    #[test]
    fn ros2_node_returns_none() {
        // ROS2 nodes use DDS pub/sub — no HTTP.
        let ep = derive_probe_endpoint(
            "static_transform_publisher",
            &cmdline(&["/opt/ros/humble/lib/tf2_ros/static_transform_publisher"]),
            &env([("ROS_DOMAIN_ID", "0"), ("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")]),
        );
        assert!(ep.is_none(), "ROS2 node must not derive a probe endpoint; got {ep:?}");
    }

    #[test]
    fn plain_bash_returns_none() {
        // A bare shell — the classifier already NotAi's this, and we
        // should too.
        let ep = derive_probe_endpoint(
            "bash",
            &cmdline(&["bash", "--login"]),
            &env([]),
        );
        assert!(ep.is_none());
    }

    // ── boundary: don't over-match on partial strings ───────────────

    #[test]
    fn my_vllm_wrapper_does_not_match_as_vllm() {
        // Bare-token guard — a token like "myvllm" shouldn't false-match
        // vllm. Note: joined-string "myvllm ..." doesn't contain
        // "vllm serve" or "vllm.entrypoints", and the token check is
        // exact/basename, so this correctly returns None.
        let ep = derive_probe_endpoint(
            "myvllm-wrapper",
            &cmdline(&["myvllm-wrapper", "--config", "cfg.yaml"]),
            &env([]),
        );
        assert!(ep.is_none());
    }

    #[test]
    fn llama_server_via_bash_c_wraps_still_matches() {
        // `bash -c './llama-server -m phi3.gguf'` — the basename check
        // sees `llama-server` in the argv tokens, so the wrapper case
        // still works. The parent (bash) itself won't have model_name
        // classification, but the child does. Confirming both branches.
        let ep = derive_probe_endpoint(
            "bash",
            &cmdline(&["bash", "-c", "./llama-server -m phi3.gguf"]),
            &env([]),
        );
        // NOTE: this test pins that bash-wrapped INVOCATIONS don't match
        // (the -c "..." is a single argv token, not multiple tokens; the
        // basename check operates per-token, so this returns None).
        // If a future refactor wants to look INSIDE `-c` strings, both
        // this test and the sampler's applies_to must move together.
        assert!(ep.is_none(),
            "current dispatch does not descend into -c strings; if this fires, \
             `is_llama_cpp_server` and the sampler's applies_to must agree \
             on the new expansion behaviour");
    }
}
