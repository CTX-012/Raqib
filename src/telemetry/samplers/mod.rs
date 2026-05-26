//! Concrete `TelemetrySource` implementations and parsing utilities.
//!
//! Tier 1.2 of latest.md. Currently ships:
//!
//! * [`stdout_parser`] — pure regex parser over runtime log lines.
//!   Used by the eventual `edge_monitor exec` wrapper to extract
//!   tokens/sec, fps, latency from stdout/stderr without intercepting
//!   the network. Public surface: [`stdout_parser::parse_line`].
//!
//! * [`vllm_prometheus`] — vLLM `/metrics` scrape (1.2a).
//! * [`llama_cpp_server`] — llama.cpp server `/metrics` scrape (1.2b).
//! * [`ollama_api`] — Ollama `/api/ps` for model identification (1.2c).
//! * [`agent_claude`] — v1.1.0 B2 activity sampler for the claude
//!   agent CLI; uses `sample_with_context` to count Bash-tool
//!   children as the activity signal.
//! * [`ros2_shellout`] — ROS2 topic-rate via `ros2 topic hz` shellout
//!   (Phase 2 / DISPATCH 2B / B3). Maps publication rate to
//!   `ActivityState` per AI-classified ROS2 process.
//! * [`embeddings_cpu`] — Embeddings-workload activity via
//!   sustained-CPU heuristic (Phase 2 / DISPATCH 2B / B4). Pure
//!   compute, no new I/O.

pub mod agent_claude;
pub mod embeddings_cpu;
pub mod llama_cpp_server;
pub mod ollama_api;
pub mod ros2_shellout;
pub mod stdout_parser;
pub mod vllm_prometheus;
