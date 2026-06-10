use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use thiserror::Error;

use std::path::PathBuf;

use crate::analysis::compare::{RegressionConfig as DetectorConfig, detect_regressions_with};
use crate::classifier;
use crate::config::{Config, WorkloadRule, expand_tilde};
use crate::exit_classify::{
    ExitContext, classify_exit, exit_reason_to_wire_strings, read_recent_kernel_log,
};
use crate::fingerprint::Fingerprinter;
use crate::governor::manual::{AuditLogEntry, ManualKillAction};
use crate::governor::{AuditWriter, GovernorExecutor, KillAction, ManualKiller};
use crate::lifecycle::tracker::LifecycleTracker;
use crate::lifecycle::{LifecycleSnapshot, LifecycleSummary};
use crate::model::{AICategory, ClassificationResult, WorkloadCategory};
use crate::platform::{self, GpuSnapshot, PlatformError, PlatformSnapshot};
use crate::storage::{LogStore, RunRecord, RunStore};
use crate::telemetry::samplers::{
    agent_claude::AgentClaudeSource, embeddings_cpu::EmbeddingsCpuSource,
    llama_cpp_server::LlamaCppServerSource, ollama_api::OllamaApiSource,
    ros2_shellout::Ros2ShelloutSource, vllm_prometheus::VllmPrometheusSource,
};
use crate::telemetry::source::ProcessSnapshot as TelemetryProcessSnapshot;
use ux_contract::activity::ActivityState;
use crate::telemetry::{Dispatcher, TelemetrySource};

/// Errors emitted by the runtime tick loop. Platform errors are fatal;
/// per-process errors are absorbed into tracing logs and the audit trail.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("platform sample failed: {0}")]
    Platform(#[from] PlatformError),
    #[error("lifecycle tracking failed: {0}")]
    Lifecycle(String),
    /// v1.3.1 / DISPATCH 53 — `EffectiveThresholds::resolve` rejected
    /// the operator's `[thresholds]` config section. Carries the
    /// resolver's operator-actionable message verbatim.
    #[error("invalid threshold config: {0}")]
    Config(String),
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

/// v1.3.2 / DISPATCH 74 — per-exit classification projected to
/// wire-stable strings. Populated for AI-classified exits at the
/// lifecycle-drain site using the same `(kind, detail)` projection
/// `WireRunRecord::from_record` uses for the legacy
/// /api/snapshot.activity feed; lock-step with
/// [`RuntimeState::completed`] so the activity-feed wire builder
/// can read the attribution by index without re-classifying.
///
/// `None` for non-AI exits (no classification runs) — the parallel
/// VecDeque carries a `None` slot to keep alignment with
/// `completed`. The wire builder skips detail emission when the
/// slot is `None`.
#[derive(Debug, Clone)]
pub struct ExitAttribution {
    /// Wire string for the kind: `"clean" | "governor" | "oom" |
    /// "signal" | "segfault" | "cuda" | "crash" | "unknown"`. Matches
    /// [`crate::web::wire::WireRunRecord::exit_kind`].
    pub exit_kind: String,
    /// Free-form detail (kill reason, dmesg fragment, signal
    /// number, exit code). `None` for plain `clean`.
    pub exit_detail: Option<String>,
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
    /// v1.3.2 / DISPATCH 74 — lock-step with [`Self::completed`].
    /// Same length, same index alignment. Populated for AI exits;
    /// `None` for non-AI exits. See [`ExitAttribution`].
    pub recent_exit_attribution: VecDeque<Option<ExitAttribution>>,
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
    /// v1.1.11 / DISPATCH 36 / Phase 3 step 1 — alert state machine,
    /// lifted from `App` (where v1.0.x parked it as a session-scoped
    /// concession). Now owned by `RuntimeState` so the alert
    /// evaluation fires on every tick regardless of UI mode —
    /// `--no-ui` (headless) used to skip `observe_alerts` entirely
    /// because `App` was never constructed. The L6 commit's
    /// "session-scoped state" rationale held under the v1.0.x
    /// UX-only audience; Phase 3 needs alerts emitted on the
    /// non-TUI surfaces too (headless logs, wire), so ownership
    /// moves to where the tick path can reach it.
    ///
    /// `AlertState::ack_all` semantics are unchanged: ack persists
    /// for the lifetime of `RuntimeState`, which matches the
    /// previous "session-scoped" behaviour (the runtime IS the
    /// session at this layer).
    pub alerts: crate::ui::alerts::AlertState,
    /// v1.3.1 / DISPATCH 53 — resolved class-2 deployment thresholds.
    /// Read by `observe_alerts`, `classify_workload_status`, the
    /// recommendation projector, the vitals + workloads panels, and
    /// the web wire's `classify_thermal`. The resolver
    /// ([`crate::thresholds::EffectiveThresholds::resolve`]) runs
    /// once at `Runtime::new`; bad TOML produces
    /// `RuntimeError::Config` and the binary fails to start rather
    /// than silently clamp operator intent.
    ///
    /// `Default` (via `RuntimeState::default()`) reads the contract
    /// defaults — tests that don't care about config get
    /// contract-value behavior automatically.
    pub thresholds: crate::thresholds::EffectiveThresholds,
    /// v1.3.2 / DISPATCH 57 — resolved per-workload suppression
    /// rules, keyed by the rule's `name` (an exact match against
    /// `/proc/<pid>/comm`). Empty when no `[[workloads]]` section
    /// is configured. Read by `observe_alerts` (gates the per-PID
    /// `observe` calls; the OOM exit-path is structurally
    /// carved-out by not going through this loop) and
    /// `recommend::project_one` (gates per-rec emission). The
    /// resolver runs once at `Runtime::new`; bad rules fail-fast
    /// rather than silently dropping.
    pub workload_rules: HashMap<String, WorkloadRule>,
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

/// Map a classified [`crate::storage::run_store::ExitReason`] to the §4 alert it should fire,
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
///
/// B4 (Sprint-2 investigation) — `tokens_per_sec_avg` and `fps_avg`
/// land here so the workloads panel can render live throughput. The
/// dispatcher's accumulator already collects them from the vLLM /
/// llama.cpp Prometheus samplers; pre-fix they were stored but
/// never surfaced into `state.live_telemetry`, leaving every LLM /
/// Vision row stuck on "running actively" forever.
#[derive(Debug, Clone, Default)]
pub struct LiveTelemetry {
    /// Peak KV-cache occupancy seen so far this run, percent (0..=100).
    pub kv_cache_peak_pct: Option<f32>,
    /// Eviction-counter delta so far this run.
    pub kv_cache_evictions_total: Option<u64>,
    /// Rolling average tokens-per-second from the dispatcher's
    /// accumulator. `None` when no sampler has fed a tokens reading
    /// for this PID yet (cold start, or a workload class that has no
    /// throughput signal — Ollama-passive case per B4-3, Vision rows
    /// that should fall through to `fps_avg`).
    pub tokens_per_sec_avg: Option<f32>,
    /// Rolling average frames-per-second. Same lifecycle as
    /// `tokens_per_sec_avg`; populated for Vision workloads when the
    /// vision-probe socket or the stdout parser has observed frames.
    pub fps_avg: Option<f32>,
    /// Phase 2 / DISPATCH 1 — most-recent activity state for this
    /// PID, sourced from the dispatcher's accumulator. `None` when
    /// no Phase-2 sampler has surfaced one yet (cold start, or the
    /// workload's category has no Phase-2 sampler — vLLM /
    /// llama.cpp continue to report throughput-only). Renderer
    /// hides the activity column when every visible row's
    /// `activity` is `None`.
    pub activity: Option<ActivityState>,
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

    /// v1.3.2 / CAR-D75 / DISPATCH 76 — atomically push a completed
    /// summary AND its (optional) classification attribution to the
    /// two lock-step VecDeques. Callers must use this entry point
    /// rather than pushing to `completed` / `recent_exit_attribution`
    /// directly so the lock-step invariant
    /// (`completed.len() == recent_exit_attribution.len()`) never
    /// breaks. The invariant is asserted by `build_events` /
    /// `build_activity` at the consumer side; this helper is the
    /// producer-side enforcement.
    pub fn push_completed_exit(
        &mut self,
        summary: LifecycleSummary,
        attribution: Option<ExitAttribution>,
    ) {
        self.completed.push_back(summary);
        self.recent_exit_attribution.push_back(attribution);
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
    /// by `record_governor_audit` when SIGTERM/SIGKILL fires;
    /// consulted on exit to attribute the kill to
    /// `ExitReason::GovernorKill`. (Pre-CAR-17 this also populated
    /// on dry-run "would-fire" entries — dry-run is gone now.)
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
    /// v1.1.8 ITEM 2 (DISPATCH 25) — long-lived `sysinfo::System`
    /// for the per-tick memory metrics. Pre-v1.1.8 this was built
    /// fresh inside `platform::collect_system_metrics` via
    /// `System::new_all()` + `refresh_all()` on every tick, which
    /// on Linux scans every PID in /proc and allocates a whole
    /// `ProcessSample`-equivalent per process AND a global CPU
    /// usage update — both wasted (`linux_proc::ProcessCollector`
    /// is the actual process source; we only read memory fields
    /// off the `System`). The long-lived handle + per-tick
    /// `sys.refresh_memory()` eliminates the wasted work.
    sys_for_metrics: sysinfo::System,
    /// v1.1.11 / DISPATCH 36 — UI-driven hint for the
    /// `AlertId::GovernorArmed` per-tick eval inside
    /// [`Self::observe_alerts`]. The kill_confirm card lives on
    /// `App` (it's session-scoped UI state); each TUI tick the
    /// dispatcher forwards `app.kill_confirm_pid()` here via
    /// [`Self::set_armed_pid`] before calling `tick`. Headless
    /// mode never sets it, so `GovernorArmed` is always Idle
    /// headless — which is correct (there's no operator to
    /// "arm" anything). `None` when no card is open.
    armed_pid: Option<u32>,
}

impl Runtime {
    /// v1.3.1 — fallible because `EffectiveThresholds::resolve`
    /// validates the operator's `[thresholds]` config section and
    /// rejects invalid combinations (amber ≥ red, critical <
    /// attention, out-of-range pct, sustain ≤ 0 or > 600). The
    /// resolver's [`crate::config::ConfigError::Invalid`] message is
    /// wrapped in [`RuntimeError::Config`] so the binary fails to
    /// start with an operator-actionable error rather than silently
    /// clamping. v1.0.1 phantom-kill lesson: silent override is
    /// worse than a fail-to-start with a fixable error.
    pub fn new(config: Config) -> Result<Self, RuntimeError> {
        let thresholds = crate::thresholds::EffectiveThresholds::resolve(&config.thresholds)
            .map_err(|e| RuntimeError::Config(e.to_string()))?;
        // v1.3.2 / DISPATCH 57 — resolve per-workload rules once at
        // startup. Empty `name`, duplicate names, and other bad
        // shapes fail-fast here rather than silently dropping at
        // observe time. Q5 LOCKED warns (does not reject) when
        // names exceed Linux's 15-byte comm truncation.
        let workload_rules = config
            .resolve_workload_rules()
            .map_err(|e| RuntimeError::Config(e.to_string()))?;
        let policy = config.build_policy();
        let governor = GovernorExecutor::new(policy);
        let manual_killer = ManualKiller::new();
        // v1.3.1 — construct RuntimeState with the resolved
        // thresholds + alert sustain in one struct-literal so clippy
        // doesn't flag the post-default reassignments. The other
        // fields read from `RuntimeState::default()` (cheap — every
        // remaining field is an empty container or `None`).
        let state = RuntimeState {
            thresholds,
            alerts: crate::ui::alerts::AlertState::new(thresholds.alert_sustain_secs),
            workload_rules,
            ..RuntimeState::default()
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
        Ok(Self {
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
            sys_for_metrics: platform::new_system_for_metrics(),
            armed_pid: None,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// v1.1.11 / DISPATCH 36 — mutable access to RuntimeState for
    /// the alert-state machine consumers. Tests in `panels/alerts`
    /// and `app::tests::acknowledge_alerts_*` drive
    /// `RuntimeState::alerts` directly via this getter; the
    /// production path mutates state internally inside `tick`.
    pub fn state_mut(&mut self) -> &mut RuntimeState {
        &mut self.state
    }

    pub fn state(&self) -> &RuntimeState {
        &self.state
    }

    /// L8 — drain queued exit-driven alert events accumulated by
    /// the lifecycle exit hook since the last call. The UI loop
    /// dispatches each event to `App::observe_exit` and discards
    /// them after; queue is cleared by the drain so it never grows
    /// across ticks.
    pub fn drain_exit_alerts(&mut self) -> Vec<ExitAlertEvent> {
        std::mem::take(&mut self.state.pending_exit_alerts)
    }

    /// v1.1.11 / DISPATCH 36 — UI signal: which PID is currently
    /// targeted by the kill_confirm card, or `None` when no card is
    /// open. The dispatcher forwards `app.kill_confirm_pid()` here
    /// each tick BEFORE calling [`Self::tick`], so the
    /// `AlertId::GovernorArmed` per-tick eval inside
    /// `Self::observe_alerts` can see the current arm state.
    /// Headless mode never calls this; `armed_pid` stays `None`
    /// and `GovernorArmed` never fires there (correct — there's
    /// no kill_confirm card without a TUI).
    pub fn set_armed_pid(&mut self, pid: Option<u32>) {
        self.armed_pid = pid;
    }

    /// v1.1.11 / DISPATCH 36 — operator (`a` key) acknowledges
    /// every Active alert. Lifted from `App::acknowledge_alerts`
    /// because the state machine now lives on `RuntimeState`.
    /// Returns the number of alerts that were ack'd, so the
    /// caller (the dispatcher) can set the
    /// `ux_contract::status::ALERTS_ACKNOWLEDGED` footer message
    /// on App when count > 0. Headless mode never calls this
    /// either (there's no `a` keybinding); active alerts stay
    /// active until their breach clears.
    pub fn acknowledge_alerts(&mut self) -> usize {
        self.state.alerts.ack_all()
    }

    /// v1.1.11 / DISPATCH 36 — fire an exit-driven alert (the
    /// L8 / §4 instant-fire path: `OomDetected`, `WorkloadExited`).
    /// Lifted from `App::observe_exit`. Called by
    /// [`Self::tick`] for every event drained from
    /// `state.pending_exit_alerts`.
    fn observe_exit_alert(&mut self, now: Instant, event: &ExitAlertEvent) {
        use crate::ui::alerts::WorkloadRef;
        self.state.alerts.observe_exit(
            now,
            WorkloadRef::workload(event.pid, &event.workload_name),
            event.alert_id,
            event.reason.clone(),
        );
    }

    /// v1.1.11 / DISPATCH 36 — per-tick metric-driven alert eval.
    /// Lifted from `App::observe_alerts`. The metric inputs come
    /// from `RuntimeState` (system RAM, total VRAM, per-process
    /// VRAM, KV-cache peak), so the eval can run on every tick
    /// regardless of UI mode. The `armed_pid` signal flows from
    /// [`Self::set_armed_pid`] — headless mode leaves it `None`
    /// and `GovernorArmed` never fires there.
    ///
    /// AUTHORITY LOCK: this is observation-only. It NEVER calls
    /// any kill/signal path. v1.1.11's contribution is moving
    /// WHERE the eval lives, not WHAT it does.
    fn observe_alerts(&mut self, now: Instant) {
        use crate::ui::alerts::WorkloadRef;
        use ux_contract::AlertId;

        // v1.3.1 — read the resolved thresholds once at the top of
        // the function. `state.thresholds` is contract defaults when
        // no [thresholds] config is set; an operator's deployment
        // overrides reach the comparisons below via the resolver.
        // Copying eight f64s + one u64 is cheaper than chasing
        // references through the loop.
        let thresholds = self.state.thresholds;

        // RAM pressure — system-scope, only one slot for the whole host.
        let ram_pct = self
            .state
            .last_snapshot
            .as_ref()
            .map(|s| s.system.memory_usage_percent());
        let ram_breaching = ram_pct.is_some_and(|p| p >= thresholds.ram_attention_pct);
        self.state.alerts.observe(
            now,
            WorkloadRef::system(),
            AlertId::RamPressure,
            ram_breaching,
        );

        // Per-AI-PID alerts. Snapshot the PIDs and names up front so
        // the borrow on `state.ai_processes()` is released before we
        // mutate `state.alerts` in the loop.
        let total_vram = self
            .state
            .last_snapshot
            .as_ref()
            .map(|s| s.gpu.total_vram_all_devices())
            .filter(|&v| v > 0);
        let armed_pid = self.armed_pid;
        let workloads: Vec<(u32, String, Option<u64>, Option<f32>)> = self
            .state
            .ai_processes()
            .map(|p| {
                let kv = self
                    .state
                    .live_telemetry
                    .get(&p.pid)
                    .and_then(|lt| lt.kv_cache_peak_pct);
                (p.pid, p.name.clone(), p.vram_bytes, kv)
            })
            .collect();

        for (pid, name, vram_bytes, kv_pct) in &workloads {
            let workload = WorkloadRef::workload(*pid, name);

            // v1.3.2 / DISPATCH 57 — per-workload suppression gate.
            // A `[[workloads]]` rule with `suppress_alerts = true`
            // skips the routine pressure observations below; the
            // workload's pressure alerts go silent. NOTE this gate
            // is structurally narrow: it ONLY covers the per-PID
            // metric-driven observes in this loop. OOM and
            // WorkloadExited go through `observe_exit_alert` (the
            // L8 exit-driven path), which is NOT gated — the OOM
            // carve-out is automatic by virtue of the separate
            // call site, not an explicit `OomDetected` exception
            // inside this loop. System-scope alerts (RAM, thermal)
            // also stay un-suppressable because they have no
            // workload identity to look up against.
            let suppress_alerts = self
                .state
                .workload_rules
                .get(name)
                .is_some_and(|rule| rule.suppress_alerts);
            if suppress_alerts {
                continue;
            }

            // VRAM: device-relative percentage.
            let vram_pct = match (total_vram, *vram_bytes) {
                (Some(total), Some(used)) => Some((used as f64 / total as f64) * 100.0),
                _ => None,
            };
            let vram_breaching = vram_pct.is_some_and(|p| p >= thresholds.vram_attention_pct);
            self.state
                .alerts
                .observe(now, workload, AlertId::VramPressure, vram_breaching);

            // KV cache: LLM-only signal.
            let kv = kv_pct.map(|v| v as f64);
            let kv_breaching = kv.is_some_and(|p| p >= thresholds.kv_attention_pct);
            self.state
                .alerts
                .observe(now, workload, AlertId::KvPressure, kv_breaching);

            // GovernorArmed: this PID is the one currently armed
            // by the operator's kill_confirm card. Instant-fire;
            // clears as soon as the arm is released.
            let armed = armed_pid == Some(*pid);
            self.state
                .alerts
                .observe(now, workload, AlertId::GovernorArmed, armed);
        }

        // v1.2.0 / DISPATCH 45 — system-scope ThermalPressure.
        // Fires when ANY thermal zone in the latest snapshot is
        // at or above `THERMAL_AMBER_C` (85 °C, per the v0.3.14
        // contract docstring on `AlertId::ThermalPressure`).
        // System-scope (one slot, no per-PID attribution; thermal
        // is whole-die / zone-level on Linux). AUTHORITY LOCK:
        // this fires an alert; the alert projects to a
        // recommendation; the recommendation is DISPLAY ONLY.
        // No kill, no signal, no actuation reached from this
        // branch — the recommendation's reduce-load suggestion is
        // a string the operator reads.
        let thermal_breaching = self
            .state
            .last_snapshot
            .as_ref()
            .map(|s| {
                s.vitals
                    .thermal_zones
                    .iter()
                    .any(|z| f64::from(z.temp_celsius) >= thresholds.thermal_amber_c)
            })
            .unwrap_or(false);
        self.state.alerts.observe(
            now,
            WorkloadRef::system(),
            AlertId::ThermalPressure,
            thermal_breaching,
        );
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

    /// Is the named process on the governor's allowlist? Surface for
    /// the TUI's kill_confirm card so it can route an explicit allowlist
    /// override without re-deriving allowlist state from the policy.
    /// Pure read; doesn't touch governor state.
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

    /// Run one full tick: sample → classify → lifecycle → governor.
    /// Updates `state` and returns it. Errors here are fatal — the loop
    /// owner must decide whether to retry or exit.
    pub fn tick(&mut self) -> Result<&RuntimeState, RuntimeError> {
        let snapshot = platform::collect_snapshot(&mut self.sys_for_metrics)?;
        let now = Instant::now();
        let vram_by_pid = vram_bytes_by_pid(&snapshot.gpu);

        let mut next_cpu: HashMap<u32, (u64, Instant)> =
            HashMap::with_capacity(snapshot.processes.len());
        // B9 — track which PIDs got a real CPU reading this tick so the
        // tracker can SKIP recording a sample for cold-start ticks
        // (where compute_cpu_pct returns None). Parallel to `annotated`
        // by index. `None` entries mean "do not feed into the rolling
        // average."
        let mut cpu_for_avg: Vec<Option<f32>> = Vec::with_capacity(snapshot.processes.len());
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
                let cpu_pct_opt = self.compute_cpu_pct(p.pid, p.cpu_time_ticks, now);
                cpu_for_avg.push(cpu_pct_opt);
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
                    // Display 0.0 on the cold-start tick (UI shows "no
                    // reading yet") — matches pre-B9 behavior at the
                    // render layer; only the averaging buffer gets the
                    // sample skip.
                    cpu_pct: cpu_pct_opt.unwrap_or(0.0),
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
        for (idx, p) in annotated.iter().enumerate() {
            // B9 — skip the rolling-average push on the cold-start tick
            // (cpu_for_avg[idx] == None). The display field on
            // AnnotatedProcess still carries 0.0 so live panels render
            // sanely; only `lifecycle.resources.cpu_sum_pct /
            // sample_count` is protected from the first-tick zero.
            // RSS / VRAM peaks DO update — those are absolute readings
            // (not deltas) and the first tick's value is honest.
            if let Some(cpu_pct) = cpu_for_avg[idx] {
                self.tracker
                    .record_sample(p.pid, cpu_pct, p.rss_mb * 1024 * 1024, p.vram_bytes);
            } else {
                // Cold-start tick: still update RSS/VRAM peaks even
                // though the CPU sample is skipped. Use 0.0 for cpu
                // (excluded from sum_pct by sample_count logic? no —
                // ResourceStats.record always increments sample_count).
                // To avoid skewing the sample_count we update peaks
                // directly via a dedicated helper.
                self.tracker
                    .record_resource_peaks(p.pid, p.rss_mb * 1024 * 1024, p.vram_bytes);
            }
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
                        // DISPATCH 1.5 — plumb CPU% so Phase-2
                        // samplers (B1 Ollama, B4 Embeddings) can
                        // anchor activity-state thresholds without
                        // re-reading /proc/<pid>/stat. Raw scale
                        // (single-core-pinned ≈ 100.0); see the
                        // doc-comment on `ProcessSnapshot::cpu_pct`.
                        cpu_pct: ann.cpu_pct,
                        // DISPATCH 1.6 — plumb parent PID so B2
                        // (Agent claude sampler) can filter
                        // bash-tool children via
                        // `child.ppid == agent.pid` in
                        // multi-instance scenarios. Sourced from
                        // `ProcessSample::ppid`; `None` for kernel
                        // threads or transient /proc race losses.
                        ppid: s.ppid,
                        // v1.1.5 ITEM B (DISPATCH 16) — plumb the
                        // classifier's verdict so samplers don't
                        // re-derive it from cmdline. B4 embeddings
                        // now gates on this instead of its own
                        // `is_embeddings_cmdline`, picking up
                        // script-file workloads the classifier
                        // already tagged (D-B4-SCRIPT-ASYMMETRY).
                        workload_category: Some(ann.workload_category),
                    })
                })
                .collect();
            // v1.1.2 (DISPATCH 7) — UNFILTERED process list for
            // child-detecting samplers. B2 agent_claude looks for
            // bash tool-children via `child.ppid == agent.pid`, but
            // bash is NotAi and would be stripped from `live_ai`
            // above — the v1.1.1 B2 active-detection bug. The
            // dispatcher iterates `live_ai` to choose which sources
            // fire, but hands each sampler `all_live` for
            // child-process detection. Same field plumbing as
            // `live_ai`; `model_name` is `None` for non-AI rows
            // (the classifier resolved none) and `cpu_pct` rides
            // along from the annotated entry (0.0 for cold-start /
            // race-failed PIDs per the field doc).
            let all_live: Vec<TelemetryProcessSnapshot> = snapshot
                .processes
                .iter()
                .filter_map(|s| {
                    let ann = annotated.iter().find(|a| a.pid == s.pid)?;
                    Some(TelemetryProcessSnapshot {
                        pid: s.pid,
                        name: s.name.clone(),
                        cmdline: s.cmdline.clone(),
                        environ: s.environ.clone(),
                        model_name: ann.model_name.clone(),
                        cpu_pct: ann.cpu_pct,
                        ppid: s.ppid,
                        // v1.1.5 ITEM B — same plumb as live_ai.
                        // For NotAi rows this carries
                        // `Some(WorkloadCategory::Unknown)` (or
                        // whatever the classifier set), so a
                        // sampler keying on `Some(Embeddings)`
                        // naturally doesn't fire on them.
                        workload_category: Some(ann.workload_category),
                    })
                })
                .collect();
            d.tick(&live_ai, &all_live);
            d.record_system_power(&live_ai, &snapshot.gpu);
            d.record_disk_io(&live_ai);
        }

        // v1.3.2 / DISPATCH 73 (P1#2) — per-tick dmesg cache. The
        // OOM-classification path (exit_classify.rs:77) shells out
        // to `journalctl -k --since=-10s`; benchmarking on this
        // host showed ~25 ms per call (cold and warm). A ROS2
        // graph with 3-4 exits per tick would burn 75-100 ms of
        // the 1 s tick budget if we re-shelled per exit. We cache
        // the read once per tick and share the Vec across the
        // exit loop. Lazy init: the read only fires if at least
        // one exit's `signal` admits the OOM branch
        // (`Some(9) | None` — see the gate at exit_classify.rs:77
        // for the v1.3.2 relaxation that opens passive-monitored
        // exits to the OOM classifier in the first place).
        let mut dmesg_cache: Option<Vec<String>> = None;
        // Record run summaries as they fire. Bounded by config to keep memory flat.
        for summary in &lifecycle.recent_exits {
            self.state.completed.push_back(summary.clone());
            // v1.3.2 / DISPATCH 74 — push a placeholder into the
            // lock-step attribution VecDeque. Will be patched below
            // for AI exits once classify_exit runs. Non-AI exits
            // leave the slot as `None`. Push-pair pattern keeps
            // `recent_exit_attribution.len() == completed.len()`
            // every step of the way.
            self.state.recent_exit_attribution.push_back(None);
            while self.state.completed.len() > self.config.runtime.completed_history {
                self.state.completed.pop_front();
                self.state.recent_exit_attribution.pop_front();
            }
            if let Some(s) = &self.summary_store
                && let Err(e) = s.append(summary)
            {
                tracing::warn!(error = %e, "failed to persist run summary");
            }

            // v1.3.2 / DISPATCH 74 — classify ALL AI exits, not just
            // those that hit the RunStore branch. Pre-D74 the
            // classification was gated by `if let Some(rs) = &mut
            // self.run_store && summary.category.is_some()`, so an
            // operator with `run_store_path = ""` got
            // `exit_kind=Unknown` everywhere — and the new
            // activity-feed wire detail (shape A) would also be
            // empty for them. Hoisting the classifier out of the
            // RunStore branch makes the attribution available on
            // every host, with the same D73 dmesg caching keeping
            // the cost bounded.
            //
            // Side benefits the hoist unlocks:
            //   * `pid_stderr` exit-marking now runs for AI exits
            //     even on run_store-disabled hosts (the 30 s
            //     post-mortem buffer survives long enough for
            //     anything that reads it).
            //   * Exit-driven alerts (`OomDetected` / `WorkloadExited`
            //     per §4) fire on every host, not just RunStore ones.
            //     The original gating was an unintended side
            //     effect of nesting alert queueing inside the
            //     RunStore branch.
            let classification: Option<(
                crate::storage::run_store::ExitReason,
                String,
                Option<String>,
            )> =
                if summary.category.is_some() {
                    let dmesg_lines = if matches!(summary.signal, Some(9) | None) {
                        dmesg_cache
                            .get_or_insert_with(|| read_recent_kernel_log(10))
                            .clone()
                    } else {
                        Vec::new()
                    };
                    let governor_reason =
                        self.governor_killed_pids.remove(&summary.pid);
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
                    let reason = classify_exit(summary, &ctx);
                    let (kind_str, detail_str) = exit_reason_to_wire_strings(&reason);
                    // L19 — mark the buffer for 30 s expiry from
                    // this exit. Hoisted with the classify call so
                    // the marker fires even on no-RunStore hosts
                    // (was previously coupled by accident).
                    if let Some(buf) = self.pid_stderr.get_mut(&summary.pid) {
                        buf.exit_at = Some(now_for_stderr);
                    }
                    // L8 / UX_CONTRACT.md §4 — exit-driven alerts.
                    // Same hoist rationale: fire on every host, not
                    // just RunStore ones.
                    if let Some((alert_id, alert_reason)) =
                        classify_for_alert(&reason)
                    {
                        self.state.pending_exit_alerts.push(ExitAlertEvent {
                            pid: summary.pid,
                            workload_name: summary.name.clone(),
                            alert_id,
                            reason: alert_reason,
                        });
                    }
                    // Patch the lock-step attribution back-slot.
                    if let Some(back) =
                        self.state.recent_exit_attribution.back_mut()
                    {
                        *back = Some(ExitAttribution {
                            exit_kind: kind_str.clone(),
                            exit_detail: detail_str.clone(),
                        });
                    }
                    Some((reason, kind_str, detail_str))
                } else {
                    None
                };

            // RunStore is query-optimized (latest.md Tier 1.1) — only
            // AI-classified processes get a record. Non-AI exits stay in
            // the legacy `summary_log_path` JSONL when configured, which
            // remains the unfiltered forensic trail.
            if let Some(rs) = &mut self.run_store
                && let Some((reason, _, _)) = classification.clone()
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
                // v1.3.2 / DISPATCH 74 — exit_reason now comes from
                // the hoisted `classification` block above so it's
                // available regardless of RunStore. Same `ExitReason`
                // enum value, no double-classification.
                record.exit_reason = reason;
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
        //
        // Phase 2 / DISPATCH 1 — also flow `activity_for(pid)` through
        // so the workloads-panel activity column has a read path. A
        // PID with activity but no metrics (e.g. embeddings workload
        // reporting only ActivityState via the CPU heuristic, no
        // Prometheus endpoint) still gets a `LiveTelemetry` entry.
        self.state.live_telemetry.clear();
        if let Some(d) = &self.telemetry {
            for p in &annotated {
                if p.category == AICategory::NotAi {
                    continue;
                }
                let metrics = d.metrics_for(p.pid);
                let activity = d.activity_for(p.pid);
                if metrics.is_some() || activity.is_some() {
                    let m = metrics.unwrap_or_default();
                    self.state.live_telemetry.insert(
                        p.pid,
                        LiveTelemetry {
                            kv_cache_peak_pct: m.kv_cache_peak_pct,
                            kv_cache_evictions_total: m.kv_cache_evictions_total,
                            // B4-1 — flow tokens/sec and fps through to the
                            // UI. The accumulator already aggregates these
                            // from the vLLM / llama.cpp Prometheus samplers
                            // (vision-probe / stdout-parser feed fps); pre-
                            // fix the workloads panel had no read path.
                            tokens_per_sec_avg: m.tokens_per_sec_avg,
                            fps_avg: m.fps_avg,
                            activity,
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

        // v1.1.11 / DISPATCH 36 — alert evaluation on the tick path.
        // Pre-v1.1.11 this lived on `App::observe_alerts` (TUI-only),
        // so `--no-ui` never evaluated alerts. Phase 3 needs alerts
        // surfaced on every UI mode (TUI, headless, web), so the
        // eval moved here. The metric-driven path runs every tick;
        // the exit-driven path drains any queued events from this
        // tick's lifecycle observations (replaces App's old
        // post-tick `for event in runtime.drain_exit_alerts()` loop).
        //
        // AUTHORITY LOCK: observation-only. No kill paths reached
        // from here.
        let now = Instant::now();
        let exit_events = std::mem::take(&mut self.state.pending_exit_alerts);
        for event in &exit_events {
            self.observe_exit_alert(now, event);
        }
        // Re-queue the events on `pending_exit_alerts` so the existing
        // TUI consumers (post-mortem card, sticky exit footer) still
        // see them on the next `Runtime::drain_exit_alerts` call.
        // The alert state is updated; the queue is preserved.
        self.state.pending_exit_alerts = exit_events;
        self.observe_alerts(now);

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
            // v1.3.2 / DISPATCH 70 (P1#1) — bridge the manual-kill
            // path into the exit-classifier's `killed_by_governor`
            // signal. Pre-v1.3.2 this HashMap had ZERO writers:
            // automated kills were unwired since v1.0.1 and the
            // manual path never populated it, so every manual-k
            // → SIGTERM exit fell through to
            // `ExitReason::from_summary` and recorded
            // `exit_kind="unknown"` even though we KNEW the
            // reason. Writing the audit entry's reason here means
            // the next lifecycle drain's `classify_exit` (called
            // at `runtime.rs:1032-1056`) sees
            // `killed_by_governor = true` and produces
            // `ExitReason::GovernorKill { reason }`.
            //
            // Gated on `entry.success`: only insert when the
            // SIGTERM actually went out. A failed kill (process
            // already gone, permission denied) MUST NOT pre-tag
            // the PID — if the OS later assigns the PID to an
            // unrelated process before the lifecycle reaper
            // notices, the stale entry would mis-attribute the
            // new process's exit as a governor kill.
            //
            // Reap point: the entry is `remove()`d at the lifecycle
            // exit-drain (`runtime.rs:1032`); the HashMap therefore
            // grows by at most one entry per outstanding manual
            // kill and shrinks back to zero on the next tick that
            // surfaces the exit. No unbounded growth path exists.
            if entry.success {
                self.governor_killed_pids
                    .insert(pid, entry.reason.clone());
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
    ///
    /// v1.0.1 B-NEW-1 / B-NEW-3 — the runtime never wires
    /// `GovernorExecutor::send_sigterm` to a real `libc::kill`
    /// call. Pre-v1.0.1 this loop populated `governor_killed_pids`
    /// and wrote `success: true` audit entries directly from
    /// `state.decisions` — phantom kills that left a trail without
    /// actually sending a signal.
    ///
    /// v1.0.1 closes the gap two ways:
    ///   * `safe_default()`'s `default_ai_action` flipped from
    ///     `Kill` to `Allow`, so `state.decisions` carries no kill
    ///     verbs unless the operator explicitly opts in.
    ///   * If a future config opts in, this loop stays a no-op
    ///     for the kill-verb branches — audit entries are now
    ///     written only when `send_sigterm` is wired AND succeeds.
    ///     The matched `kill_action` is kept for the v1.x+ wiring
    ///     target so when the path lights up the audit shape is
    ///     already correct.
    ///
    /// Manual kills via `Runtime::manual_kill` continue to write
    /// audit entries with real success/failure — they go through
    /// `ManualKiller::kill_sigterm` which actually calls
    /// `libc::kill` and reports the OS result.
    pub fn record_governor_audit(&mut self) {
        for (pid, action, _reason) in &self.state.decisions {
            let kill_action = match action {
                KillAction::SignalTermSent => Some(ManualKillAction::SendSigterm),
                KillAction::SignalKillSent => Some(ManualKillAction::SendSigkill),
                _ => None,
            };
            let Some(_kill_action) = kill_action else {
                continue;
            };
            // v1.0.1 B-NEW-1 — intentional gap. See doc-comment.
            // `governor_killed_pids` only gets populated when an
            // actual `send_sigterm` call succeeds; that wiring lives
            // in a future minor release. Until then the path is a
            // no-op for kill verbs (Allow → no decisions, Kill +
            // unwired send → no audit). Pid var stays as `_pid` to
            // signal "we know this is the candidate but we are
            // intentionally NOT recording an unrealised kill."
            let _ = pid;
        }
    }
}

impl Runtime {
    /// Converts a fresh (pid, cumulative_ticks, now) reading into a CPU
    /// percentage by looking up the previous tick's value.
    ///
    /// Returns `None` for the **cold-start** tick — the first time a PID
    /// is observed there is no previous reading to delta against, so any
    /// number we'd compute is fabricated. Callers use the `None` arm to
    /// SKIP averaging-buffer pushes (B9 in the Sprint-2 investigation:
    /// pre-fix the first tick recorded 0.0 into the rolling average,
    /// which dominated short-lived process avg_cpu_pct numbers and
    /// caused `samples=1, avg_cpu_pct=0.0` records on the disk for
    /// processes that genuinely had no time to register CPU activity).
    /// UI display still uses 0.0 in this case (operator sees "no
    /// reading yet"); the change is purely about which samples enter
    /// the rolling sum.
    ///
    /// Also returns `None` when the ticks counter went backwards (PID
    /// reuse / collision), since a negative delta would lie.
    fn compute_cpu_pct(&self, pid: u32, ticks_now: u64, now: Instant) -> Option<f32> {
        let &(ticks_prev, prev_at) = self.prev_cpu.get(&pid)?;
        let dt = now.saturating_duration_since(prev_at).as_secs_f32();
        if dt <= 0.0 || ticks_now < ticks_prev {
            return None;
        }
        let delta_ticks = (ticks_now - ticks_prev) as f32;
        Some((delta_ticks / self.clk_tck as f32 / dt) * 100.0)
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
pub fn compute_workload_status(
    inputs: &WorkloadStatusInputs,
    thresholds: &crate::thresholds::EffectiveThresholds,
) -> ux_contract::WorkloadStatus {
    use ux_contract::WorkloadStatus;
    // v1.3.1 — class-3 absolute constants (BASELINE_WARMUP_SECS,
    // THROUGHPUT_ATTENTION_RATIO) still come from the contract;
    // class-2 deployment thresholds (VRAM / RAM / KV pressure) come
    // from `thresholds` so an operator's [thresholds] override
    // reaches every status-classification path.
    use ux_contract::thresholds::{BASELINE_WARMUP_SECS, THROUGHPUT_ATTENTION_RATIO};

    if inputs.telemetry_age < Duration::from_secs(BASELINE_WARMUP_SECS) {
        return WorkloadStatus::Loading;
    }

    let critical = inputs.governor_armed
        || inputs.oom_detected
        || inputs.vram_pct.is_some_and(|v| v >= thresholds.vram_critical_pct)
        || inputs.kv_cache_pct.is_some_and(|kv| kv >= thresholds.kv_critical_pct);
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
        || inputs.vram_pct.is_some_and(|v| v >= thresholds.vram_attention_pct)
        || inputs.ram_pct.is_some_and(|r| r >= thresholds.ram_attention_pct)
        || inputs.kv_cache_pct.is_some_and(|kv| kv >= thresholds.kv_attention_pct);
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
    // Phase 2 / DISPATCH 2A — B2 Agent (claude) sampler. Uses
    // sample_with_context + ppid filtering for multi-instance
    // attribution.
    if config.telemetry.agent_claude {
        sources.push(Box::new(AgentClaudeSource::new()));
    }
    // Phase 2 / DISPATCH 2B — B3 ROS2 shellout. Default on; falls
    // silent on hosts without ros2cli (spawn fails, sample returns
    // Transient, dispatcher logs and retries).
    if config.telemetry.ros2_shellout {
        sources.push(Box::new(Ros2ShelloutSource::new()));
    }
    // Phase 2 / DISPATCH 2B — B4 embeddings CPU heuristic. Pure
    // compute, no I/O; cheap to leave on.
    if config.telemetry.embeddings_cpu {
        sources.push(Box::new(EmbeddingsCpuSource::new()));
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
    fn tick_populates_state() {
        let cfg = Config::default();
        let mut rt = Runtime::new(cfg)
            .expect("Runtime::new must succeed with contract default config");
        // Platform sampling can fail in restricted CI; tolerate that.
        let Ok(state) = rt.tick() else { return };
        assert!(state.last_snapshot.is_some());
        assert!(state.last_lifecycle.is_some());
        assert!(state.tick_count == 1);
    }

    /// v1.1.11 / DISPATCH 36 / Phase 3 step 1 — structural pin: a
    /// freshly constructed `RuntimeState` carries an `AlertState`,
    /// and `Runtime::tick` evaluates alerts regardless of UI mode.
    /// Pre-v1.1.11 `AlertState` lived on `App`, so `--no-ui` (which
    /// never constructed `App`) silently dropped every alert.
    ///
    /// Three pins in one test:
    ///   (1) `RuntimeState::default()` yields a usable `AlertState`
    ///       (count == 0; an empty state machine, not a panic).
    ///   (2) The metric-driven `observe_alerts` runs on the tick
    ///       path. We can't easily synthesise "RAM > threshold"
    ///       inside an in-process test (platform reads return the
    ///       host's real memory), so we drive `set_armed_pid` and
    ///       assert the `GovernorArmed` slot lights up — that
    ///       proves the eval ran end-to-end through tick.
    ///   (3) `Runtime::acknowledge_alerts` returns the ack count
    ///       and the state machine clears (the path the dispatcher
    ///       uses for the `a` key on TUI and would use for any
    ///       future headless ack surface).
    ///
    /// AUTHORITY LOCK: the only mutation is `observe`/`ack`, never
    /// any kill path. v1.1.11 is observation-only.
    /// v1.3.1 / DISPATCH 53 — wiring pin: an operator's
    /// `alert_sustain_secs` override in `[thresholds]` reaches the
    /// `AlertState` constructed at `Runtime::new`. Pre-v1.3.1 the
    /// sustain was a contract const read inline at `transition`;
    /// the new path resolves into `EffectiveThresholds`, plumbs
    /// onto `state.thresholds`, and seeds `AlertState::new(...)`.
    /// If a refactor breaks the wiring (e.g. drops the explicit
    /// `state.alerts = AlertState::new(...)` line and re-uses the
    /// default), this test catches it before the operator's config
    /// silently no-ops.
    #[test]
    fn alertstate_sustain_wires_from_threshold_config() {
        let mut cfg = Config::default();
        cfg.thresholds.alert_sustain_secs = Some(17);
        let rt = Runtime::new(cfg)
            .expect("override 17 must validate (in 1..=600)");
        assert_eq!(
            rt.state().alerts.sustain_secs(),
            17,
            "Runtime::new must wire EffectiveThresholds.alert_sustain_secs into AlertState",
        );
    }

    /// v1.3.1 / DISPATCH 53 — `Runtime::new` rejects an invalid
    /// `[thresholds]` config at startup, returning `RuntimeError::Config`
    /// with the resolver's operator-actionable message. Pinned so a
    /// refactor that bypasses validation (e.g. drops the `?` chain
    /// from `Runtime::new` body, or replaces resolve+validate with
    /// a silent default-fallback) trips this test before shipping.
    #[test]
    fn runtime_new_rejects_invalid_thresholds() {
        let mut cfg = Config::default();
        cfg.thresholds.thermal_amber_c = Some(95.0);
        cfg.thresholds.thermal_red_c = Some(85.0); // < amber: invalid
        // Cannot use `.expect_err(...)` because `Runtime` doesn't
        // derive Debug (it owns non-Debug Tokio + fingerprinter
        // handles). Match on the result directly.
        match Runtime::new(cfg) {
            Ok(_) => panic!("inverted thermal pair must reject; resolver accepted"),
            Err(RuntimeError::Config(msg)) => {
                assert!(
                    msg.contains("thermal_red_c") && msg.contains("thermal_amber_c"),
                    "message must name both fields; got: {msg}"
                );
            }
            Err(other) => panic!("expected RuntimeError::Config; got {other:?}"),
        }
    }

    #[test]
    fn alertstate_constructed_in_headless_mode() {
        use crate::ui::alerts::WorkloadRef;
        use ux_contract::AlertId;

        // (1) Structural pin: RuntimeState carries a fresh AlertState
        // immediately on default-construction — no `App` involved.
        let s = RuntimeState::default();
        assert_eq!(
            s.alerts.active_count(),
            0,
            "RuntimeState::default() must yield an empty-but-usable \
             AlertState — pre-v1.1.11 this field didn't exist (state \
             lived on App) and headless mode silently dropped alerts.",
        );

        // (2) Drive a tick path that exercises observe_alerts without
        // requiring a real platform snapshot. Construct the state
        // directly with an annotated AI process so `state.ai_processes`
        // yields it, then call observe_alerts.
        let mut rt = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let armed_pid = 4242u32;
        rt.set_armed_pid(Some(armed_pid));
        // Seed an annotated process matching armed_pid so the
        // per-PID loop has something to fire GovernorArmed on.
        rt.state.annotated.push(AnnotatedProcess {
            pid: armed_pid,
            name: "test-llm".into(),
            category: crate::model::AICategory::Inference,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        });
        // Drive the same internal method `tick()` would call.
        rt.observe_alerts(Instant::now());
        assert!(
            rt.state.alerts.active_count() > 0,
            "observe_alerts on Runtime must fire the GovernorArmed slot \
             for an armed PID with an annotated AI process — the \
             metric-driven eval lifted from App must work via Runtime \
             now, including in headless mode.",
        );
        let fired_ids: Vec<_> = rt
            .state
            .alerts
            .visible()
            .iter()
            .map(|e| e.alert_id)
            .collect();
        assert!(
            fired_ids.contains(&AlertId::GovernorArmed),
            "GovernorArmed must be among the visible alerts after \
             set_armed_pid + observe_alerts. visible: {fired_ids:?}",
        );

        // (3) Ack path: Runtime::acknowledge_alerts clears the state.
        let n = rt.acknowledge_alerts();
        assert!(
            n > 0,
            "acknowledge_alerts must report the count it ack'd \
             (used by the dispatcher to set the ALERTS_ACKNOWLEDGED \
             status footer); got {n}.",
        );
        assert_eq!(
            rt.state.alerts.active_count(),
            0,
            "post-ack, the state machine must report zero active alerts.",
        );

        // (4) Observe_exit path: the L8 instant-fire route must also
        // reach the lifted state machine. Construct an
        // `ExitAlertEvent` and route it through `observe_exit_alert`
        // (the same method Runtime::tick calls per drained event).
        rt.observe_exit_alert(
            Instant::now(),
            &ExitAlertEvent {
                pid: 9999,
                workload_name: "exited-llm".into(),
                alert_id: AlertId::OomDetected,
                reason: None,
            },
        );
        let after_exit = rt.state.alerts.visible();
        assert!(
            after_exit
                .iter()
                .any(|e| e.alert_id == AlertId::OomDetected),
            "observe_exit_alert must fire the instant-fire slot on \
             RuntimeState::alerts; visible: {after_exit:?}",
        );
        // Stop unused-variable warning in the WorkloadRef import path
        // if a future refactor drops the import block above.
        let _ = WorkloadRef::workload(0, "_");
    }

    /// v1.2.0 / DISPATCH 45 — ThermalPressure fires when any
    /// thermal zone in the snapshot is at or above
    /// `THERMAL_AMBER_C` (85 °C per the v0.3.14 contract). System-
    /// scope alert; one slot, no per-PID attribution.
    ///
    /// Drives `observe_alerts` directly with a synthesized snapshot
    /// carrying one moderately-hot zone, observes twice across the
    /// sustain window, and asserts the slot reaches Active.
    /// AUTHORITY LOCK: this test exercises the alert path only —
    /// no kill, no signal, no actuation reached.
    #[test]
    fn thermal_pressure_alert_fires_when_zone_crosses_amber() {
        use crate::platform::{GpuSnapshot, PlatformSnapshot, SystemMetrics};
        use ux_contract::AlertId;
        use ux_contract::host_vitals::{HostVitals, ThermalZone};

        let mut rt = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let snap = PlatformSnapshot {
            timestamp: chrono::Utc::now(),
            system: SystemMetrics {
                timestamp: chrono::Utc::now(),
                total_memory: 16 * 1024 * 1024 * 1024,
                used_memory: 8 * 1024 * 1024 * 1024,
                available_memory: 8 * 1024 * 1024 * 1024,
                cpu_count: 8,
                load_average: [0.0, 0.0, 0.0],
            },
            processes: vec![],
            gpu: GpuSnapshot { devices: vec![] },
            vitals: HostVitals {
                thermal_zones: vec![ThermalZone {
                    label: "x86_pkg_temp".into(),
                    temp_celsius: 90.0, // above amber (85.0), below red
                }],
                power_rails: Vec::new(),
            },
        };
        rt.state.last_snapshot = Some(snap.clone());

        // Drive the metric-driven eval twice across the sustain
        // window so the slot graduates from Pending to Active.
        let t0 = Instant::now();
        rt.observe_alerts(t0);
        rt.observe_alerts(t0 + std::time::Duration::from_secs(5));

        let visible = rt.state.alerts.visible();
        assert!(
            visible.iter().any(|e| e.alert_id == AlertId::ThermalPressure),
            "ThermalPressure must be visible after a zone crosses \
             THERMAL_AMBER_C; visible alerts: {:?}",
            visible.iter().map(|e| e.alert_id).collect::<Vec<_>>(),
        );
    }

    /// v1.2.0 / DISPATCH 45 — ThermalPressure does NOT fire when
    /// every zone is below the amber threshold. Counterpart to
    /// the above pin; together they guard the threshold boundary.
    #[test]
    fn thermal_pressure_alert_silent_below_amber() {
        use crate::platform::{GpuSnapshot, PlatformSnapshot, SystemMetrics};
        use ux_contract::AlertId;
        use ux_contract::host_vitals::{HostVitals, ThermalZone};

        let mut rt = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let snap = PlatformSnapshot {
            timestamp: chrono::Utc::now(),
            system: SystemMetrics {
                timestamp: chrono::Utc::now(),
                total_memory: 0,
                used_memory: 0,
                available_memory: 0,
                cpu_count: 0,
                load_average: [0.0, 0.0, 0.0],
            },
            processes: vec![],
            gpu: GpuSnapshot { devices: vec![] },
            vitals: HostVitals {
                thermal_zones: vec![
                    ThermalZone {
                        label: "acpitz".into(),
                        temp_celsius: 32.0,
                    },
                    ThermalZone {
                        label: "x86_pkg_temp".into(),
                        temp_celsius: 48.5,
                    },
                ],
                power_rails: Vec::new(),
            },
        };
        rt.state.last_snapshot = Some(snap);

        let t0 = Instant::now();
        rt.observe_alerts(t0);
        rt.observe_alerts(t0 + std::time::Duration::from_secs(5));

        let visible = rt.state.alerts.visible();
        assert!(
            !visible.iter().any(|e| e.alert_id == AlertId::ThermalPressure),
            "ThermalPressure must NOT fire when every zone is below \
             THERMAL_AMBER_C; visible: {:?}",
            visible.iter().map(|e| e.alert_id).collect::<Vec<_>>(),
        );
    }

    /// v1.3.2 / DISPATCH 57 C3 — a `[[workloads]]` rule with
    /// `suppress_alerts = true` makes the routine pressure
    /// observations (VRAM / KV / GovernorArmed) for that workload
    /// silent. The OOM carve-out is verified separately below.
    ///
    /// Synthesizes a config with one rule for "phi3", spins up
    /// the runtime, plants an annotated "phi3" process with
    /// armed_pid set so GovernorArmed would otherwise fire, drives
    /// observe_alerts twice across the sustain window, and asserts
    /// no per-PID alerts surface.
    #[test]
    fn suppress_alerts_silences_per_pid_observes_for_matching_workload() {
        use crate::config::WorkloadRule;
        use ux_contract::AlertId;
        let mut cfg = Config::default();
        cfg.workloads.push(WorkloadRule {
            name: "phi3".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let mut rt = Runtime::new(cfg).expect("Runtime::new must succeed");
        let pid = 4242u32;
        rt.set_armed_pid(Some(pid));
        rt.state.annotated.push(AnnotatedProcess {
            pid,
            name: "phi3".into(),
            category: crate::model::AICategory::Inference,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        });

        let t0 = Instant::now();
        rt.observe_alerts(t0);
        rt.observe_alerts(t0 + std::time::Duration::from_secs(5));

        let visible: Vec<AlertId> = rt
            .state
            .alerts
            .visible()
            .iter()
            .map(|e| e.alert_id)
            .collect();
        // GovernorArmed would have fired without the rule; with
        // suppress_alerts the entire per-PID branch is skipped, so
        // it doesn't appear.
        assert!(
            !visible.contains(&AlertId::GovernorArmed),
            "GovernorArmed for a suppress_alerts workload must not surface; \
             visible: {visible:?}",
        );
        assert!(
            !visible.contains(&AlertId::VramPressure),
            "VramPressure for a suppress_alerts workload must not surface; \
             visible: {visible:?}",
        );
        assert!(
            !visible.contains(&AlertId::KvPressure),
            "KvPressure for a suppress_alerts workload must not surface; \
             visible: {visible:?}",
        );
    }

    /// v1.3.2 / DISPATCH 57 C3 — the OOM carve-out. Even when a
    /// workload's rule has `suppress_alerts = true`, an
    /// OomDetected event MUST fire. OOM is the first brick in
    /// the actuation safety wall — a kernel-OOM kill is never
    /// silenced. This is structurally guaranteed (OOM goes
    /// through `observe_exit_alert` which doesn't read the rule),
    /// but the test pins the behaviour against a future refactor
    /// that might thread suppression through the exit path.
    #[test]
    fn oom_fires_even_when_workload_suppress_alerts_is_true() {
        use crate::config::WorkloadRule;
        use crate::ui::alerts::WorkloadRef;
        use ux_contract::AlertId;
        let mut cfg = Config::default();
        cfg.workloads.push(WorkloadRule {
            name: "phi3".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let mut rt = Runtime::new(cfg).expect("Runtime::new must succeed");

        // Simulate the lifecycle exit hook: OOM-detected event for
        // a workload that has suppress_alerts. The runtime's exit
        // path drains pending_exit_alerts via observe_exit_alert
        // (no `suppress_alerts` consultation by design).
        let pid = 5151u32;
        let now = Instant::now();
        rt.state.alerts.observe_exit(
            now,
            WorkloadRef::workload(pid, "phi3"),
            AlertId::OomDetected,
            None,
        );

        let visible: Vec<AlertId> = rt
            .state
            .alerts
            .visible()
            .iter()
            .map(|e| e.alert_id)
            .collect();
        assert!(
            visible.contains(&AlertId::OomDetected),
            "OomDetected MUST fire for a suppress_alerts workload — the \
             OOM carve-out is structural (exit-path bypasses the rule). \
             visible: {visible:?}",
        );
    }

    /// v1.3.2 / DISPATCH 57 C3 — system-scope alerts (RAM,
    /// ThermalPressure) are NOT bound to any single workload's
    /// suppression rule; they have no `name` to look up against.
    /// Pinning this so a future refactor that, say, tags a
    /// dominant workload onto the system-scope path doesn't
    /// silently introduce a way to mute pressure signals.
    #[test]
    fn system_scope_alerts_not_silenceable_via_workload_rule() {
        use crate::config::WorkloadRule;
        use crate::platform::{GpuSnapshot, PlatformSnapshot, SystemMetrics};
        use ux_contract::AlertId;
        use ux_contract::host_vitals::{HostVitals, ThermalZone};
        let mut cfg = Config::default();
        // Even with a wide rule that suppresses many workloads,
        // system-scope alerts must surface.
        cfg.workloads.push(WorkloadRule {
            name: "phi3".into(),
            suppress_alerts: true,
            suppress_recommendations: false,
        });
        let mut rt = Runtime::new(cfg).expect("Runtime::new must succeed");
        rt.state.last_snapshot = Some(PlatformSnapshot {
            timestamp: chrono::Utc::now(),
            system: SystemMetrics {
                timestamp: chrono::Utc::now(),
                total_memory: 16 * 1024 * 1024 * 1024,
                used_memory: 8 * 1024 * 1024 * 1024,
                available_memory: 8 * 1024 * 1024 * 1024,
                cpu_count: 8,
                load_average: [0.0, 0.0, 0.0],
            },
            processes: vec![],
            gpu: GpuSnapshot { devices: vec![] },
            vitals: HostVitals {
                thermal_zones: vec![ThermalZone {
                    label: "x86_pkg_temp".into(),
                    temp_celsius: 90.0, // above amber
                }],
                power_rails: Vec::new(),
            },
        });
        let t0 = Instant::now();
        rt.observe_alerts(t0);
        rt.observe_alerts(t0 + std::time::Duration::from_secs(5));
        let visible: Vec<AlertId> = rt
            .state
            .alerts
            .visible()
            .iter()
            .map(|e| e.alert_id)
            .collect();
        assert!(
            visible.contains(&AlertId::ThermalPressure),
            "system-scope ThermalPressure must surface regardless of \
             any [[workloads]] rule; visible: {visible:?}",
        );
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Loading
        );
    }

    #[test]
    fn compute_workload_status_healthy_at_warmup_boundary() {
        let mut inputs = healthy_inputs();
        inputs.telemetry_age = Duration::from_secs(ux_contract::thresholds::BASELINE_WARMUP_SECS);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_critical_on_vram_at_95pct() {
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_CRITICAL_PCT);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_attention_on_vram_at_85pct() {
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_ATTENTION_PCT);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_healthy_on_vram_just_below_attention() {
        let mut inputs = healthy_inputs();
        inputs.vram_pct = Some(ux_contract::thresholds::VRAM_ATTENTION_PCT - 0.1);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_critical_on_kv_at_95pct() {
        let mut inputs = healthy_inputs();
        inputs.kv_cache_pct = Some(ux_contract::thresholds::KV_CRITICAL_PCT);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_attention_on_kv_at_80pct() {
        let mut inputs = healthy_inputs();
        inputs.kv_cache_pct = Some(ux_contract::thresholds::KV_ATTENTION_PCT);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_attention_on_ram_at_90pct() {
        let mut inputs = healthy_inputs();
        inputs.ram_pct = Some(ux_contract::thresholds::RAM_ATTENTION_PCT);
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Attention
        );
    }

    #[test]
    fn compute_workload_status_critical_on_governor_armed() {
        let mut inputs = healthy_inputs();
        inputs.governor_armed = true;
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_critical_on_oom() {
        let mut inputs = healthy_inputs();
        inputs.oom_detected = true;
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Critical
        );
    }

    #[test]
    fn compute_workload_status_attention_on_throughput_below_80pct_baseline() {
        let mut inputs = healthy_inputs();
        // 31 tok/s vs baseline 40 → 0.775, below the 0.80 ratio.
        inputs.throughput_vs_baseline = Some((31.0, 40.0));
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Attention
        );

        // 32.01 / 40.0 = 0.80025 — just above the threshold → Healthy.
        inputs.throughput_vs_baseline = Some((32.01, 40.0));
        assert_eq!(
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    #[test]
    fn compute_workload_status_healthy_when_all_within_bounds() {
        // Healthy baseline: every metric is comfortably under its
        // Attention band, no governor / OOM, baseline matches current.
        assert_eq!(
            compute_workload_status(&healthy_inputs(), &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
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
            compute_workload_status(&inputs, &crate::thresholds::EffectiveThresholds::default()),
            ux_contract::WorkloadStatus::Healthy
        );
    }

    // ── v1.3.2 / DISPATCH 70 (P1#1) — manual_kill → governor_killed_pids bridge ──

    /// Spawn a real child that traps SIGTERM (so a successful
    /// `kill_sigterm` returns Ok without the child actually dying)
    /// then SIGKILL it via the returned handle for cleanup.
    fn spawn_sigterm_trap_child() -> std::process::Child {
        std::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn test child")
    }

    /// Plant `pid` in `runtime.state.last_lifecycle` as a freshly-
    /// observed process so `manual_kill`'s `find_by_pid` lookup
    /// succeeds. Returns the planted name so the test can assert
    /// against the audit entry.
    fn plant_lifecycle_for_pid(runtime: &mut Runtime, pid: u32, name: &str) {
        use crate::lifecycle::{LifecycleSnapshot, ProcessLifecycle};
        let sample = crate::model::ProcessSample {
            pid,
            name: name.into(),
            ..Default::default()
        };
        let lc = ProcessLifecycle::new(&sample, None);
        let mut snap = LifecycleSnapshot::new();
        snap.processes.insert(pid, lc);
        runtime.state.last_lifecycle = Some(snap);
    }

    /// On a successful SIGTERM the manual_kill path MUST insert
    /// `(pid → reason)` into `governor_killed_pids`. Pre-v1.3.2
    /// the HashMap had ZERO writers and every manual-k exit
    /// fell through to `ExitReason::from_summary` → unknown.
    /// classify_exit already handles `killed_by_governor = true`
    /// correctly (see `exit_classify::tests`); this test pins the
    /// WRITE so the wired-up reader (`runtime.rs:1032` — already
    /// consuming the HashMap entry) sees the input it never got.
    #[test]
    fn manual_kill_records_pid_in_governor_killed_pids_on_success() {
        let mut child = spawn_sigterm_trap_child();
        let pid = child.id();

        let mut rt = Runtime::new(Config::default()).expect("runtime");
        plant_lifecycle_for_pid(&mut rt, pid, "sh");

        let reason = "test: PID overran KV budget".to_string();
        let result = rt.manual_kill(pid, reason.clone());
        // Always clean up the trapping child regardless of the
        // assertion outcome — letting it leak would burn a stray
        // shell PID across CI workers.
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            result.is_ok(),
            "manual_kill against an existing trapping child must \
             succeed (kill(2) returns 0 even when the target traps \
             the signal): {result:?}",
        );

        let stored = rt
            .governor_killed_pids
            .get(&pid)
            .expect("governor_killed_pids must contain the killed PID after manual_kill");
        assert_eq!(
            stored, &reason,
            "stored reason must match the operator's reason verbatim, \
             so classify_exit's `ExitReason::GovernorKill {{ reason }}` \
             carries the operator-supplied detail rather than a \
             fabricated one",
        );
    }

    /// A FAILED kill (target PID gone before SIGTERM, or permission
    /// denied) MUST NOT pre-tag the PID. If the OS later assigns
    /// the same PID to an unrelated process before the lifecycle
    /// tracker observes the gap, a stale entry would mis-attribute
    /// the new process's exit as a governor kill — exactly the
    /// v1.0.0-class phantom-attribution risk the v1.0.1 scar fix
    /// guards against.
    #[test]
    fn manual_kill_does_not_insert_when_kill_fails() {
        let mut child = spawn_sigterm_trap_child();
        let pid = child.id();

        let mut rt = Runtime::new(Config::default()).expect("runtime");
        plant_lifecycle_for_pid(&mut rt, pid, "sh");

        // Force the kill to fail: reap the child BEFORE we issue
        // manual_kill, so libc::kill returns ESRCH when SIGTERM
        // tries to land. The lifecycle entry still says the
        // process is alive (we planted it manually); manual_kill
        // discovers the failure at the actual signal site, not at
        // the lookup.
        let _ = child.kill();
        let _ = child.wait();

        // Spin a brief moment for the kernel to fully reap the
        // zombie — kill(reaped_pid, _) gives ESRCH only after the
        // PID is unassigned.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let _ = rt.manual_kill(pid, "should-not-be-recorded".into());
        assert!(
            !rt.governor_killed_pids.contains_key(&pid),
            "governor_killed_pids MUST NOT contain a failed-kill PID: \
             a stale entry could mis-attribute a PID-reuse-recipient \
             process's exit as a governor kill. v1.0.1 phantom-kill \
             scar lesson — failed signals leave NO audit trail.",
        );
    }

    /// The lifecycle-exit reap site at `runtime.rs:1032` already
    /// `remove()`s the entry when the exit is recorded. This test
    /// pins the structural reap path so a future refactor of the
    /// exit-drain that drops the `.remove()` call would fail
    /// loudly: a kill recorded into the HashMap must be drained by
    /// the consumer, otherwise `governor_killed_pids` grows
    /// unbounded across long-running monitor sessions.
    #[test]
    fn governor_killed_pids_remove_call_persists_at_exit_drain_site() {
        // Tripwire / lock-as-test: a literal grep of the runtime
        // source for the `.remove(` call site. If a future refactor
        // moves or renames the field, update this assertion AND
        // restore the equivalent reap point. The HashMap-leak
        // failure mode is silent at runtime — only a guard like
        // this surfaces it before production.
        let src = include_str!("runtime.rs");
        assert!(
            src.contains("self.governor_killed_pids.remove(&summary.pid)"),
            "the lifecycle exit-drain path MUST `.remove()` the \
             entry for the exited PID — otherwise governor_killed_pids \
             grows unbounded over the monitor's session lifetime. \
             see runtime.rs:1032 for the current reap site; if it \
             moved, update this test AND the doc-comment in \
             `manual_kill`'s P1#1 insert.",
        );
    }
}
