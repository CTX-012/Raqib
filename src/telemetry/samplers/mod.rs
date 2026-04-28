//! Concrete `TelemetrySource` implementations and parsing utilities.
//!
//! Tier 1.2 of latest.md. Currently ships:
//!
//! * [`stdout_parser`] — pure regex parser over runtime log lines.
//!   Used by the eventual `edge_monitor exec` wrapper to extract
//!   tokens/sec, fps, latency from stdout/stderr without intercepting
//!   the network. Public surface: [`stdout_parser::parse_line`].
//!
//! Future sub-modules:
//!
//! * `vllm_prometheus` — Prom `/metrics` scrape (1.2a).
//! * `llama_cpp_server` — same shape, llama.cpp metrics names (1.2b).
//! * `ollama_api` — `/api/ps` for model identification (1.2c).

pub mod stdout_parser;
