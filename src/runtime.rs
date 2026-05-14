use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use thiserror::Error;

use std::path::PathBuf;

use crate::analysis::compare::{RegressionConfig as DetectorConfig, detect_regressions_with};
use crate::classifier;
use crate::config::{Config, expand_tilde};
use crate::exit_classify::{ExitContext, classify_exit, read_recent_kernel_log};
use crate::fingerprint::Fingerprinter;
use crate::governor::manual::{AuditLogEntry, KillSource, ManualKillAction};
use crate::governor::{AuditWriter, GovernorExecutor, KillAction, ManualKiller};
use crate::lifecycle::tracker::LifecycleTracker;
use crate::lifecycle::{LifecycleSnapshot, LifecycleSummary};
use crate::model::{AICategory, ClassificationResult, WorkloadCategory};
use crate::platform::{self, GpuSnapshot, PlatformError, PlatformSnapshot};
use crate::storage::{LogStore, RunRecord, RunStore};
use crate::telemetry::samplers::{
    llama_cpp_server::LlamaCppServerSource, ollama_api::OllamaApiSource,
    vllm_prometheus::VllmPrometheusSource,
};
use crate::telemetry::source::ProcessSnapshot as TelemetryProcessSnapshot;
use crate::telemetry::{Dispatcher, TelemetrySource};

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
    /// L11a / UX_CONTRACT.md §1 region 4 — contract-aligned workload
    /// type for grouping in the workloads panel and per-row metric
    /// formatting (§2). Sits alongside `category` (which classifies
    /// workflow phase); the two enums measure different axes and are
    /// kept side-by-side. See `model::WorkloadCategory` for the
    /// rationale.
    pub workload_category: WorkloadCategory,
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
    /// L11b — first tick at which this PID was observed by the
    /// runtime. Drives `WorkloadStatus::Loading` per UX_CONTRACT.md
    /// §3: workloads with `(now - first_observed_at) <
    /// BASELINE_WARMUP_SECS` render as Loading regardless of current
    /// metric values. Distinct from OS-level spawn time
    /// (`LifecycleSummary::spawn_time`); a process that was already
    /// running before edge_monitor started has a young
    /// `first_observed_at` even though it has a long process age.
    pub first_observed_at: Instant,
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
    /// Recent regression alerts (Tier 1.3). Bounded by
    /// `runtime.audit_history` so it doesn't grow unbounded.
    pub regressions: VecDeque<crate::analysis::RegressionEvent>,
    /// Tier 3.3 — per-PID live telemetry for the registry panel. Refreshed
    /// each tick from the dispatcher's accumulator. Keyed by PID; entries
    /// only present for AI processes the dispatcher has sampled at least
    /// once. Empty when telemetry is disabled.
    pub live_telemetry: HashMap<u32, LiveTelemetry>,
    /// L8 / UX_CONTRACT.md §4 — exit-driven alert events queued by
    /// the lifecycle exit hook. Drained by the UI loop after each
    /// tick (`Runtime::drain_exit_alerts`) and dispatched to
    /// `App::observe_exit`. Cleared between drains so unbounded
    /// growth is impossible if the UI loop keeps up.
    pub pending_exit_alerts: Vec<ExitAlertEvent>,
    pub dry_run: bool,
    pub tick_count: u64,
    pub last_tick: Option<Instant>,
}

/// L8 — one queued exit-driven alert. Emitted by the lifecycle exit
/// hook in `Runtime::tick` when `classify_for_alert` returns
/// `Some((alert_id, reason))` for the workload's classified
/// `ExitReason`. The UI side translates this into an
/// `AlertState::observe_exit` call.
#[derive(Debug, Clone)]
pub struct ExitAlertEvent {
    pub pid: u32,
    pub workload_name: String,
    pub alert_id: ux_contract::AlertId,
    /// Reason string for `{reason}` substitution in the
    /// `WorkloadExited` template. `None` for `OomDetected` (its
    /// template has no `{reason}` placeholder).
    pub reason: Option<String>,
}

/// Map a classified [`ExitReason`] to the §4 alert it should fire,
/// if any. Returns `None` for `CleanExit` (per §4 "never on clean
/// (code 0) exits"). `OutOfMemory` resolves to `OomDetected`;
/// everything else non-clean resolves to `WorkloadExited` with a
/// human-readable reason for `{reason}` substitution.
///
/// `OomDetected` and `WorkloadExited` are disjoint — a single
/// `ExitReason` produces exactly one alert (or none for clean
/// exits), so the L8 "OomDetected supersedes WorkloadExited for OOM
/// class" rule is satisfied by construction rather than by an
/// explicit precedence check.
pub fn classify_for_alert(
    exit_reason: &crate::storage::run_store::ExitReason,
) -> Option<(ux_contract::AlertId, Option<String>)> {
    use crate::storage::run_store::ExitReason;
    use ux_contract::AlertId;
    match exit_reason {
        ExitReason::CleanExit => None,
        ExitReason::OutOfMemory { .. } => Some((AlertId::OomDetected, None)),
        ExitReason::CudaError { last_msg } => Some((
            AlertId::WorkloadExited,
            Some(
                last_msg
                    .clone()
                    .unwrap_or_else(|| "CUDA error".to_string()),
            ),
        )),
        ExitReason::Segfault => Some((AlertId::WorkloadExited, Some("segfault".into()))),
        ExitReason::GovernorKill { reason } => Some((
            AlertId::WorkloadExited,
            Some(format!("killed by governor ({reason})")),
        )),
        ExitReason::Crash { exit_code } => Some((
            AlertId::WorkloadExited,
            Some(format!("exit code {exit_code}")),
        )),
        ExitReason::UserSignal { signal } => Some((
            AlertId::WorkloadExited,
            Some(format!("signal {signal}")),
        )),
        ExitReason::Unknown => Some((AlertId::WorkloadExited, Some("unknown".into()))),
    }
}

/// Tier 3.3 — what the UI knows *right now* about a single PID's
/// telemetry. Computed each tick from the dispatcher; not persisted.
/// Kept narrow on purpose — full `RunMetrics` is ~20 fields and most
/// of them aren't useful in a live panel.
#[derive(Debug, Clone, Default)]
pub struct LiveTelemetry {
    /// Peak KV-cache occupancy seen so far this run, percent (0..=100).
    pub kv_cache_peak_pct: Option<f32>,
    /// Eviction-counter delta so far this run.
    pub kv_cache_evictions_total: Option<u64>,
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

/// L19 / UX_CONTRACT.md §5 — transient per-PID stderr buffer feeding
/// the post-mortem card. Lines accumulate while the process is live;
/// once `mark_exit` is called the buffer enters read-only state and
/// auto-prunes after [`StderrBuffer::EXPIRY`] (30 s) per "stderr is
/// ephemeral" (the privacy stance documented at the top of
/// `src/storage/run_store.rs`).
///
/// Bounded by [`StderrBuffer::MAX_LINES`] × [`StderrBuffer::MAX_LINE_BYTES`]
/// so a chatty workload cannot OOM the monitor through stderr.
#[derive(Debug, Default, Clone)]
pub struct StderrBuffer {
    /// Captured stderr lines, oldest at front. Bounded ring (drop-oldest
    /// when over `MAX_LINES`).
    lines: VecDeque<String>,
    /// `None` while the PID is still live; `Some(t)` once `mark_exit`
    /// fires. The buffer is considered expired once `EXPIRY` elapses
    /// past `t`.
    exit_at: Option<Instant>,
}

impl StderrBuffer {
    /// Cap on the number of retained lines. Matches the exec-wrapper's
    /// `STDERR_TAIL` so the wrapper-side capture and the runtime-side
    /// buffer agree on "tail length".
    pub const MAX_LINES: usize = 64;
    /// Cap on the per-line byte length. Lines longer than this are
    /// truncated at the nearest UTF-8 boundary ≤ this length.
    pub const MAX_LINE_BYTES: usize = 1024;
    /// Window during which the buffer remains queryable after the
    /// process exits. Matches the post-mortem card's auto-dismiss
    /// (`PostMortemCard::WINDOW`) so the two lifetimes converge.
    pub const EXPIRY: Duration = Duration::from_secs(30);

    fn push_line(&mut self, line: &str) {
        // Trim to MAX_LINE_BYTES on a char boundary so we never store
        // an invalid UTF-8 prefix.
        let clipped: String = if line.len() <= Self::MAX_LINE_BYTES {
            line.to_string()
        } else {
            let mut end = Self::MAX_LINE_BYTES;
            while end > 0 && !line.is_char_boundary(end) {
                end -= 1;
            }
            line[..end].to_string()
        };
        self.lines.push_back(clipped);
        while self.lines.len() > Self::MAX_LINES {
            self.lines.pop_front();
        }
    }

    fn is_expired_at(&self, now: Instant) -> bool {
        self.exit_at
            .is_some_and(|t| now.saturating_duration_since(t) >= Self::EXPIRY)
    }

    /// Read the captured tail as a fresh `Vec<String>` (oldest first).
    /// Returns an empty vec when the buffer has expired — callers should
    /// not need to distinguish "no entry at all" from "entry-but-empty";
    /// either case omits the stderr block on the post-mortem card.
    pub fn tail(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
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
    /// Tier 1.2 — telemetry dispatcher. Owns a Tokio runtime + the
    /// vLLM / llama.cpp / Ollama scrapers and folds frames into a
    /// per-PID accumulator. `None` when construction failed (rare —
    /// usually a kernel thread-creation failure) so the rest of the
    /// runtime degrades gracefully.
    telemetry: Option<Dispatcher>,
    /// Tier 2.3 — cumulative governor-kill counter, keyed by reason.
    /// Surfaced as `edge_monitor_governor_kills_total{reason="..."}`
    /// in the Prometheus exporter.
    kills_by_reason: HashMap<String, u64>,
    /// Tier 2.3 — cumulative regression count keyed by (model, metric).
    regressions_count: HashMap<(String, String), u64>,
    /// Tier 2.3 — last observed cold-load duration per model (seconds).
    /// Populated when the cold-load detector finalises a stats record;
    /// stays at the most recent value until a new run for that model
    /// completes a fresh cold-load.
    cold_load_seconds_by_model: HashMap<String, f32>,
    /// Tier 3.1 — model fingerprint cache. Hashes head+tail of the
    /// weight file once per `(dev, inode, mtime, len)` tuple.
    fingerprinter: Fingerprinter,
    /// Tier 3.1 — last seen weight-file path per AI PID. Updated on
    /// every classification result that includes one; consulted on
    /// exit so we know which file to fingerprint.
    pid_to_model_path: HashMap<u32, PathBuf>,
    /// Tier 3.5 — PIDs the governor has signalled this run. Populated
    /// by `record_governor_audit` when SIGTERM/SIGKILL fires (or
    /// would-fire in dry-run); consulted on exit to attribute the
    /// kill to `ExitReason::GovernorKill`.
    governor_killed_pids: HashMap<u32, String>,
    /// Previous tick's cumulative CPU ticks, per PID, plus the wall-clock
    /// timestamp that reading was taken at. Used to compute cpu_pct as
    /// delta_ticks / CLK_TCK / elapsed_secs × 100.
    prev_cpu: HashMap<u32, (u64, Instant)>,
    /// L11b — first tick at which each PID was observed by this
    /// runtime instance. Populated lazily via `or_insert_with(Instant::now)`
    /// in the per-tick annotation pass; entries are removed when a
    /// PID exits (covered by the existing `pid_to_model_path` /
    /// telemetry cleanup hooks). Drives the Loading-state warmup
    /// gate in `compute_workload_status`.
    pid_first_seen_at: HashMap<u32, Instant>,
    /// L19 — transient per-PID stderr capture for the post-mortem card.
    /// Lives in `Runtime` (not in `state`) because the contents are
    /// **never persisted** (see `src/storage/run_store.rs` "Privacy
    /// stance: no stderr persistence") and `state` is otherwise the
    /// surface the storage layer mirrors. Pruned per tick by
    /// `sweep_expired_stderr`; entries auto-expire 30 s after
    /// `mark_stderr_exit` and on `clear_stderr` (called from the L24
    /// Esc cascade when the post-mortem card dismisses).
    pid_stderr: HashMap<u32, StderrBuffer>,
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
                .map(|s| s.with_keep_limit(Some(config.storage.keep_runs_per_model as usize)))
        });
        let mut telemetry = build_dispatcher(&config);
        if let Some(d) = telemetry.as_mut()
            && let Err(e) = d.enable_exporter(&config.telemetry.prometheus_bind)
        {
            tracing::error!(error = %e, "prometheus exporter setup failed; continuing without it");
        }
        // Tier 3.6 — vision probe Unix socket. enable_vision_probe is a
        // no-op when the configured path is empty, so this call is safe
        // unconditionally. Without it, [telemetry] vision_probe_socket
        // is parsed but the listener never binds — the gap [B-4-3.6]
        // smoke surfaced.
        if let Some(d) = telemetry.as_mut() {
            d.enable_vision_probe(&config.telemetry.vision_probe_socket);
        }
        let fingerprinter = Fingerprinter::open(if config.storage.fingerprint_cache.is_empty() {
            None
        } else {
            Some(expand_tilde(&config.storage.fingerprint_cache))
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
            telemetry,
            kills_by_reason: HashMap::new(),
            regressions_count: HashMap::new(),
            cold_load_seconds_by_model: HashMap::new(),
            fingerprinter,
            pid_to_model_path: HashMap::new(),
            pid_first_seen_at: HashMap::new(),
            governor_killed_pids: HashMap::new(),
            prev_cpu: HashMap::new(),
            pid_stderr: HashMap::new(),
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

    /// L8 — drain queued exit-driven alert events accumulated by
    /// the lifecycle exit hook since the last call. The UI loop
    /// dispatches each event to `App::observe_exit` and discards
    /// them after; queue is cleared by the drain so it never grows
    /// across ticks.
    pub fn drain_exit_alerts(&mut self) -> Vec<ExitAlertEvent> {
        std::mem::take(&mut self.state.pending_exit_alerts)
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
    /// Is the named process on the governor's allowlist? Surface for
    /// the TUI's armed-kill banner so it can show the override-variant
    /// copy without re-deriving allowlist state from the policy. Pure
    /// read; doesn't touch governor state.
    pub fn is_allowlisted(&self, name: &str) -> bool {
        crate::governor::manual::ManualKiller::is_allowlisted(name, &self.governor)
    }

    /// Build the post-mortem snapshot for the most recent run of
    /// `model`. Returns the snapshot **and the exited PID** so the
    /// caller can stamp the PID onto the `PostMortemCard` for the
    /// L24 Esc cascade's dismiss-clear hook. `None` when the run
    /// store has no history for the model.
    ///
    /// Used by the `Enter`-on-focused-row handler in [UX-2] (UI
    /// Contract v2): the card is shown on demand for the focused
    /// workload, not auto-pushed when any AI process exits.
    ///
    /// L19 — consults the transient stderr buffer (keyed by the
    /// exited PID) so the card can render the captured tail when
    /// present. Empty tail when no buffer entry exists, when the
    /// 30 s expiry has elapsed, or when no sampler populated the
    /// buffer for this PID.
    pub fn latest_postmortem(
        &self,
        model: &str,
    ) -> Option<(crate::ui::panels::postmortem::PostMortem, u32)> {
        let rs = self.run_store.as_ref()?;
        let mut recent = rs.recent(model, 1);
        if recent.is_empty() {
            return None;
        }
        let record = recent.remove(0);
        let baseline_status = build_baseline_status(
            record.metrics.tokens_per_sec_avg,
            rs,
            model,
            &self.config.regression,
        );
        let exited_pid = record.summary.pid;
        let stderr_tail = self.stderr_tail(exited_pid);
        let post_mortem = crate::ui::panels::postmortem::PostMortem::from_run_record_with_stderr(
            &record,
            baseline_status,
            stderr_tail,
        );
        Some((post_mortem, exited_pid))
    }

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
                    workload_category,
                    evidence,
                    model_name,
                    model_path,
                } = classifier::classify_process(p);
                if let Some(path) = model_path {
                    // Tier 3.1 — remember which weight file each PID
                    // is using so we can fingerprint it on exit.
                    self.pid_to_model_path.insert(p.pid, path);
                }
                let cpu_pct = self.compute_cpu_pct(p.pid, p.cpu_time_ticks, now);
                next_cpu.insert(p.pid, (p.cpu_time_ticks, now));
                let first_observed_at =
                    *self.pid_first_seen_at.entry(p.pid).or_insert(now);
                AnnotatedProcess {
                    pid: p.pid,
                    name: p.name.clone(),
                    category,
                    workload_category,
                    evidence,
                    model_name,
                    cpu_pct,
                    rss_mb: p.rss_bytes / (1024 * 1024),
                    vram_bytes: vram_by_pid.get(&p.pid).copied(),
                    first_observed_at,
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

        // Tier 1.2 — drive telemetry samplers against AI processes
        // BEFORE we tally exits, so the accumulator sees one last
        // sample window for any process that's about to exit.
        // Tier 2.1 — also fold system NVML/RAPL power into per-PID
        // frames here.
        if let Some(d) = &mut self.telemetry {
            let live_ai: Vec<TelemetryProcessSnapshot> = snapshot
                .processes
                .iter()
                .filter_map(|s| {
                    let ann = annotated.iter().find(|a| a.pid == s.pid)?;
                    if ann.category == AICategory::NotAi {
                        return None;
                    }
                    Some(TelemetryProcessSnapshot {
                        pid: s.pid,
                        name: s.name.clone(),
                        cmdline: s.cmdline.clone(),
                        environ: s.environ.clone(),
                        model_name: ann.model_name.clone(),
                    })
                })
                .collect();
            d.tick(&live_ai);
            d.record_system_power(&live_ai, &snapshot.gpu);
            d.record_disk_io(&live_ai);
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
                let mut summary_to_record = summary.clone();
                // Tier 1.2c — promote authoritative model_name from
                // an API source (Ollama /api/ps) over the classifier's
                // heuristic guess. Done before constructing the record
                // so model_or_name() routes the record to the right
                // per-model bucket in RunStore.
                if let Some(d) = &self.telemetry
                    && let Some(hint) = d.model_name_hint_for(summary.pid)
                {
                    summary_to_record.model_name = Some(hint);
                }
                let mut record = RunRecord::from_summary(summary_to_record);
                // Fold telemetry-derived metrics onto the record.
                if let Some(d) = &self.telemetry
                    && let Some(metrics) = d.metrics_for(summary.pid)
                {
                    record.metrics = metrics;
                }
                // Tier 2.2 — attach cold-load stats if the detector
                // saw a complete load before exit. Streaming inference
                // workloads with no plateau will have None here.
                if let Some(d) = &self.telemetry
                    && let Some(cs) = d.cold_start_for(summary.pid)
                {
                    // Tier 2.3 — keep the most recent cold-load duration
                    // per model name available to the Prom exporter so
                    // dashboards can plot warm-vs-cold start latency.
                    let model_label = record.model_or_name().to_string();
                    self.cold_load_seconds_by_model
                        .insert(model_label, cs.duration_seconds);
                    record.cold_start = Some(cs);
                }
                // Tier 3.1 — fingerprint the weight file (cached by
                // dev+inode+mtime+len) so the history viewer can tell
                // quantization variants of the same model name apart.
                if let Some(path) = self.pid_to_model_path.get(&summary.pid).cloned()
                    && let Some(fp) = self.fingerprinter.fingerprint(&path)
                {
                    record.model_fingerprint = Some(fp);
                }
                // Tier 3.5 — richer exit-reason classification. We
                // only spend a journalctl invocation when the signal
                // hint suggests something dmesg might explain (SIGKILL
                // for OOM); for clean exits / SIGTERM we skip the
                // subprocess entirely.
                let dmesg_lines = if summary.signal == Some(9) {
                    read_recent_kernel_log(10)
                } else {
                    Vec::new()
                };
                let governor_reason = self.governor_killed_pids.remove(&summary.pid);
                // L19 — feed the transient buffer's tail into
                // `ExitContext` so Tier 3.5 classification can see
                // recent stderr (CUDA OOM / CUDA error patterns)
                // when a runtime-side sampler has populated the
                // buffer for this PID. Empty when no sampler ran
                // against this PID — exec_wrapper-launched workloads
                // still use their in-process tail. Direct field
                // access keeps the borrow scoped to `pid_stderr`
                // (a sibling field) so the surrounding
                // `&mut self.run_store` borrow stays live.
                let now_for_stderr = Instant::now();
                let stderr_lines = self
                    .pid_stderr
                    .get(&summary.pid)
                    .filter(|b| !b.is_expired_at(now_for_stderr))
                    .map(|b| b.tail())
                    .unwrap_or_default();
                let ctx = ExitContext {
                    dmesg_lines,
                    stderr_lines,
                    killed_by_governor: governor_reason.is_some(),
                    governor_reason,
                };
                record.exit_reason = classify_exit(summary, &ctx);
                // L19 — mark the buffer for 30 s expiry from this
                // exit. Until then the post-mortem card can render
                // the captured tail; after that the entry is swept
                // by `sweep_expired_stderr`. Direct field access
                // for the same borrow-scope reason as above.
                if let Some(buf) = self.pid_stderr.get_mut(&summary.pid) {
                    buf.exit_at = Some(now_for_stderr);
                }
                // L8 / UX_CONTRACT.md §4 — queue an exit-driven
                // alert if the classified reason warrants one.
                // Clean exits are silent per §4 ("never on clean
                // (code 0) exits"); OOM cases fire OomDetected;
                // everything else non-clean fires WorkloadExited
                // with a `{reason}` string captured at fire time.
                if let Some((alert_id, alert_reason)) = classify_for_alert(&record.exit_reason) {
                    self.state.pending_exit_alerts.push(ExitAlertEvent {
                        pid: summary.pid,
                        workload_name: summary.name.clone(),
                        alert_id,
                        reason: alert_reason,
                    });
                }
                let model = record.model_or_name().to_string();
                let record_clone = record.clone();
                if let Err(e) = rs.append(record) {
                    tracing::warn!(error = %e, "failed to persist run record");
                }

                // Tier 1.3 — regression detection. Run *after* the
                // append so the new record is part of the history but
                // baseline excludes it (rs.recent skips the in-flight
                // record by index ordering... actually it's first now,
                // so we must explicitly compare against the prior N).
                let regs_before = self.state.regressions.len();
                check_regressions(
                    rs,
                    &record_clone,
                    &model,
                    &self.config.regression,
                    &mut self.state.regressions,
                    self.config.runtime.audit_history,
                );
                // Tier 2.3 — count regressions for the Prom counter.
                for ev in self.state.regressions.iter().skip(regs_before) {
                    *self
                        .regressions_count
                        .entry((ev.model.clone(), ev.regression.metric.clone()))
                        .or_insert(0) += 1;
                }
            }
            // Drop accumulator state for the exited PID so a recycled
            // PID later starts fresh. Tier 1.2 + 3.1.
            if let Some(d) = &mut self.telemetry {
                d.forget(summary.pid);
            }
            self.pid_to_model_path.remove(&summary.pid);
            // L11b — drop the warmup-gate timestamp so a recycled PID
            // starts fresh in the Loading state instead of inheriting
            // the prior process's age.
            self.pid_first_seen_at.remove(&summary.pid);
        }

        let decisions = self.governor.evaluate(&lifecycle);

        // Tier 2.3 — count governor decisions by reason, for the
        // Prometheus exporter. Done before we move `decisions` onto
        // the runtime state.
        for (_pid, action, reason) in &decisions {
            let key = match action {
                KillAction::SignalTermSent => "sigterm".to_string(),
                KillAction::SignalKillSent => "sigkill".to_string(),
                KillAction::DryRunTermWould | KillAction::DryRunKillWould => "dry_run".to_string(),
                KillAction::Whitelisted => "whitelisted".to_string(),
                KillAction::AlreadyExited => "already_exited".to_string(),
                KillAction::RateLimited => "rate_limited".to_string(),
                KillAction::PidReusedAborted => "pid_reused_aborted".to_string(),
                KillAction::Skipped => format!("skipped:{}", reason),
            };
            *self.kills_by_reason.entry(key).or_insert(0) += 1;
        }

        // Tier 3.3 — refresh per-PID live telemetry the registry panel
        // reads. Pulls from the dispatcher's accumulator (already
        // updated above by `d.tick(...)`). Empty when telemetry is
        // off or the accumulator has no samples for that PID.
        self.state.live_telemetry.clear();
        if let Some(d) = &self.telemetry {
            for p in &annotated {
                if p.category == AICategory::NotAi {
                    continue;
                }
                if let Some(m) = d.metrics_for(p.pid) {
                    self.state.live_telemetry.insert(
                        p.pid,
                        LiveTelemetry {
                            kv_cache_peak_pct: m.kv_cache_peak_pct,
                            kv_cache_evictions_total: m.kv_cache_evictions_total,
                        },
                    );
                }
            }
        }

        self.state.annotated = annotated;
        self.state.decisions = decisions;
        self.state.last_snapshot = Some(snapshot);
        self.state.last_lifecycle = Some(lifecycle);
        self.state.tick_count += 1;
        self.state.last_tick = Some(Instant::now());

        // L19 — drop stderr buffers whose 30 s post-exit window has
        // elapsed. Runs once per tick so retained entries never
        // outlive their contract-defined lifetime by more than one
        // tick interval (≤ 1 s at the default rate).
        self.sweep_expired_stderr();

        // Tier 2.3 — refresh the Prometheus exporter snapshot if any
        // operator is scraping. publish_metrics() is non-blocking;
        // try_lock-fail just drops this update in favour of the next.
        self.publish_metrics();

        Ok(&self.state)
    }

    /// Build the [`MetricsSnapshot`] the Prometheus exporter serves
    /// and push it into the dispatcher's shared handle. No-op when
    /// telemetry / exporter is disabled.
    fn publish_metrics(&self) {
        let Some(d) = &self.telemetry else {
            return;
        };
        let mut snap = crate::telemetry::exporter::MetricsSnapshot::new();
        snap.tick_count = self.state.tick_count;
        // GPU temperature — NVML reports per-device. Attribute it to a
        // PID by looking up which device holds that PID's VRAM. When
        // multiple devices are involved we pick the first match (a
        // multi-GPU model is rare on edge boxes; the alternative is
        // emitting one series per pid×device which the exporter then
        // has to label-disambiguate).
        let gpu_temp_for_pid = |pid: u32| -> Option<f32> {
            let snap = self.state.last_snapshot.as_ref()?;
            for dev in &snap.gpu.devices {
                if dev.per_process_vram.contains_key(&pid) {
                    return dev.temp_c;
                }
            }
            None
        };
        for p in &self.state.annotated {
            let cat_label = match p.category {
                AICategory::Inference => "inference",
                AICategory::Training => "training",
                AICategory::ModelDownload => "model_download",
                AICategory::Framework => "framework",
                AICategory::NotAi => continue,
            };
            *snap
                .processes_by_category
                .entry(cat_label.into())
                .or_insert(0) += 1;
            snap.ai_processes_active += 1;
            // Live per-PID gauges — for AI processes only, with the
            // dispatcher's accumulator providing tokens/fps/watts.
            let metrics = d.metrics_for(p.pid).unwrap_or_default();
            snap.live.push(crate::telemetry::exporter::LiveAiSample {
                pid: p.pid,
                model: p.model_name.clone().unwrap_or_else(|| p.name.clone()),
                category: cat_label.into(),
                tokens_per_sec: metrics.tokens_per_sec_avg,
                fps: metrics.fps_avg,
                vram_bytes: p.vram_bytes,
                gpu_watts: metrics.gpu_watts_avg,
                cpu_watts: metrics.cpu_watts_avg,
                gpu_temp_celsius: gpu_temp_for_pid(p.pid),
            });
        }
        snap.kills_by_reason = self.kills_by_reason.clone();
        snap.regressions = self.regressions_count.clone();
        snap.cold_load_seconds = self.cold_load_seconds_by_model.clone();
        d.publish_metrics(snap);
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

            // Tier 3.5 — remember governor-driven kills so the exit
            // classifier can attribute them. Both real and dry-run
            // signals get tracked: in dry-run the process exits on
            // its own and we still want correct attribution if it
            // happens to die during the same window.
            self.governor_killed_pids.insert(*pid, reason.clone());

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

// L19 / UX_CONTRACT.md §5 — transient stderr-when-fresh buffer.
// Lives on `Runtime` rather than `RuntimeState` because the contents
// are never persisted and never serialised (the L18 privacy guard
// at tests/no_stderr_persistence_guard.rs walks `src/storage/` and
// forbids any `stderr*` field on a Serialize-deriving type; `Runtime`
// is out of that scope on purpose).
impl Runtime {
    /// Append a stderr line to `pid`'s transient buffer. Caps at
    /// [`StderrBuffer::MAX_LINES`] × [`StderrBuffer::MAX_LINE_BYTES`];
    /// oldest lines fall off when full, lines longer than the byte cap
    /// are truncated at the nearest UTF-8 boundary. No-op once the
    /// buffer has been marked exited (post-exit data is suspect).
    pub fn record_stderr_line(&mut self, pid: u32, line: &str) {
        let buf = self.pid_stderr.entry(pid).or_default();
        if buf.exit_at.is_some() {
            return;
        }
        buf.push_line(line);
    }

    /// Mark `pid`'s buffer as exited at `now`. From this instant the
    /// buffer is read-only and counts down to expiry per
    /// [`StderrBuffer::EXPIRY`]. No-op when no buffer entry exists —
    /// we don't allocate empty entries just to time them.
    ///
    /// Exposed with an explicit `now` (rather than calling
    /// `Instant::now` internally) so tests can rewind the timer and
    /// exercise the expiry branch without sleeping.
    pub fn mark_stderr_exit_at(&mut self, pid: u32, now: Instant) {
        if let Some(buf) = self.pid_stderr.get_mut(&pid) {
            buf.exit_at = Some(now);
        }
    }

    /// Convenience for production callers: mark the exit at
    /// `Instant::now()`.
    pub fn mark_stderr_exit(&mut self, pid: u32) {
        self.mark_stderr_exit_at(pid, Instant::now());
    }

    /// Drop the buffer entry for `pid`. Called from the L24 Esc
    /// cascade when the post-mortem card dismisses — the buffer must
    /// not outlive the card's visibility per "stderr is ephemeral".
    pub fn clear_stderr(&mut self, pid: u32) {
        self.pid_stderr.remove(&pid);
    }

    /// Read `pid`'s current stderr tail. Returns an empty vec when no
    /// entry exists or when the entry has expired (≥
    /// [`StderrBuffer::EXPIRY`] past `now`'s `exit_at`). Pure read —
    /// does not prune; callers that want the buffer dropped must call
    /// [`Self::clear_stderr`] or [`Self::sweep_expired_stderr_at`].
    pub fn stderr_tail_at(&self, pid: u32, now: Instant) -> Vec<String> {
        let Some(buf) = self.pid_stderr.get(&pid) else {
            return Vec::new();
        };
        if buf.is_expired_at(now) {
            return Vec::new();
        }
        buf.tail()
    }

    /// `stderr_tail_at(pid, Instant::now())`.
    pub fn stderr_tail(&self, pid: u32) -> Vec<String> {
        self.stderr_tail_at(pid, Instant::now())
    }

    /// Drop every buffer entry whose 30 s post-exit window has lapsed
    /// at `now`. Cheap; called once per tick from the tick loop so
    /// retained entries never outlive their expiry by more than one
    /// tick interval.
    pub fn sweep_expired_stderr_at(&mut self, now: Instant) {
        self.pid_stderr.retain(|_, buf| !buf.is_expired_at(now));
    }

    /// `sweep_expired_stderr_at(Instant::now())`.
    pub fn sweep_expired_stderr(&mut self) {
        self.sweep_expired_stderr_at(Instant::now());
    }
}

/// Compute the [`BaselineStatus`] for a freshly-exited run by reading
/// the same baseline window the regression detector uses. Independent
/// of `RegressionConfig` thresholds (the post-mortem-card contract
/// pins its bands directly: ≥20% slower → critical, ≥10% slower →
/// attention, ≤-10% (faster) → healthy, otherwise matching). Returns
/// `NotAvailable` whenever there's no usable baseline (first run, or
/// fewer than `min_baseline_samples` prior records).
fn build_baseline_status(
    current: Option<f32>,
    rs: &RunStore,
    model: &str,
    cfg: &crate::config::RegressionConfig,
) -> crate::ui::panels::postmortem::BaselineStatus {
    use crate::ui::panels::postmortem::BaselineStatus;

    let Some(_) = current else {
        return BaselineStatus::NotAvailable;
    };
    let window = cfg.baseline_window as usize;
    // Same shape as `check_regressions`: pull window+1 newest records
    // and drop the in-flight one. We don't have its run_id here, so
    // we drop the leading entry (newest first); if that leading
    // record happens to be the in-flight one (it is — it was just
    // appended), this is the baseline-excluding-current view.
    let mut history = rs.recent(model, window + 1);
    if !history.is_empty() {
        history.remove(0);
    }
    if history.len() < cfg.min_baseline_samples as usize {
        return BaselineStatus::NotAvailable;
    }
    let strategy = cfg.strategy();
    let (metrics, _outliers) =
        crate::analysis::BaselineMetrics::from_records_with(&history, strategy, cfg.drop_outliers);
    let baseline_mean = metrics.tokens_per_sec_avg.map(|m| m.mean);
    BaselineStatus::from_metric(current, baseline_mean)
}

/// Inputs to [`compute_workload_status`]. Gathered each tick from
/// `RuntimeState.live_telemetry`, the platform RAM snapshot, the
/// governor's armed-kill state, and the OOM-detection window.
///
/// Optional fields denote "telemetry not available for this workload"
/// (no GPU, non-LLM, missing throughput sample) and contribute nothing
/// to the status — they don't escalate to a worse band by absence.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkloadStatusInputs {
    /// VRAM utilization for this workload, percent (0..=100). `None`
    /// when no GPU or no per-process allocation reading.
    pub vram_pct: Option<f64>,
    /// Host RAM utilization (system-wide), percent (0..=100).
    pub ram_pct: Option<f64>,
    /// KV-cache occupancy for LLM workloads, percent (0..=100). `None`
    /// for non-LLM (vision / embeddings / ROS2 / unknown).
    pub kv_cache_pct: Option<f64>,
    /// Throughput vs. baseline as `(current, baseline)`. `None` when
    /// telemetry has no current sample, or when no baseline exists
    /// for this model yet — without a baseline, throughput contributes
    /// nothing per UX_CONTRACT.md §3 ("throughput ≤ baseline × 0.80"
    /// requires the baseline to exist).
    pub throughput_vs_baseline: Option<(f64, f64)>,
    /// True when the manual-kill arm is active against this PID.
    pub governor_armed: bool,
    /// True when the OOM detector has fired against this PID within
    /// the recent detection window.
    pub oom_detected: bool,
    /// How long telemetry has been collected for this PID. Workloads
    /// younger than `ux_contract::thresholds::BASELINE_WARMUP_SECS`
    /// render as `Loading` regardless of current values — there's no
    /// baseline yet and current readings haven't stabilized.
    pub telemetry_age: Duration,
}

/// Compute the live workload-status dot per UX_CONTRACT.md §3.
///
/// **No hysteresis** per contract §3 — a workload that flickers
/// between Attention and Healthy has a real problem; smoothing it
/// would mask the signal. If a future row introduces a debounce
/// timer, audit it carefully against the contract before merging.
///
/// Priority order:
/// 1. Loading (warmup gate; before anything else can fire),
/// 2. Critical (any of: VRAM ≥ 95%, KV ≥ 95%, governor armed, OOM),
/// 3. Attention (any of: VRAM ≥ 85%, RAM ≥ 90%, KV ≥ 80%, throughput
///    ≤ baseline × 0.80),
/// 4. Healthy (everything else).
///
/// L3 produces only the enum value. Symbol/colour rendering belongs
/// to L21 + the workloads panel — this function must not reach into
/// `ratatui` or `Span` types.
pub fn compute_workload_status(inputs: &WorkloadStatusInputs) -> ux_contract::WorkloadStatus {
    use ux_contract::WorkloadStatus;
    use ux_contract::thresholds::{
        BASELINE_WARMUP_SECS, KV_ATTENTION_PCT, KV_CRITICAL_PCT, RAM_ATTENTION_PCT,
        THROUGHPUT_ATTENTION_RATIO, VRAM_ATTENTION_PCT, VRAM_CRITICAL_PCT,
    };

    if inputs.telemetry_age < Duration::from_secs(BASELINE_WARMUP_SECS) {
        return WorkloadStatus::Loading;
    }

    let critical = inputs.governor_armed
        || inputs.oom_detected
        || inputs.vram_pct.is_some_and(|v| v >= VRAM_CRITICAL_PCT)
        || inputs.kv_cache_pct.is_some_and(|kv| kv >= KV_CRITICAL_PCT);
    if critical {
        return WorkloadStatus::Critical;
    }

    let throughput_regressed =
        inputs
            .throughput_vs_baseline
            .is_some_and(|(current, baseline)| {
                baseline > 0.0 && current <= baseline * THROUGHPUT_ATTENTION_RATIO
            });
    let attention = throughput_regressed
        || inputs.vram_pct.is_some_and(|v| v >= VRAM_ATTENTION_PCT)
        || inputs.ram_pct.is_some_and(|r| r >= RAM_ATTENTION_PCT)
        || inputs.kv_cache_pct.is_some_and(|kv| kv >= KV_ATTENTION_PCT);
    if attention {
        return WorkloadStatus::Attention;
    }

    WorkloadStatus::Healthy
}

/// Construct the telemetry [`Dispatcher`] from the toggles in
/// `config.telemetry`. Returns `None` (and logs an error) if the
/// Tokio runtime cannot be built — the rest of the runtime degrades
/// gracefully without telemetry.
fn build_dispatcher(config: &Config) -> Option<Dispatcher> {
    let mut sources: Vec<Box<dyn TelemetrySource>> = Vec::new();
    if config.telemetry.vllm_scrape {
        sources.push(Box::new(VllmPrometheusSource::new()));
    }
    if config.telemetry.llamacpp_scrape {
        sources.push(Box::new(LlamaCppServerSource::new()));
    }
    if config.telemetry.ollama_api {
        sources.push(Box::new(OllamaApiSource::new()));
    }
    if sources.is_empty() {
        return None;
    }
    match Dispatcher::new(sources) {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::error!(error = %e, "failed to build telemetry dispatcher; continuing without telemetry");
            None
        }
    }
}

/// Tier 1.3 hook: compare a freshly-appended record against the rolling
/// baseline of prior runs. Emits a tracing::warn per detected regression
/// (severity ≥ Warn) and pushes a `RegressionEvent` onto the in-memory
/// ring so the TUI audit panel can render it.
///
/// The new record is now the *first* entry in `rs.recent(model, _)`, so
/// the baseline is intentionally computed from `recent(model, window+1)`
/// and the leading entry is dropped — otherwise the new run would
/// average itself into the very baseline it's being compared against.
fn check_regressions(
    rs: &RunStore,
    new_record: &RunRecord,
    model: &str,
    cfg: &crate::config::RegressionConfig,
    sink: &mut VecDeque<crate::analysis::RegressionEvent>,
    sink_cap: usize,
) {
    let window = cfg.baseline_window as usize;
    let mut history = rs.recent(model, window + 1);
    if history.is_empty() {
        return;
    }
    // Drop the in-flight record (it's the newest, hence first).
    if history[0].run_id == new_record.run_id {
        history.remove(0);
    }
    if history.len() < cfg.min_baseline_samples as usize {
        return;
    }
    let strategy = cfg.strategy();
    let (metrics, outliers) =
        crate::analysis::BaselineMetrics::from_records_with(&history, strategy, cfg.drop_outliers);
    let baseline = crate::analysis::Baseline {
        model: model.to_string(),
        sample_size: history.len(),
        metrics,
        computed_at: chrono::Utc::now(),
        outlier_run_ids: outliers,
        strategy,
    };
    let detector_cfg = DetectorConfig {
        warn_pct: cfg.warn_pct,
        critical_pct: cfg.critical_pct,
        min_baseline_samples: cfg.min_baseline_samples as usize,
    };
    let regressions = detect_regressions_with(new_record, &baseline, &detector_cfg);
    for r in regressions {
        if r.severity < crate::analysis::Severity::Warn {
            continue;
        }
        tracing::warn!(
            model = %model,
            metric = %r.metric,
            baseline = r.baseline,
            current = r.current,
            delta_pct = r.delta_pct,
            severity = ?r.severity,
            "regression detected"
        );
        let event = crate::analysis::RegressionEvent {
            timestamp: chrono::Utc::now(),
            model: model.to_string(),
            baseline_size: baseline.sample_size,
            regression: r,
        };
        sink.push_back(event);
        while sink.len() > sink_cap {
            sink.pop_front();
        }
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

    // ── Tier 1.3 regression-detection plumbing tests ────────────────
    //
    // These hit `check_regressions` directly: build a tempdir RunStore,
    // seed it with a baseline of fast records, append a slow one, and
    // assert the sink fills (or doesn't, for the negative cases).

    use crate::analysis::{RegressionEvent, Severity};
    use crate::lifecycle::LifecycleSummary;
    use crate::model::AICategory;
    use chrono::Utc;

    fn lc(model: &str, peak_rss_mb: u64) -> LifecycleSummary {
        LifecycleSummary {
            pid: 1,
            name: "python".into(),
            category: Some(AICategory::Inference),
            model_name: Some(model.into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 30,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 50.0,
            peak_cpu_pct: 80.0,
            peak_rss_mb,
            peak_vram_mb: 0,
            samples: 30,
        }
    }

    #[test]
    fn regression_fires_on_metric_blowup() {
        let dir = tempfile::tempdir().unwrap();
        let mut rs = RunStore::open(dir.path()).unwrap();
        // Baseline of 10 runs at 1024 MB peak RSS.
        for _ in 0..10 {
            rs.append(RunRecord::from_summary(lc("phi3-mini", 1024)))
                .unwrap();
        }
        // New run at 2000 MB → ~95% over baseline → critical regression.
        let bad = RunRecord::from_summary(lc("phi3-mini", 2000));
        rs.append(bad.clone()).unwrap();

        let cfg = crate::config::RegressionConfig::default();
        let mut sink: VecDeque<RegressionEvent> = VecDeque::new();
        check_regressions(&rs, &bad, "phi3-mini", &cfg, &mut sink, 100);

        let event = sink
            .iter()
            .find(|e| e.regression.metric == "peak_rss_mb")
            .expect("expected peak_rss_mb regression");
        assert!(event.regression.severity >= Severity::Critical);
        assert_eq!(event.model, "phi3-mini");
        assert_eq!(event.baseline_size, 10);
    }

    #[test]
    fn regression_silent_on_matching_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut rs = RunStore::open(dir.path()).unwrap();
        for _ in 0..10 {
            rs.append(RunRecord::from_summary(lc("phi3", 1024)))
                .unwrap();
        }
        let same = RunRecord::from_summary(lc("phi3", 1024));
        rs.append(same.clone()).unwrap();

        let cfg = crate::config::RegressionConfig::default();
        let mut sink: VecDeque<RegressionEvent> = VecDeque::new();
        check_regressions(&rs, &same, "phi3", &cfg, &mut sink, 100);
        assert!(sink.is_empty(), "no regression expected, got: {sink:?}");
    }

    #[test]
    fn regression_silent_below_min_baseline_samples() {
        let dir = tempfile::tempdir().unwrap();
        let mut rs = RunStore::open(dir.path()).unwrap();
        // Only 2 baseline records; min_baseline_samples = 3 by default.
        for _ in 0..2 {
            rs.append(RunRecord::from_summary(lc("phi3", 1024)))
                .unwrap();
        }
        // Catastrophic value but baseline too small → no event.
        let bad = RunRecord::from_summary(lc("phi3", 50_000));
        rs.append(bad.clone()).unwrap();

        let cfg = crate::config::RegressionConfig::default();
        let mut sink: VecDeque<RegressionEvent> = VecDeque::new();
        check_regressions(&rs, &bad, "phi3", &cfg, &mut sink, 100);
        assert!(
            sink.is_empty(),
            "tiny baseline must not flag regressions; got: {sink:?}"
        );
    }

    #[test]
    fn regression_sink_caps_at_configured_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut rs = RunStore::open(dir.path()).unwrap();
        for _ in 0..10 {
            rs.append(RunRecord::from_summary(lc("phi3", 1024)))
                .unwrap();
        }
        let bad = RunRecord::from_summary(lc("phi3", 5000));
        rs.append(bad.clone()).unwrap();

        let cfg = crate::config::RegressionConfig::default();
        let mut sink: VecDeque<RegressionEvent> = VecDeque::new();
        // Multiple metrics regress (peak_rss_mb at minimum). Cap at 1
        // and verify the buffer trims.
        check_regressions(&rs, &bad, "phi3", &cfg, &mut sink, 1);
        assert!(sink.len() <= 1, "sink overflowed cap: {sink:?}");
    }

    // ========================================================================
    // L3 / UX_CONTRACT.md §3 — `compute_workload_status` tests.
    // ========================================================================

    /// Inputs that would produce `Healthy` after warmup. Tests start
    /// from this baseline and perturb individual fields so the
    /// "Healthy" assertions catch any drift in the default-construction
    /// path.
    fn healthy_inputs() -> WorkloadStatusInputs {
        WorkloadStatusInputs {
            vram_pct: Some(50.0),
            ram_pct: Some(50.0),
            kv_cache_pct: Some(50.0),
            throughput_vs_baseline: Some((40.0, 40.0)),
            governor_armed: false,
            oom_detected: false,
            telemetry_age: Duration::from_secs(60),
        }
    }

    #[test]
    fn compute_workload_status_returns_loading_during_warmup() {
        let mut inputs = healthy_inputs();
        inputs.telemetry_age = Duration::from_secs(0);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Loading
        );
    }

    #[test]
    fn compute_workload_status_loading_at_warmup_boundary_minus_one() {
        // 1s before the warmup gate releases.
        let mut inputs = healthy_inputs();
        inputs.telemetry_age =
            Duration::from_secs(ux_contract::thresholds::BASELINE_WARMUP_SECS - 1);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Loading
        );
    }

    #[test]
    fn compute_workload_status_healthy_at_warmup_boundary() {
        let mut inputs = healthy_inputs();
        inputs.telemetry_age = Duration::from_secs(ux_contract::thresholds::BASELINE_WARMUP_SECS);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_critical_on_vram_at_95pct() {
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_CRITICAL_PCT);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_attention_on_vram_at_85pct() {
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_ATTENTION_PCT);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_healthy_on_vram_just_below_attention() {
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_ATTENTION_PCT - 0.1);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_critical_on_kv_at_95pct() {
        let mut inputs = healthy_inputs();
        inputs.kv_cache_pct = Some(ux_contract::thresholds::KV_CRITICAL_PCT);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_attention_on_kv_at_80pct() {
        let mut inputs = healthy_inputs();
        inputs.kv_cache_pct = Some(ux_contract::thresholds::KV_ATTENTION_PCT);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_attention_on_ram_at_90pct() {
        let mut inputs = healthy_inputs();
        inputs.ram_pct = Some(ux_contract::thresholds::RAM_ATTENTION_PCT);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_attention_only_on_ram_above_critical_const() {
        // Per UX_CONTRACT.md §3, RAM does NOT have a Critical condition
        // for the live status dot — only Attention at ≥ 90%. The
        // contract defines `RAM_CRITICAL_PCT = 95.0` for alerting
        // purposes (§4 RAM_PRESSURE) but the status dot omits RAM from
        // its Critical conditions. This test pins that intentional
        // asymmetry: RAM at 99% is Attention, not Critical.
        let mut inputs = healthy_inputs();
        inputs.ram_pct = Some(99.0);
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_critical_on_governor_armed() {
        let mut inputs = healthy_inputs();
        inputs.governor_armed = true;
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_critical_on_oom() {
        let mut inputs = healthy_inputs();
        inputs.oom_detected = true;
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_attention_on_throughput_below_80pct_baseline() {
        let mut inputs = healthy_inputs();
        // 31 tok/s vs baseline 40 → 0.775, below the 0.80 ratio.
        inputs.throughput_vs_baseline = Some((31.0, 40.0));
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_healthy_at_exactly_80pct_baseline() {
        let mut inputs = healthy_inputs();
        // 32.0 / 40.0 = 0.80 exactly. The contract band is "≤ baseline
        // × 0.80" → Attention; 32.0 lands on Attention.
        inputs.throughput_vs_baseline = Some((32.0, 40.0));
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Attention
        );

        // 32.01 / 40.0 = 0.80025 — just above the threshold → Healthy.
        inputs.throughput_vs_baseline = Some((32.01, 40.0));
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_no_throughput_contribution_without_baseline() {
        // Per UX_CONTRACT.md §3, throughput regression triggers
        // Attention only when a baseline exists. Without a baseline
        // the throughput side contributes None to the status; if every
        // resource value is below its Attention band, the workload is
        // Healthy.
        let mut inputs = healthy_inputs();
        inputs.throughput_vs_baseline = None;
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_healthy_when_all_within_bounds() {
        // Healthy baseline: every metric is comfortably under its
        // Attention band, no governor / OOM, baseline matches current.
        assert_eq!(
            compute_workload_status(&healthy_inputs()),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_critical_overrides_attention() {
        // VRAM at 95% (Critical) AND throughput regressed (Attention)
        // must resolve to Critical — the priority order in §3 is
        // Critical → Attention → Healthy.
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_CRITICAL_PCT);
        inputs.throughput_vs_baseline = Some((10.0, 40.0));
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_loading_overrides_critical_during_warmup() {
        // Warmup gate is the outermost: even with Critical-band VRAM
        // and OOM, a workload that hasn't been observed for
        // BASELINE_WARMUP_SECS is Loading (no baseline yet, readings
        // not stable). This locks the priority order so a future PR
        // doesn't accidentally swap Loading and Critical.
        let mut inputs = healthy_inputs();
        inputs.telemetry_age = Duration::from_secs(0);
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_CRITICAL_PCT);
        inputs.oom_detected = true;
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Loading
        );
    }

    // ========================================================================
    // L8 / UX_CONTRACT.md §4 — `classify_for_alert` mapping tests.
    // ========================================================================

    use crate::storage::run_store::ExitReason;

    #[test]
    fn classify_for_alert_clean_exit_fires_no_alert() {
        // §4 — "never on clean (code 0) exits".
        assert_eq!(classify_for_alert(&ExitReason::CleanExit), None);
    }

    #[test]
    fn classify_for_alert_oom_kill_fires_oom_detected() {
        // RAM-side kernel OOM: kernel_oom=true, vram=false. The
        // OomDetected template has no {reason} placeholder, so the
        // mapping returns None for the reason.
        let r = ExitReason::OutOfMemory {
            ram: true,
            vram: false,
        };
        assert_eq!(
            classify_for_alert(&r),
            Some((ux_contract::AlertId::OomDetected, None))
        );
    }

    #[test]
    fn classify_for_alert_cuda_oom_fires_oom_detected() {
        // CUDA OOM: kernel_oom=false, vram=true. Same mapping —
        // OomDetected covers both flavours.
        let r = ExitReason::OutOfMemory {
            ram: false,
            vram: true,
        };
        assert_eq!(
            classify_for_alert(&r),
            Some((ux_contract::AlertId::OomDetected, None))
        );
    }

    #[test]
    fn classify_for_alert_segfault_fires_workload_exited_with_reason() {
        let r = ExitReason::Segfault;
        assert_eq!(
            classify_for_alert(&r),
            Some((ux_contract::AlertId::WorkloadExited, Some("segfault".into())))
        );
    }

    #[test]
    fn classify_for_alert_governor_kill_fires_workload_exited_with_reason() {
        let r = ExitReason::GovernorKill {
            reason: "rate limited".into(),
        };
        let (alert_id, reason) = classify_for_alert(&r).expect("should fire");
        assert_eq!(alert_id, ux_contract::AlertId::WorkloadExited);
        let reason = reason.expect("reason captured at fire time");
        assert!(reason.contains("rate limited"), "reason = {reason}");
        assert!(reason.contains("governor"), "reason = {reason}");
    }

    #[test]
    fn classify_for_alert_exit_nonzero_fires_workload_exited_with_reason() {
        // ExitReason::Crash carries the non-zero exit code.
        let r = ExitReason::Crash { exit_code: 42 };
        let (alert_id, reason) = classify_for_alert(&r).expect("should fire");
        assert_eq!(alert_id, ux_contract::AlertId::WorkloadExited);
        assert_eq!(reason, Some("exit code 42".into()));
    }

    #[test]
    fn classify_for_alert_unknown_fires_workload_exited_with_reason() {
        let (alert_id, reason) = classify_for_alert(&ExitReason::Unknown).expect("should fire");
        assert_eq!(alert_id, ux_contract::AlertId::WorkloadExited);
        assert_eq!(reason, Some("unknown".into()));
    }

    #[test]
    fn classify_for_alert_oom_supersedes_workload_exited_for_oom_class() {
        // OomDetected and WorkloadExited are disjoint by
        // construction — a single ExitReason produces exactly one
        // alert id, never both. This test pins the precedence so a
        // future "fire both" refactor breaks here.
        let r = ExitReason::OutOfMemory {
            ram: true,
            vram: false,
        };
        let (alert_id, _) = classify_for_alert(&r).expect("should fire");
        assert_eq!(alert_id, ux_contract::AlertId::OomDetected);
        assert_ne!(alert_id, ux_contract::AlertId::WorkloadExited);
    }

    #[test]
    fn compute_workload_status_zero_baseline_does_not_fire_attention() {
        // Defensive: if a degenerate baseline of 0.0 sneaks in (ratio
        // would multiply to 0, current ≤ 0 is impossible without
        // negative tok/s), treat the throughput input as "no useful
        // baseline" so we don't accidentally fire Attention against
        // every healthy workload.
        let mut inputs = healthy_inputs();
        inputs.throughput_vs_baseline = Some((40.0, 0.0));
        assert_eq!(
            compute_workload_status(&inputs),
            ux_contract::WorkloadStatus::Healthy
        );
    }
}
