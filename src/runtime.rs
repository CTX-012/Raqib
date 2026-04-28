use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use thiserror::Error;

use crate::classifier;
use crate::config::Config;
use crate::governor::manual::{AuditLogEntry, KillSource, ManualKillAction};
use crate::governor::{AuditWriter, GovernorExecutor, KillAction, ManualKiller};
use crate::lifecycle::tracker::LifecycleTracker;
use crate::lifecycle::{LifecycleSnapshot, LifecycleSummary};
use crate::model::{AICategory, ClassificationResult};
use crate::platform::{self, GpuSnapshot, PlatformError, PlatformSnapshot};
use crate::storage::{LogStore, RunRecord, RunStore};

/// Errors emitted by the runtime tick loop. Platform errors are fatal;
/// per-process errors are absorbed into tracing logs and the audit trail.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("platform sample failed: {0}")]
    Platform(#[from] PlatformError),
    #[error("lifecycle tracking failed: {0}")]
    Lifecycle(String),
}

/// Per-process annotation paired with its raw classification.
/// UI panels iterate this rather than re-classifying.
#[derive(Debug, Clone)]
pub struct AnnotatedProcess {
    pub pid: u32,
    pub name: String,
    pub category: AICategory,
    pub evidence: String,
    /// Short model name (e.g. "qwen2.5-0.5b-instruct-q8_0") when the
    /// classifier extracted a concrete weight file; None otherwise.
    /// This is the field the Registry renders as its "Model" column.
    pub model_name: Option<String>,
    /// Per-process CPU utilization for the tick, percent of one core.
    /// Zero on the first tick a process appears (no previous sample to delta).
    pub cpu_pct: f32,
    /// Resident set size in megabytes. Converted from /proc RSS bytes.
    pub rss_mb: u64,
    /// Per-process VRAM in bytes, aggregated across GPU devices. None when
    /// NVML didn't report this process or no GPU is present.
    pub vram_bytes: Option<u64>,
}

/// Aggregated state from the most recent tick. Cheap to clone for the UI
/// to render between samples.
#[derive(Debug, Clone, Default)]
pub struct RuntimeState {
    pub last_snapshot: Option<PlatformSnapshot>,
    pub last_lifecycle: Option<LifecycleSnapshot>,
    pub annotated: Vec<AnnotatedProcess>,
    pub decisions: Vec<(u32, KillAction, String)>,
    pub completed: VecDeque<LifecycleSummary>,
    pub audit: VecDeque<AuditLogEntry>,
    pub dry_run: bool,
    pub tick_count: u64,
    pub last_tick: Option<Instant>,
}

impl RuntimeState {
    /// Convenience: only the AI-classified processes, in the order produced
    /// by the platform layer (PID order on Linux).
    pub fn ai_processes(&self) -> impl Iterator<Item = &AnnotatedProcess> {
        self.annotated
            .iter()
            .filter(|p| p.category != AICategory::NotAi)
    }

    pub fn non_ai_processes(&self) -> impl Iterator<Item = &AnnotatedProcess> {
        self.annotated
            .iter()
            .filter(|p| p.category == AICategory::NotAi)
    }
}

/// Owns the per-tick pipeline and the in-memory state shown by the UI.
/// Instantiated once at startup; `tick()` is the only entry point.
pub struct Runtime {
    config: Config,
    tracker: LifecycleTracker,
    governor: GovernorExecutor,
    manual_killer: ManualKiller,
    state: RuntimeState,
    /// Persistent audit-log sink. None when config leaves `audit_log_path`
    /// empty — CI/tests and the `cargo test` runs take that path.
    audit_writer: Option<AuditWriter>,
    /// Persistent run-summary sink; separate file so operators can tail
    /// `summaries.jsonl` without drowning in audit entries.
    summary_store: Option<LogStore>,
    /// Foundation-A typed run store. The history subcommand and the
    /// regression detector both read from here. Writes mirror to
    /// `summary_store` when both are configured, preserving Phase-1
    /// `summaries.jsonl` consumers during the transition.
    run_store: Option<RunStore>,
    /// Previous tick's cumulative CPU ticks, per PID, plus the wall-clock
    /// timestamp that reading was taken at. Used to compute cpu_pct as
    /// delta_ticks / CLK_TCK / elapsed_secs × 100.
    prev_cpu: HashMap<u32, (u64, Instant)>,
    /// Cached USER_HZ. Resolved once at startup via sysconf(_SC_CLK_TCK);
    /// falls back to the standard Linux default of 100 if the call fails.
    clk_tck: u64,
}

impl Runtime {
    pub fn new(config: Config) -> Self {
        let policy = config.build_policy();
        let dry_run = !policy.enforce;
        let governor = GovernorExecutor::new(policy);
        let manual_killer = ManualKiller::new(dry_run);
        let state = RuntimeState {
            dry_run,
            ..Default::default()
        };
        let audit_writer = config.runtime.audit_log().and_then(|p| {
            AuditWriter::open(&p)
                .inspect_err(|e| tracing::error!(error = %e, "failed to open audit log; continuing without persistence"))
                .ok()
        });
        let summary_store = config.runtime.summary_log().and_then(|p| {
            LogStore::open(&p)
                .inspect_err(|e| tracing::error!(error = %e, "failed to open summary log; continuing without persistence"))
                .ok()
        });
        let run_store = config.storage.run_store().and_then(|p| {
            RunStore::open(&p)
                .inspect_err(|e| tracing::error!(error = %e, path = %p.display(), "failed to open run store; continuing without persistence"))
                .ok()
        });
        Self {
            config,
            tracker: LifecycleTracker::new(),
            governor,
            manual_killer,
            state,
            audit_writer,
            summary_store,
            run_store,
            prev_cpu: HashMap::new(),
            clk_tck: read_clk_tck(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn state(&self) -> &RuntimeState {
        &self.state
    }

    pub fn dry_run(&self) -> bool {
        self.state.dry_run
    }

    /// Most-recent run records for `model` from the typed run store, or
    /// an empty vec when persistence is disabled. Used by the TUI history
    /// overlay (`h` key) so it can populate without needing direct
    /// access to the store.
    pub fn history(&self, model: &str, limit: usize) -> Vec<crate::storage::RunRecord> {
        match &self.run_store {
            Some(rs) => rs.recent(model, limit),
            None => Vec::new(),
        }
    }

    /// Toggle the `enforce` bit in the live policy. The next tick will use
    /// the new mode. Manual kills follow the same flag.
    pub fn toggle_dry_run(&mut self) {
        let policy = self.governor.policy_mut();
        policy.enforce = !policy.enforce;
        self.state.dry_run = !policy.enforce;
        self.manual_killer.set_dry_run(self.state.dry_run);
        tracing::info!(dry_run = self.state.dry_run, "dry-run mode toggled");
    }

    /// Run one full tick: sample → classify → lifecycle → governor.
    /// Updates `state` and returns it. Errors here are fatal — the loop
    /// owner must decide whether to retry or exit.
    pub fn tick(&mut self) -> Result<&RuntimeState, RuntimeError> {
        let snapshot = platform::collect_snapshot()?;
        let now = Instant::now();
        let vram_by_pid = vram_bytes_by_pid(&snapshot.gpu);

        let mut next_cpu: HashMap<u32, (u64, Instant)> =
            HashMap::with_capacity(snapshot.processes.len());
        let annotated: Vec<AnnotatedProcess> = snapshot
            .processes
            .iter()
            .map(|p| {
                let ClassificationResult {
                    category,
                    evidence,
                    model_name,
                    ..
                } = classifier::classify_process(p);
                let cpu_pct = self.compute_cpu_pct(p.pid, p.cpu_time_ticks, now);
                next_cpu.insert(p.pid, (p.cpu_time_ticks, now));
                AnnotatedProcess {
                    pid: p.pid,
                    name: p.name.clone(),
                    category,
                    evidence,
                    model_name,
                    cpu_pct,
                    rss_mb: p.rss_bytes / (1024 * 1024),
                    vram_bytes: vram_by_pid.get(&p.pid).copied(),
                }
            })
            .collect();
        self.prev_cpu = next_cpu;

        let lifecycle = self
            .tracker
            .update(&snapshot.processes)
            .map_err(|e| RuntimeError::Lifecycle(e.to_string()))?;

        // Fold the tick's per-process resource + model readings into the
        // tracked lifecycle records. The runtime is the only place where
        // classification output (model_name) and runtime metrics (cpu_pct,
        // vram_bytes) are available together, so the tracker is deliberately
        // dumb about these and accepts them here. Stats persist across ticks
        // in `tracker.previous` so the run summary on the exit tick carries
        // the peak/avg computed over the process's entire life.
        for p in &annotated {
            self.tracker
                .record_sample(p.pid, p.cpu_pct, p.rss_mb * 1024 * 1024, p.vram_bytes);
            self.tracker.record_model_name(p.pid, p.model_name.clone());
        }

        // Record run summaries as they fire. Bounded by config to keep memory flat.
        for summary in &lifecycle.recent_exits {
            self.state.completed.push_back(summary.clone());
            while self.state.completed.len() > self.config.runtime.completed_history {
                self.state.completed.pop_front();
            }
            if let Some(s) = &self.summary_store
                && let Err(e) = s.append(summary)
            {
                tracing::warn!(error = %e, "failed to persist run summary");
            }
            // RunStore is query-optimized (latest.md Tier 1.1) — only
            // AI-classified processes get a record. Non-AI exits stay in
            // the legacy `summary_log_path` JSONL when configured, which
            // remains the unfiltered forensic trail.
            if let Some(rs) = &mut self.run_store
                && summary.category.is_some()
            {
                let record = RunRecord::from_summary(summary.clone());
                if let Err(e) = rs.append(record) {
                    tracing::warn!(error = %e, "failed to persist run record");
                }
            }
        }

        let decisions = self.governor.evaluate(&lifecycle);

        self.state.annotated = annotated;
        self.state.decisions = decisions;
        self.state.last_snapshot = Some(snapshot);
        self.state.last_lifecycle = Some(lifecycle);
        self.state.tick_count += 1;
        self.state.last_tick = Some(Instant::now());

        Ok(&self.state)
    }

    /// Manual-kill entry point used by the TUI keybinding. Returns Err if
    /// the PID is gone or the kill failed; success is logged to the audit
    /// trail and surfaced in the UI's audit panel.
    pub fn manual_kill(&mut self, pid: u32, reason: String) -> Result<(), String> {
        let lifecycle = self
            .state
            .last_lifecycle
            .as_ref()
            .ok_or_else(|| "no snapshot available yet".to_string())?;

        let (pid, name, category) =
            ManualKiller::find_by_pid(pid, lifecycle).map_err(|e| e.to_string())?;

        // Allowlisted processes still get killed when the user explicitly
        // confirms; the UI is responsible for the confirm prompt.
        let _is_allowlisted = ManualKiller::is_allowlisted(&name, &self.governor);

        let result = self
            .manual_killer
            .kill_sigterm(pid, name.clone(), category, reason)
            .map_err(|e| e.to_string());

        // Mirror the audit entry into the bounded UI buffer and, when
        // configured, the persistent JSONL trail.
        if let Some(entry) = self.manual_killer.audit_log().entries().last() {
            if let Some(w) = &self.audit_writer
                && let Err(e) = w.append(entry)
            {
                tracing::warn!(error = %e, "failed to persist manual-kill audit entry");
            }
            self.state.audit.push_back(entry.clone());
            while self.state.audit.len() > self.config.runtime.audit_history {
                self.state.audit.pop_front();
            }
        }

        result
    }

    /// Mirror governor (automated) decisions into the audit ring buffer.
    /// Called once per tick by the loop owner so the UI sees them.
    pub fn record_governor_audit(&mut self) {
        for (pid, action, reason) in &self.state.decisions {
            let kill_action = match action {
                KillAction::SignalTermSent | KillAction::DryRunTermWould => {
                    Some(ManualKillAction::SendSigterm)
                }
                KillAction::SignalKillSent | KillAction::DryRunKillWould => {
                    Some(ManualKillAction::SendSigkill)
                }
                _ => None,
            };
            let Some(kill_action) = kill_action else {
                continue;
            };

            let name = self
                .state
                .annotated
                .iter()
                .find(|p| p.pid == *pid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let category = self
                .state
                .annotated
                .iter()
                .find(|p| p.pid == *pid)
                .map(|p| p.category)
                .filter(|c| *c != AICategory::NotAi);

            let entry = AuditLogEntry {
                timestamp: chrono::Utc::now(),
                action: kill_action,
                source: KillSource::Automated,
                pid: *pid,
                process_name: name,
                category,
                reason: reason.clone(),
                success: true,
                error_msg: None,
            };
            if let Some(w) = &self.audit_writer
                && let Err(e) = w.append(&entry)
            {
                tracing::warn!(error = %e, "failed to persist automated audit entry");
            }
            self.state.audit.push_back(entry);
            while self.state.audit.len() > self.config.runtime.audit_history {
                self.state.audit.pop_front();
            }
        }
    }
}

impl Runtime {
    /// Converts a fresh (pid, cumulative_ticks, now) reading into a CPU
    /// percentage by looking up the previous tick's value. Returns 0.0 when
    /// no previous sample exists (first appearance of this PID) or when the
    /// ticks counter went backwards (PID reuse).
    fn compute_cpu_pct(&self, pid: u32, ticks_now: u64, now: Instant) -> f32 {
        let Some(&(ticks_prev, prev_at)) = self.prev_cpu.get(&pid) else {
            return 0.0;
        };
        let dt = now.saturating_duration_since(prev_at).as_secs_f32();
        if dt <= 0.0 || ticks_now < ticks_prev {
            return 0.0;
        }
        let delta_ticks = (ticks_now - ticks_prev) as f32;
        (delta_ticks / self.clk_tck as f32 / dt) * 100.0
    }
}

/// Reads `sysconf(_SC_CLK_TCK)`. Returns 100 on failure — that's the Linux
/// kernel's compiled-in default on every distribution shipped since 2008, so
/// the fallback is strictly better than panicking.
fn read_clk_tck() -> u64 {
    // SAFETY: sysconf is a pure POSIX C function with no preconditions.
    let raw = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if raw > 0 { raw as u64 } else { 100 }
}

/// Folds per-device VRAM maps from `GpuSnapshot` into a flat `pid → bytes` map.
/// NVML's `process.used_gpu_memory` comes back as a string in the current
/// snapshot (a known bug, tracked separately); we parse the "Used(NNN)" form
/// when present and otherwise leave the entry out rather than surface 0.
fn vram_bytes_by_pid(gpu: &GpuSnapshot) -> HashMap<u32, u64> {
    let mut out: HashMap<u32, u64> = HashMap::new();
    for device in &gpu.devices {
        for (&pid, (_name, mem_str)) in &device.per_process_vram {
            if let Some(bytes) = parse_used_gpu_memory_debug(mem_str) {
                *out.entry(pid).or_insert(0) += bytes;
            }
        }
    }
    out
}

/// Extracts the byte count from the Debug form of `nvml_wrapper::enum_wrappers
/// ::device::UsedGpuMemory` — e.g. `"Used(1073741824)"` → 1_073_741_824.
/// Returns None for `"Unavailable"` or malformed inputs.
fn parse_used_gpu_memory_debug(s: &str) -> Option<u64> {
    let start = s.find('(')?;
    let end = s[start..].find(')')? + start;
    s[start + 1..end].trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_starts_in_configured_dry_run() {
        let cfg = Config::default();
        let rt = Runtime::new(cfg);
        assert!(
            rt.dry_run(),
            "default config must initialize runtime in dry-run"
        );
    }

    #[test]
    fn toggle_dry_run_flips_state_and_manual_killer() {
        let cfg = Config::default();
        let mut rt = Runtime::new(cfg);
        assert!(rt.dry_run());
        rt.toggle_dry_run();
        assert!(!rt.dry_run());
        assert!(!rt.manual_killer.is_dry_run());
        rt.toggle_dry_run();
        assert!(rt.dry_run());
        assert!(rt.manual_killer.is_dry_run());
    }

    #[test]
    fn tick_populates_state() {
        let cfg = Config::default();
        let mut rt = Runtime::new(cfg);
        // Platform sampling can fail in restricted CI; tolerate that.
        let Ok(state) = rt.tick() else { return };
        assert!(state.last_snapshot.is_some());
        assert!(state.last_lifecycle.is_some());
        assert!(state.tick_count == 1);
    }

    #[test]
    fn parse_used_gpu_memory_debug_extracts_bytes() {
        assert_eq!(
            parse_used_gpu_memory_debug("Used(1073741824)"),
            Some(1_073_741_824)
        );
        assert_eq!(parse_used_gpu_memory_debug("Used( 42 )"), Some(42));
        assert_eq!(parse_used_gpu_memory_debug("Unavailable"), None);
        assert_eq!(parse_used_gpu_memory_debug("junk"), None);
    }
}
