//! `edge_monitor exec [--name LABEL] -- COMMAND...` (latest.md Tier 1.2d).
//!
//! Forks COMMAND with piped stdio, tees stdout/stderr to the
//! invoking terminal AND to the stdout-regex parser
//! (`telemetry::samplers::stdout_parser`). On child exit, the
//! collected throughput/fps/latency stats are folded into a
//! `RunRecord` and persisted to the configured `RunStore`.
//!
//! The exec wrapper is the only path that captures process stderr
//! today, so this is also where the Tier 3.5 exit classifier's
//! `stderr_lines` input becomes populated. CUDA OOM and CUDA-error
//! detection works on `exec`-launched workloads even when the
//! background-monitor governor wouldn't see them.
//!
//! **Signal forwarding.** SIGINT (Ctrl-C) sent to `edge_monitor exec`
//! is forwarded to the child immediately; on receipt of a second
//! SIGINT we hard-exit so a stuck child can't trap the user.

use std::io::Write;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, anyhow};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::analysis::compare::MeanStd;
use crate::config::Config;
use crate::exit_classify::{ExitContext, classify_exit};
use crate::lifecycle::LifecycleSummary;
use crate::model::AICategory;
use crate::storage::run_store::{RunMetrics, RunRecord, RunStore};
use crate::telemetry::samplers::stdout_parser::{MetricKind, parse_line};

/// How many recent stderr lines to retain for `ExitContext`. Bounded
/// so a chatty child can't OOM us through stderr.
const STDERR_TAIL: usize = 64;

/// Aggregated stats observed during the wrapped run.
#[derive(Debug, Default, Clone)]
struct ExecStats {
    tps_values: Vec<f32>,
    fps_values: Vec<f32>,
    latency_values: Vec<f32>,
    stderr_tail: Vec<String>,
}

impl ExecStats {
    fn record_metric_line(&mut self, line: &str) {
        for m in parse_line(line) {
            match m.kind {
                MetricKind::TokensPerSec => self.tps_values.push(m.value),
                MetricKind::Fps => self.fps_values.push(m.value),
                MetricKind::LatencyMs => self.latency_values.push(m.value),
            }
        }
    }

    fn record_stderr_line(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        self.stderr_tail.push(line.to_string());
        if self.stderr_tail.len() > STDERR_TAIL {
            self.stderr_tail.remove(0);
        }
        // stderr can also carry the metric lines (Ultralytics emits
        // them via the python logger, sometimes on stderr).
        self.record_metric_line(line);
    }

    fn to_run_metrics(&self) -> RunMetrics {
        let mut m = RunMetrics::default();
        if !self.tps_values.is_empty() {
            let n = self.tps_values.len() as f32;
            m.tokens_per_sec_avg = Some(self.tps_values.iter().sum::<f32>() / n);
            m.tokens_per_sec_peak = self
                .tps_values
                .iter()
                .copied()
                .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))));
        }
        if !self.fps_values.is_empty() {
            let n = self.fps_values.len() as f32;
            m.fps_avg = Some(self.fps_values.iter().sum::<f32>() / n);
        }
        if !self.latency_values.is_empty() {
            let n = self.latency_values.len() as f32;
            let mean = self.latency_values.iter().sum::<f32>() / n;
            m.inference_latency_ms_avg = Some(mean);
            // p99 nearest-rank.
            let mut sorted = self.latency_values.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let idx = ((sorted.len() as f32) * 0.99) as usize;
            let idx = idx.min(sorted.len() - 1);
            m.inference_latency_ms_p99 = Some(sorted[idx]);
        }
        let _ = MeanStd {
            // ensure import is used in non-test builds
            mean: 0.0,
            stddev: 0.0,
            n: 0,
        };
        m
    }
}

/// Entry point invoked from `main.rs` when the user types
/// `edge_monitor exec -- COMMAND...`. Runs the command to completion
/// and writes a `RunRecord` reflecting what we observed.
pub async fn run_exec(
    name: Option<String>,
    command: Vec<String>,
    config: &Config,
) -> anyhow::Result<i32> {
    if command.is_empty() {
        return Err(anyhow!("exec requires a command after `--`"));
    }
    let label = name.unwrap_or_else(|| command[0].clone());
    let spawn_time = Utc::now();

    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::inherit());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning child: {:?}", command))?;
    let pid = child.id().unwrap_or(0);
    eprintln!(
        "edge_monitor: launched {:?} as pid {} (label={})",
        command, pid, label
    );

    let stats = Arc::new(Mutex::new(ExecStats::default()));
    let stats_stdout = stats.clone();
    let stats_stderr = stats.clone();

    let stdout = child.stdout.take().context("missing child stdout")?;
    let stderr = child.stderr.take().context("missing child stderr")?;

    // Tee stdout: tokio reader → parse + forward to local stdout.
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            // Forward verbatim to the user's terminal.
            println!("{}", line);
            std::io::stdout().flush().ok();
            // Parse for metrics.
            stats_stdout.lock().await.record_metric_line(&line);
        }
    });

    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            eprintln!("{}", line);
            std::io::stderr().flush().ok();
            stats_stderr.lock().await.record_stderr_line(&line);
        }
    });

    // Forward signals. ctrlc::set_handler can only be installed once
    // process-wide; we use a counter to upgrade SIGINT-twice to a
    // hard exit.
    let interrupt_count = Arc::new(AtomicU32::new(0));
    let ic = interrupt_count.clone();
    let target_pid = pid;
    if let Err(e) = ctrlc::set_handler(move || {
        let n = ic.fetch_add(1, Ordering::SeqCst);
        if n == 0 && target_pid != 0 {
            // Best-effort SIGINT to the child. Errors logged only.
            // SAFETY: kill(2) is a thin syscall; signal value is a
            // compile-time constant.
            unsafe {
                if libc::kill(target_pid as libc::pid_t, libc::SIGINT) != 0 {
                    eprintln!("edge_monitor: failed to forward SIGINT to child");
                }
            }
        } else {
            eprintln!("edge_monitor: hard exit on second Ctrl-C");
            std::process::exit(130);
        }
    }) {
        eprintln!("edge_monitor: signal handler install failed: {}", e);
    }

    let status: ExitStatus = child.wait().await.context("child wait failed")?;
    // Drain readers — they exit when the pipes close.
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let exit_code = status.code();
    let signal: Option<i32> = {
        // On Unix, .signal() is on ExitStatusExt.
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    let exit_time = Utc::now();
    let uptime_secs = (exit_time - spawn_time).num_seconds();

    // Build a synthetic LifecycleSummary that matches what the
    // background runtime would have produced.
    let summary = LifecycleSummary {
        pid,
        name: command[0].clone(),
        category: Some(AICategory::Inference),
        model_name: Some(label),
        spawn_time,
        exit_time,
        uptime_secs,
        exit_code,
        signal,
        avg_cpu_pct: 0.0,
        peak_cpu_pct: 0.0,
        peak_rss_mb: 0,
        peak_vram_mb: 0,
        samples: 0,
    };

    let s = stats.lock().await.clone();
    let mut record = RunRecord::from_summary(summary.clone());
    record.metrics = s.to_run_metrics();

    // Tier 3.5 exit reason — exec is the only path with stderr
    // capture, so feed the tail through `classify_exit`.
    let ctx = ExitContext {
        dmesg_lines: Vec::new(),
        stderr_lines: s.stderr_tail.clone(),
        killed_by_governor: false,
        governor_reason: None,
    };
    record.exit_reason = classify_exit(&summary, &ctx);

    // Persist to RunStore.
    if let Some(path) = config.storage.run_store() {
        match RunStore::open(&path) {
            Ok(mut store) => {
                if let Err(e) = store.append(record) {
                    eprintln!("edge_monitor: failed to persist run record: {}", e);
                }
            }
            Err(e) => eprintln!("edge_monitor: opening run store failed: {}", e),
        }
    }

    eprintln!(
        "edge_monitor: child exited (exit_code={:?}, signal={:?})",
        exit_code, signal
    );
    Ok(exit_code.unwrap_or_else(|| signal.map(|s| 128 + s).unwrap_or(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_tracks_tps_average_and_peak() {
        let mut s = ExecStats::default();
        s.record_metric_line(
            "llama_print_timings:        eval time =    1234.56 ms /   140 runs   (    8.81 ms per token,   100.00 tokens per second)",
        );
        s.record_metric_line(
            "llama_print_timings:        eval time =    1234.56 ms /   140 runs   (    8.81 ms per token,   60.00 tokens per second)",
        );
        let m = s.to_run_metrics();
        assert!((m.tokens_per_sec_avg.unwrap() - 80.0).abs() < 1e-3);
        assert!((m.tokens_per_sec_peak.unwrap() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn stats_tracks_ultralytics_fps_and_latency() {
        let mut s = ExecStats::default();
        s.record_metric_line("Speed: 1.0ms preprocess, 9.0ms inference, 0.0ms postprocess");
        let m = s.to_run_metrics();
        assert!((m.fps_avg.unwrap() - 100.0).abs() < 1e-2);
        assert!((m.inference_latency_ms_avg.unwrap() - 9.0).abs() < 1e-3);
    }

    #[test]
    fn stderr_tail_is_capped() {
        let mut s = ExecStats::default();
        for i in 0..(STDERR_TAIL + 50) {
            s.record_stderr_line(&format!("line {}", i));
        }
        assert!(s.stderr_tail.len() <= STDERR_TAIL);
        // Most recent line should be present.
        let last = format!("line {}", STDERR_TAIL + 49);
        assert!(s.stderr_tail.iter().any(|l| l == &last));
    }

    #[tokio::test]
    async fn exec_with_empty_command_errors() {
        let cfg = Config::default();
        let r = run_exec(None, vec![], &cfg).await;
        assert!(r.is_err());
    }
}
