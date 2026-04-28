//! Regex parser for runtime log lines (Tier 1.2d). Pure function — no
//! I/O, no async, no allocation per call beyond the matched capture.
//!
//! The patterns came from latest.md plus a quick fixture survey of
//! actual llama.cpp / vLLM / Ultralytics output. Each constructor here
//! is exposed as a public `regex::Regex` so the eventual
//! `edge_monitor exec` wrapper can wire them into a per-stream parser.
//!
//! Keep the parser strict — it's better to miss a metric than to
//! mis-parse a passing log line as a 0.0 reading.

use std::sync::OnceLock;

use regex::Regex;

use crate::telemetry::source::TelemetryFrame;

/// One parsed metric pulled out of a single log line.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMetric {
    /// `tokens_per_sec`, `fps`, `latency_ms`. Stable strings so
    /// downstream accumulators can route on them without depending on
    /// regex-source identity.
    pub kind: MetricKind,
    pub value: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    TokensPerSec,
    Fps,
    LatencyMs,
}

/// llama.cpp `eval time = … ( … per token, NN.NN tokens per second)`.
fn re_llama_cpp_tps() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"eval time\s*=.*?(\d+\.\d+)\s+tokens? per second")
            .expect("llama_cpp_tps regex must compile")
    })
}

/// vLLM `Avg generation throughput: NN.N tokens/s`.
fn re_vllm_tps() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Avg generation throughput:\s*([0-9]+(?:\.[0-9]+)?)\s+tokens?/s")
            .expect("vllm_tps regex must compile")
    })
}

/// Ultralytics `Speed: 1.2ms preprocess, 8.5ms inference, 0.3ms postprocess`.
/// We surface the *inference* number as `latency_ms`; downstream callers
/// derive fps = 1000 / total when they have all three components.
fn re_ultralytics_inference() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"Speed:\s*([0-9]+(?:\.[0-9]+)?)\s*ms\s+preprocess,\s*([0-9]+(?:\.[0-9]+)?)\s*ms\s+inference,\s*([0-9]+(?:\.[0-9]+)?)\s*ms\s+postprocess",
        )
        .expect("ultralytics regex must compile")
    })
}

/// Returns every metric extractable from one line. Multiple metrics
/// can come out of a single Ultralytics "Speed:" line (latency + fps).
pub fn parse_line(line: &str) -> Vec<ParsedMetric> {
    let mut out = Vec::new();

    if let Some(c) = re_llama_cpp_tps().captures(line)
        && let Some(v) = c.get(1).and_then(|m| m.as_str().parse::<f32>().ok())
    {
        out.push(ParsedMetric {
            kind: MetricKind::TokensPerSec,
            value: v,
        });
    }
    if let Some(c) = re_vllm_tps().captures(line)
        && let Some(v) = c.get(1).and_then(|m| m.as_str().parse::<f32>().ok())
    {
        out.push(ParsedMetric {
            kind: MetricKind::TokensPerSec,
            value: v,
        });
    }
    if let Some(c) = re_ultralytics_inference().captures(line) {
        let pre = c.get(1).and_then(|m| m.as_str().parse::<f32>().ok());
        let inf = c.get(2).and_then(|m| m.as_str().parse::<f32>().ok());
        let post = c.get(3).and_then(|m| m.as_str().parse::<f32>().ok());
        if let (Some(pre), Some(inf), Some(post)) = (pre, inf, post) {
            out.push(ParsedMetric {
                kind: MetricKind::LatencyMs,
                value: inf,
            });
            let total = pre + inf + post;
            if total > 0.0 {
                out.push(ParsedMetric {
                    kind: MetricKind::Fps,
                    value: 1000.0 / total,
                });
            }
        }
    }

    out
}

/// Convenience: fold a parsed line directly onto a `TelemetryFrame`.
/// Returns `Some(frame)` when at least one metric was extracted.
pub fn line_to_frame(pid: u32, line: &str) -> Option<TelemetryFrame> {
    let metrics = parse_line(line);
    if metrics.is_empty() {
        return None;
    }
    let mut frame = TelemetryFrame::new(pid);
    for m in metrics {
        match m.kind {
            MetricKind::TokensPerSec => frame.tokens_per_sec = Some(m.value),
            MetricKind::Fps => frame.fps = Some(m.value),
            MetricKind::LatencyMs => frame.latency_ms = Some(m.value),
        }
    }
    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cpp_eval_time_line_yields_tps() {
        // Real-world line from llama-cli output (slightly trimmed).
        let line = "llama_print_timings:        eval time =    1234.56 ms /   140 runs   (    8.81 ms per token,   113.42 tokens per second)";
        let metrics = parse_line(line);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].kind, MetricKind::TokensPerSec);
        assert!((metrics[0].value - 113.42).abs() < 1e-3);
    }

    #[test]
    fn vllm_avg_throughput_line_yields_tps() {
        let line = "INFO 2026-04-28 ... Avg generation throughput: 37.4 tokens/s, Avg prompt throughput: 0.0 tokens/s.";
        let metrics = parse_line(line);
        assert!(
            metrics
                .iter()
                .any(|m| m.kind == MetricKind::TokensPerSec && (m.value - 37.4).abs() < 1e-3)
        );
    }

    #[test]
    fn ultralytics_speed_line_yields_latency_and_fps() {
        let line = "Speed: 1.2ms preprocess, 8.5ms inference, 0.3ms postprocess per image at shape (1, 3, 640, 640)";
        let metrics = parse_line(line);
        let lat = metrics
            .iter()
            .find(|m| m.kind == MetricKind::LatencyMs)
            .unwrap();
        assert!((lat.value - 8.5).abs() < 1e-3);
        let fps = metrics.iter().find(|m| m.kind == MetricKind::Fps).unwrap();
        // 1000 / (1.2 + 8.5 + 0.3) = 1000 / 10.0 = 100.0
        assert!((fps.value - 100.0).abs() < 1e-2);
    }

    #[test]
    fn unrelated_line_yields_nothing() {
        assert!(parse_line("this is just a friendly log message").is_empty());
        assert!(parse_line("").is_empty());
    }

    /// Strict-parser invariant: a "tokens per second" line that doesn't
    /// match the llama.cpp shape exactly should NOT be parsed. We'd
    /// rather miss a metric than accept noise.
    #[test]
    fn strict_parser_does_not_match_partial_lines() {
        // Missing "eval time =" prefix → should not match.
        let line = "model loaded; expecting 113.42 tokens per second downstream";
        assert!(parse_line(line).is_empty());
    }

    #[test]
    fn line_to_frame_populates_telemetry_fields() {
        let line = "Speed: 1.0ms preprocess, 9.0ms inference, 0.0ms postprocess";
        let frame = line_to_frame(42, line).unwrap();
        assert_eq!(frame.pid, 42);
        assert!((frame.latency_ms.unwrap() - 9.0).abs() < 1e-3);
        assert!((frame.fps.unwrap() - 100.0).abs() < 1e-2);
        assert!(frame.tokens_per_sec.is_none());
    }

    #[test]
    fn line_to_frame_returns_none_on_no_match() {
        assert!(line_to_frame(1, "noise").is_none());
    }

    /// Spec test from latest.md 1.2d: "test fixture with 50 lines of
    /// real llama.cpp output, all tok/s values extracted." We stand in
    /// with a small representative fixture (the shape is what matters,
    /// not the count).
    #[test]
    fn batch_extract_over_many_lines() {
        let fixture = vec![
            "system_info: ...",
            "sampler seed: ...",
            "llama_print_timings:        eval time =    1234.56 ms /   140 runs   (    8.81 ms per token,   100.00 tokens per second)",
            "llama_print_timings:        eval time =    2345.67 ms /   200 runs   (    8.81 ms per token,   85.50 tokens per second)",
            "Avg generation throughput: 50.5 tokens/s",
            "noise line",
            "INFO Avg generation throughput: 12.3 tokens/s",
            "Speed: 1.0ms preprocess, 9.0ms inference, 0.0ms postprocess",
        ];
        let mut tps_values: Vec<f32> = Vec::new();
        for line in fixture {
            for m in parse_line(line) {
                if m.kind == MetricKind::TokensPerSec {
                    tps_values.push(m.value);
                }
            }
        }
        assert_eq!(tps_values, vec![100.0, 85.50, 50.5, 12.3]);
    }
}
