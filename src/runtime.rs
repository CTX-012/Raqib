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
    /// DISPATCH 80 / C3 — per-PID wall-clock instant of the first
    /// tick at which the PID was observed as VRAM-breaching. The
    /// `record_governor_audit` actuation site reads this to decide
    /// whether `(now - first_breached) >= kill_sustain_secs`. A PID
    /// whose breach clears (no longer in `breaches` with
    /// `vram_breached=true`) is dropped from the map next tick, so
    /// a re-emerging breach restarts the sustain window. Bounded by
    /// concurrent AI workload count (few-dozen max); pruned each
    /// tick alongside the breach update so it can't drift unbounded.
    ///
    /// Honesty: an unmeasured VRAM PID (`vram_pct = None` ⇒
    /// `vram_breached = false`) never appears here — same
    /// "absence is not breach" discipline the projection layer
    /// enforces.
    breach_since: HashMap<u32, Instant>,
    /// v1.3.2 / DISPATCH 86 — optional shared cell holding the
    /// web-tunable settings (thresholds + `kill_sustain_secs`).
    /// `None` when the binary was launched without a web companion
    /// (e.g. `--no-web`); the runtime then uses the static config
    /// values it was constructed with. `Some(_)` ⇒ at the top of
    /// each tick the runtime mirrors the latest tunables into
    /// `state.thresholds` and `config.governor.kill_sustain_secs`
    /// so a web POST takes effect on the very next tick.
    shared_tunables: Option<crate::web::tunables::SharedTunables>,
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
    /// v1.3.2 / DISPATCH 90 / PHASE 5 step 3 — per-PID trajectory
    /// store. Constructed exactly ONCE in [`Self::new`] using the
    /// caps from `runtime.history_trajectory_samples_per_pid` /
    /// `runtime.history_event_archive_cap` (D89 config fields).
    ///
    /// Capture site: the existing `for (idx, p) in annotated.iter()`
    /// loop in [`Self::tick`] (adjacent to the
    /// `self.tracker.record_sample` call). Samples reuse metrics
    /// already in scope at that point — `p.rss_mb` /
    /// `p.vram_bytes` / `cpu_for_avg[idx]` — so capture adds NO new
    /// /proc reads or GPU queries.
    ///
    /// Hand-off site: the exit-drain loop at
    /// `for summary in &lifecycle.recent_exits` — the PID's ring is
    /// `drain_trajectory`d into the `LifecycleSummary` that's pushed
    /// into `state.completed`. The RunStore branch's clone is
    /// EXPLICITLY set to `trajectory: None` so disk records stay
    /// peak-only per the Q3-C design.
    ///
    /// The single ONE-construction-ONE-capture-ONE-drain shape is
    /// pinned by the converted D89 tripwire
    /// `history_capture_is_wired_exactly_once_in_runtime`.
    pub(crate) history: crate::history::History,
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
        // v1.3.2 / DISPATCH 90 / PHASE 5 step 3 — read the doc-locked
        // caps once at construction; the per-tick capture path stays
        // a pure ring push with no config re-reads. Pre-validated by
        // `Config::validate` (zero / above-10× rejected at load time).
        let history = crate::history::History::new(
            config.runtime.history_trajectory_samples_per_pid,
            config.runtime.history_event_archive_cap,
        );
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
            breach_since: HashMap::new(),
            shared_tunables: None,
            prev_cpu: HashMap::new(),
            pid_stderr: HashMap::new(),
            clk_tck: read_clk_tck(),
            sys_for_metrics: platform::new_system_for_metrics(),
            armed_pid: None,
            history,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// v1.3.2 / DISPATCH 94 / PHASE 5 step 6 — accessor for the
    /// history subsystem's read side. Returns a reference to the
    /// live `History` container so the tick-loop refresh in
    /// `main.rs` / `ui/mod.rs` can rebuild the shared web view
    /// without any read living inside `runtime.rs` (the D91
    /// tripwire scans this file's source and forbids the
    /// specific field-access literals for the trajectory / event
    /// archive collections; a bare `&self.history` accessor
    /// doesn't match).
    ///
    /// Callers (currently just `web::history::refresh_shared`) are
    /// responsible for the read-only discipline — the endpoint is
    /// the ONLY consumer.
    ///
    /// Name note: distinct from the pre-existing
    /// `Runtime::history(model, limit)` — that's the RunStore-backed
    /// per-model peaks browser (Tier 1.1, TUI `h` key). This D94
    /// accessor is the in-memory History subsystem's read side.
    pub fn history_capture(&self) -> &crate::history::History {
        &self.history
    }

    /// v1.3.2 / DISPATCH 86 — wire the shared web-tunables cell.
    /// Called from `main.rs` after `Runtime::new` so the runtime
    /// can mirror the latest web POST values into its own state at
    /// the top of each tick.
    pub fn attach_shared_tunables(
        &mut self,
        tunables: crate::web::tunables::SharedTunables,
    ) {
        self.shared_tunables = Some(tunables);
    }

    /// v1.3.2 / DISPATCH 86 — copy the latest shared tunables into
    /// the runtime's authoritative slots so a web POST that landed
    /// between ticks takes effect on the upcoming one. No-op when
    /// no shared cell is attached (e.g. `--no-web`).
    ///
    /// THE BOUNDARY (echoes [`crate::web::tunables::RuntimeTunables`]):
    /// this method ONLY copies thresholds + `kill_sustain_secs`. It
    /// MUST NOT touch `auto_actuate`, `default_ai_action`, or any
    /// other "whether to kill" knob. If it did, the structural
    /// allowlist on the web write surface would be a lie.
    fn apply_pending_tunables(&mut self) {
        let Some(tunables) = &self.shared_tunables else {
            return;
        };
        let guard = match tunables.read() {
            Ok(g) => g,
            // The web handler panicked while holding the write
            // lock — recover the inner value rather than crashing
            // the tick loop. The corrupted-state case is the same
            // as a fresh boot from the operator's perspective.
            Err(poisoned) => poisoned.into_inner(),
        };
        self.state.thresholds = guard.thresholds;
        self.config.governor.kill_sustain_secs = guard.kill_sustain_secs;
        // NOTE the omissions: `auto_actuate`, `default_ai_action`,
        // policy lists, audit history, etc. are NEVER written here.
        // The structural allowlist on `RuntimeTunables` is the
        // single point of truth for what the web can change.
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
        // v1.3.2 / DISPATCH 86 — pull the latest web-tunable values
        // BEFORE this tick reads thresholds. A web POST that landed
        // between the previous tick and now takes effect HERE.
        self.apply_pending_tunables();

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
        let mut annotated: Vec<AnnotatedProcess> = snapshot
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

            // v1.3.2 / DISPATCH 90 / PHASE 5 step 3 — also push a
            // History sample for AI-classified PIDs. REUSE the
            // metrics already in scope from THIS tick — no second
            // /proc read, no second GPU query. The cost is:
            //   * `cpu_for_avg[idx].unwrap_or(0.0)` — already
            //     computed above (cold-start ticks contribute 0.0;
            //     the trajectory's first sample on cold start is
            //     honest about that, mirroring the panel display).
            //   * `p.rss_mb` narrowed to u32 — capped at u32::MAX
            //     (4 GB sample ceiling well above any AI workload;
            //     `min` is saturating, not silently truncating).
            //   * `p.vram_bytes` ⇒ MB (preserving the `None ≠ 0`
            //     honesty rule the D74/D78 VRAM_UNMEASURED display
            //     enforces). NEVER zero-fill an unmeasured reading.
            //
            // AI-only filter: NotAi processes don't get a trajectory
            // (the per-PID ring would balloon the HashMap across
            // every transient shell on the host without any value
            // — non-AI PIDs never receive a `LifecycleSummary` ⇒
            // never project into the history view). Matches the
            // PHASE5 design doc's "for each live AI-tracked PID."
            if p.category != crate::model::AICategory::NotAi {
                let sample = crate::history::Sample {
                    timestamp: chrono::Utc::now(),
                    cpu_pct: cpu_for_avg[idx].unwrap_or(0.0),
                    rss_mb: p.rss_mb.min(u32::MAX as u64) as u32,
                    vram_mb: p
                        .vram_bytes
                        .map(|b| (b / (1024 * 1024)).min(u32::MAX as u64) as u32),
                };
                self.history.record_sample(p.pid, sample);
            }
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

            // v1.3.2 / DISPATCH 107 FIX 3 — sha256 blob → friendly
            // name promotion. The classifier's cmdline-extract path
            // (`augment_with_model_name`) pulls the `--model
            // /root/.ollama/models/blobs/sha256-...` argument out of
            // the ollama runner's cmdline; that's the digest, not a
            // human-readable model tag. The ollama sampler's
            // `/api/ps` sees the friendly name (e.g. `smollm:135m`)
            // and emits it as `model_name_hint`. Promote the hint
            // onto `AnnotatedProcess.model_name` here, but ONLY when
            // the current value looks like a raw sha256 blob — good
            // extractions (llama.cpp `--model /models/foo.gguf` →
            // `foo`) are left untouched.
            //
            // BOARD_AUDIT §2.2 "sha256 name-leak" line closed here.
            // The RunRecord attribution path (line ~1350 below) also
            // reads the hint; both surfaces now converge on the
            // friendly name.
            for ap in annotated.iter_mut() {
                let looks_like_sha_blob = ap
                    .model_name
                    .as_deref()
                    .map(|m| m.starts_with("sha256-"))
                    .unwrap_or(false);
                if !looks_like_sha_blob {
                    continue;
                }
                if let Some(hint) = d.model_name_hint_for(ap.pid) {
                    // Only overwrite when the HINT itself isn't
                    // another sha256 (defensive — an /api/ps that
                    // somehow only knew the digest wouldn't be an
                    // improvement).
                    if !hint.starts_with("sha256-") {
                        ap.model_name = Some(hint);
                    }
                }
            }
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
            // v1.3.2 / DISPATCH 90 / PHASE 5 step 4 — attach the
            // PID's trajectory to the in-memory summary BEFORE
            // pushing it into `state.completed`. This is the Q3-C
            // hand-off: live trajectory lives on `Runtime::history`;
            // on exit it MOVES onto `LifecycleSummary.trajectory` and
            // the per-PID ring is removed from the HashMap. Dead PIDs
            // do not linger — their data follows the lifecycle of the
            // `LifecycleSummary` in `state.completed`, capped by
            // `completed_history`.
            let trajectory = self.history.drain_trajectory(summary.pid);
            let mut summary_for_completed = summary.clone();
            summary_for_completed.trajectory = trajectory;

            // v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — mirror the
            // exit into the cross-PID event archive (cap 500 per
            // D89 config). AI-only filter matches the live activity
            // feed (`build_activity` in web/wire.rs lines 1273-1276
            // and `build_events` in ui/panels/activity.rs lines
            // 194-196) — non-AI exits stay in the persistent
            // summary_log JSONL but don't surface to the operator's
            // history view. Structural dedup: this loop pushes once
            // per `lifecycle.recent_exits` entry; the feed sources
            // are themselves the deduped truth, so no key check is
            // needed here.
            if summary.category.is_some() {
                self.history
                    .record_event(crate::history::exit_event(summary));
            }

            self.state.completed.push_back(summary_for_completed);
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
                // v1.3.2 / DISPATCH 90 / PHASE 5 Q3-C — disk-record
                // discipline. The trajectory rides on
                // `state.completed[i].trajectory` in memory; it MUST
                // NOT bloat the RunStore JSONL records. The source
                // `summary` (from `lifecycle.recent_exits`) carries
                // no trajectory at this point (only the
                // `summary_for_completed` clone above received the
                // attached trajectory), so this assignment is
                // belt-and-suspenders: it pins the invariant against
                // a future refactor that might thread the trajectory
                // through the source struct. `skip_serializing_if =
                // "Option::is_none"` keeps the JSONL bytes
                // identical to pre-D90 records.
                summary_to_record.trajectory = None;
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
                //
                // v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — same scan
                // ALSO mirrors each new regression into the history
                // event archive. Structural dedup: the
                // `iter().skip(regs_before)` slice is exactly the
                // events `check_regressions` added on this tick;
                // each lands in the archive exactly once.
                for ev in self.state.regressions.iter().skip(regs_before) {
                    *self
                        .regressions_count
                        .entry((ev.model.clone(), ev.regression.metric.clone()))
                        .or_insert(0) += 1;
                    self.history
                        .record_event(crate::history::regression_event(ev));
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
            // DISPATCH 81 — pending_kills cleanup. Pre-D80 this map
            // had no production callers so leakage was impossible;
            // post-D80/D81 it's populated by the auto_actuate
            // SIGTERM path and SHOULD shrink when the PID actually
            // exits. Without this, a recycled PID later would see
            // the stale `pending_kills` entry → `AlreadyPending`
            // short-circuit → never get evaluated, and the entry
            // would persist forever. Mirrors the
            // `governor_killed_pids` / `pid_to_model_path` /
            // `pid_first_seen_at` cleanup discipline.
            self.governor.clear_pending(summary.pid);
        }

        // v1.3.2 / DISPATCH 78 / step-3 — build the per-tick
        // threshold-breach projection (Q6 — VRAM%-first) and pass
        // it into the governor alongside the lifecycle snapshot.
        //
        // DISPATCH 84 / step-8 — widened from VRAM-only to:
        //   * Per-PID VRAM% (unchanged)
        //   * Per-PID RAM% (NEW)
        //   * Host-level thermal breach (NEW, max-across-zones)
        //
        // The auto-kill SIGNAL surface and the alert SIGNAL surface
        // read the same projection so an operator who's seen an
        // alert won't be surprised by a different number on the kill
        // decision.
        //
        // Empty-fallback: when no platform snapshot is available
        // yet (very first tick before the platform thread has
        // produced one), we project against empty GPU + None total
        // RAM, which yields `*_pct=None` for every PID → no per-PID
        // breaches → no kill decisions. Honest default.
        let (breaches, host_breach) = if let Some(snap) = self.state.last_snapshot.as_ref() {
            let breaches = crate::governor::threshold_breach::build_threshold_breaches(
                &self.state.annotated,
                &snap.gpu,
                Some(snap.system.total_memory),
                &self.state.thresholds,
            );
            let host_breach = crate::governor::threshold_breach::build_host_breach(
                &snap.vitals,
                &self.state.thresholds,
            );
            (breaches, host_breach)
        } else {
            (
                Vec::new(),
                crate::governor::threshold_breach::HostBreach::default(),
            )
        };

        // DISPATCH 80 / C3 — refresh the per-PID breach-since map
        // before the actuation site reads it.
        //
        // DISPATCH 84 — the breach-since map now tracks ANY of the
        // three signal sources:
        //   1. Per-PID VRAM-breach.
        //   2. Per-PID RAM-breach.
        //   3. Host-level thermal-breach → all AI-classified PIDs
        //      become eligible because the host is shedding load;
        //      the sustain gate then waits kill_sustain_secs of
        //      sustained thermal pressure before firing on any of
        //      them. This matches Q6's framing of thermal as the
        //      "shed load because the system is overheating"
        //      trigger — without inclusion here, a thermal-only
        //      kill could never satisfy the sustain check.
        //
        // Two-step update keeps the map bounded by the currently-
        // breaching PID set; a clear-then-reappear restarts the
        // window (no accumulated sustain credit on flickers).
        let now_for_sustain = Instant::now();
        let mut breaching_pids: std::collections::HashSet<u32> = breaches
            .iter()
            .filter(|b| b.vram_breached || b.ram_breached)
            .map(|b| b.pid)
            .collect();
        if host_breach.thermal_breached {
            for p in &self.state.annotated {
                if p.category != crate::model::AICategory::NotAi {
                    breaching_pids.insert(p.pid);
                }
            }
        }
        for pid in &breaching_pids {
            self.breach_since.entry(*pid).or_insert(now_for_sustain);
        }
        self.breach_since
            .retain(|pid, _| breaching_pids.contains(pid));

        let decisions = self.governor.evaluate(&lifecycle, &breaches, &host_breach);

        // Tier 2.3 — count governor decisions by reason, for the
        // Prometheus exporter. Done before we move `decisions` onto
        // the runtime state.
        for (_pid, action, reason) in &decisions {
            let key = match action {
                KillAction::SignalTermSent => "sigterm".to_string(),
                KillAction::SignalKillSent => "sigkill".to_string(),
                KillAction::Whitelisted => "whitelisted".to_string(),
                KillAction::AlreadyExited => "already_exited".to_string(),
                // v1.3.2 / DISPATCH 77 / 62-E — sibling of
                // `already_exited` for the Prometheus exporter's
                // `kills_by_reason` counter. Currently never
                // increments in production because `pending_kills`
                // stays empty until step-5 of the auto-kill arc
                // wires actuation; reserving the bucket here means
                // the dashboard renderer doesn't need a separate
                // update when that lands.
                KillAction::AlreadyPending => "already_pending".to_string(),
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

    /// Manual-kill entry point used by the TUI keybinding (operator
    /// presses `k` then Enter on the Confirm-state `KillConfirmCard`).
    /// Returns Err if the PID is gone or the kill failed; success is
    /// logged to the audit trail and surfaced in the UI's audit panel.
    ///
    /// ## DISPATCH 83 / C1 — routed through `send_sigterm`
    ///
    /// Pre-D83 this called `ManualKiller::kill_sigterm` directly,
    /// which called `libc::kill(pid, SIGTERM)` raw — so the PID never
    /// landed in `pending_kills`, and the D81 escalation machinery
    /// had nothing to escalate. After D83 the SIGTERM goes through
    /// `governor.send_sigterm`, which:
    ///
    ///   1. Captures pidfd + `/proc/<pid>/stat` starttime BEFORE the
    ///      signal (the v1.0.1 PID-reuse guard).
    ///   2. Populates `pending_kills` with the identity tokens so
    ///      `send_sigkill` can verify them later.
    ///   3. Inserts the PID into the rate-limit window (counted as a
    ///      kill against the per-minute budget, even though it was
    ///      operator-initiated).
    ///
    /// The audit entry is built in-place here with `KillSource::Manual`;
    /// it is NOT routed through `ManualKiller::kill_sigterm`'s
    /// internal audit log (`KillSource::Manual` source is enforced
    /// by the constructors we call, and the audit-trail bridge into
    /// `state.audit` + `audit_writer` + `governor_killed_pids` was
    /// the only reason that path existed).
    ///
    /// ## NOT auto_actuate-gated
    ///
    /// Auto kills require `governor.auto_actuate = true`. This path
    /// does NOT consult that gate — manual kills are operator-driven
    /// and always available (the operator's `k` + Enter IS the
    /// consent surface). The `default_off_emits_zero_kills` and
    /// `default_off_emits_no_sigterm_and_no_sigkill` invariants
    /// remain unaffected because they assert no AUTONOMOUS kill
    /// fires; this path requires deliberate operator action.
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

        // DISPATCH 83 / C1 — manual SIGTERM via the same path as
        // auto-kill, so `pending_kills` carries the identity tokens
        // for any later force-SIGKILL. `send_sigterm` requires a
        // non-optional `AICategory`; when the classifier didn't tag
        // the workload (operator killing a non-AI process), default
        // to `NotAi`, the honest "we didn't classify this" value.
        let send_category = category.unwrap_or(AICategory::NotAi);
        let result = self
            .governor
            .send_sigterm(pid, name.clone(), send_category)
            .map_err(|e| e.to_string());

        let entry = match &result {
            Ok(()) => AuditLogEntry::success(
                ManualKillAction::SendSigterm,
                pid,
                name.clone(),
                category,
                reason.clone(),
            ),
            Err(e) => AuditLogEntry::failure(
                ManualKillAction::SendSigterm,
                pid,
                name.clone(),
                category,
                reason.clone(),
                e.clone(),
            ),
        };

        // Persist the entry to the JSONL trail (when configured)
        // before pushing into the in-memory ring, so a crash between
        // the two leaves the durable trail intact.
        if let Some(w) = &self.audit_writer
            && let Err(e) = w.append(&entry)
        {
            tracing::warn!(error = %e, "failed to persist manual-kill audit entry");
        }

        // v1.3.2 / DISPATCH 70 (P1#1) — bridge the manual-kill path
        // into the exit-classifier's `killed_by_governor` signal so
        // the eventual exit records `ExitReason::GovernorKill { reason }`.
        // Gated on `entry.success`: a failed kill (process gone,
        // permission denied) MUST NOT pre-tag the PID — if the OS
        // later assigns the PID to an unrelated process before the
        // lifecycle reaper notices, the stale entry would
        // mis-attribute the new process's exit as a governor kill.
        if entry.success {
            self.governor_killed_pids
                .insert(pid, entry.reason.clone());
        }
        // v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — mirror this kill
        // into the cross-PID event archive. Build the HistoryEvent
        // BEFORE the move into state.audit so we don't need to
        // re-borrow the entry. Same structural-dedup rationale as
        // the exit-drain push above.
        self.history.record_event(crate::history::kill_event(&entry));
        self.state.audit.push_back(entry);
        while self.state.audit.len() > self.config.runtime.audit_history {
            self.state.audit.pop_front();
        }

        result
    }

    /// DISPATCH 83 / C3 — operator-consent-gated SIGKILL escalation.
    /// Called from the TUI when the operator presses Enter on a
    /// `KillConfirmCard` in `Waiting` state (i.e. a SIGTERM has been
    /// sent and the operator is now consenting to force the
    /// uncatchable kill).
    ///
    /// ## Consent, not auto_actuate
    ///
    /// Unlike the D81 auto-escalation (gated on `auto_actuate`),
    /// this path is gated by the OPERATOR's Enter press at the
    /// dispatch layer (`apply_action` in `src/ui/mod.rs`). Reaching
    /// this method already implies operator consent — there is no
    /// other caller. The `send_sigkill_callers_are_gated` tripwire
    /// pins that invariant: this is the only runtime-direct
    /// `send_sigkill` caller, and it lives inside a function whose
    /// name distinguishes it from `record_governor_audit`.
    ///
    /// ## PID-reuse guard ALWAYS engages
    ///
    /// The SIGKILL goes through `governor.send_sigkill`, which
    /// re-verifies the pidfd / starttime captured at SIGTERM time
    /// (in `manual_kill` above, via `send_sigterm`). Mismatch →
    /// `KillAction::PidReusedAborted`, NO signal sent. This is
    /// CRITICAL for the manual path: the operator may take seconds
    /// or minutes to press Enter, opening a long reuse window where
    /// the kernel could reassign the PID to an unrelated process.
    /// The v1.0.1 hazard is non-negotiable here.
    ///
    /// Returns `Ok(())` only when SIGKILL was actually delivered
    /// (`KillAction::SignalKillSent`). Refusal (`PidReusedAborted`)
    /// and OS errors return `Err` with an operator-actionable
    /// message; the audit trail records both outcomes.
    pub fn manual_force_kill(&mut self, pid: u32) -> Result<(), String> {
        let lifecycle = self
            .state
            .last_lifecycle
            .as_ref()
            .ok_or_else(|| "no snapshot available yet".to_string())?;

        // Identity for the audit entry. Prefer the live lifecycle row
        // (the PID may still be alive — that's exactly why we're
        // force-killing). Fall back to `pending_kills` (captured at
        // manual SIGTERM time) when the lifecycle has already
        // reaped the entry.
        let (name, category) = lifecycle
            .processes
            .get(&pid)
            .map(|lc| (lc.name.clone(), lc.category))
            .unwrap_or_else(|| {
                let pending = self.governor.get_pending_kills();
                let from_pending = pending.iter().find(|p| p.pid == pid);
                let n = from_pending
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "<unknown>".to_string());
                let c = from_pending.map(|p| p.category);
                (n, c)
            });

        let reason = "operator force-kill via TUI (kill_confirm Waiting → Enter)".to_string();
        let kill_result = self.governor.send_sigkill(pid, &name);

        let entry = match &kill_result {
            Ok(KillAction::SignalKillSent) => AuditLogEntry::success(
                ManualKillAction::SendSigkill,
                pid,
                name.clone(),
                category,
                reason.clone(),
            ),
            Ok(KillAction::PidReusedAborted) => AuditLogEntry::failure(
                ManualKillAction::PidReusedAborted,
                pid,
                name.clone(),
                category,
                reason.clone(),
                "PID-reuse guard refused SIGKILL: process exited during grace OR \
                 PID reassigned to an unrelated process — captured identity \
                 tokens (pidfd / starttime) no longer match the live PID"
                    .to_string(),
            ),
            Ok(other) => AuditLogEntry::failure(
                ManualKillAction::SendSigkill,
                pid,
                name.clone(),
                category,
                reason.clone(),
                format!("unexpected SIGKILL outcome: {other:?}"),
            ),
            Err(e) => AuditLogEntry::failure(
                ManualKillAction::SendSigkill,
                pid,
                name.clone(),
                category,
                reason.clone(),
                e.to_string(),
            ),
        };

        if let Some(w) = &self.audit_writer
            && let Err(e) = w.append(&entry)
        {
            tracing::warn!(error = %e, "failed to persist force-kill audit entry");
        }

        let success = entry.success;
        if success {
            self.governor_killed_pids
                .insert(pid, "force-kill via SIGKILL".to_string());
        }
        // v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — mirror into archive.
        self.history.record_event(crate::history::kill_event(&entry));
        self.state.audit.push_back(entry);
        while self.state.audit.len() > self.config.runtime.audit_history {
            self.state.audit.pop_front();
        }

        match kill_result {
            Ok(KillAction::SignalKillSent) => Ok(()),
            Ok(KillAction::PidReusedAborted) => {
                Err("SIGKILL refused by PID-reuse guard".to_string())
            }
            Ok(other) => Err(format!("unexpected SIGKILL outcome: {other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Walk per-tick governor decisions and — when the operator has
    /// opted in — actuate them. Called once per tick by the loop
    /// owner (main.rs / ui/mod.rs) right after `tick()`.
    ///
    /// ## DISPATCH 80 — THE LINE CROSSING
    ///
    /// This function is where the v1.0.1 phantom-kill scar's
    /// **layer 2** (severed tick path) and **layer 3** (audit
    /// silence) come down — gated. **Layer 1** (`default_ai_action
    /// = Allow` at `policy.rs:59`) is UNTOUCHED. Even with this
    /// function fully wired, an out-of-the-box install fires zero
    /// kills because:
    ///
    ///   * Layer 1 (default Allow) ⇒ `state.decisions` carries no
    ///     `SignalTermSent` verbs ⇒ the loop body has nothing to act
    ///     on. Pinned by
    ///     `default_allow_policy_emits_no_signaltermsent_even_with_breaches`.
    ///   * Layer 2 (this gate) ⇒ even if Layer 1 were flipped,
    ///     `config.governor.auto_actuate` defaults `false` ⇒ early
    ///     return below ⇒ byte-identical to v1.3.2 observe-only.
    ///     Pinned by [`tests::default_off_emits_zero_kills`] — the
    ///     headline regression guard.
    ///
    /// TWO independent operator opt-ins are required for ANY kill:
    /// `policy.default_ai_action = Kill` (or a per-workload Kill
    /// blacklist hit) AND `governor.auto_actuate = true`. Removing
    /// EITHER prevents kills. Pinned by
    /// [`tests::two_gate_invariant_holds`].
    ///
    /// ## Sustain (Q3)
    ///
    /// A SignalTermSent decision only actuates when the PID has
    /// been observed as VRAM-breaching for at least
    /// `config.governor.kill_sustain_secs` (default 10 s,
    /// validated `>= alert_sustain_secs`). The breach-since map is
    /// refreshed in `tick()` BEFORE this runs, so a freshly-arriving
    /// breach has `(now - since) ≈ 0` and is held until sustained.
    ///
    /// ## PID-reuse guard
    ///
    /// The actuation calls
    /// [`crate::governor::GovernorExecutor::send_sigterm`], which
    /// captures `pidfd_open` + `/proc/<pid>/stat` starttime BEFORE
    /// sending SIGTERM — the v1.0.1 protection (TEST.md G.1.11). The
    /// subsequent SIGKILL escalation (`execute_after_grace`) checks
    /// the captured identity and aborts when it no longer matches.
    /// This actuation goes THROUGH that guard, not around it.
    ///
    /// ## Web is OUT
    ///
    /// The actuation lives here (tick-loop, owning `&mut Runtime`),
    /// per the standing network-never-in-safety-path lock. The web
    /// thread does NOT drive kills; the web companion remains a
    /// policy editor (writes config TOML; this loop reads it). The
    /// observe-only firewalls 1/2/4 stay intact; firewall 3 (config
    /// schema) is unaffected because `kill_sustain_secs` is a
    /// duration knob, not an action verb.
    ///
    /// ## Audit
    ///
    /// Successful actuations mirror into `state.audit` with
    /// `KillSource::Automated`, persist via `audit_writer` when
    /// configured, and populate `governor_killed_pids` so the
    /// eventual exit is classified `ExitReason::GovernorKill`
    /// (DISPATCH 70 manual-kill bridge — same path, automated
    /// source). Failures (ESRCH, EPERM) record `success=false`
    /// audit entries but do NOT pre-tag the PID — same discipline
    /// as `manual_kill`.
    pub fn record_governor_audit(&mut self) {
        // GATE 1 — operator opt-in. Default false ⇒ no-op. This
        // is THE invariant pinned by `default_off_emits_zero_kills`:
        // when this branch returns, ZERO `libc::kill` syscalls fire
        // from this function, regardless of `state.decisions` shape.
        // Out-of-the-box installs MUST take this branch — that's
        // what makes shipping this dispatch safe.
        if !self.config.governor.auto_actuate {
            return;
        }

        // From here on, the operator has CONSENTED to automated
        // kills. Anything below that fires a signal is intended.
        let kill_sustain = std::time::Duration::from_secs(self.config.governor.kill_sustain_secs);
        let now = Instant::now();
        let audit_history = self.config.runtime.audit_history;

        // Clone the (pid, action, reason) triples we need to act on
        // so the iteration doesn't borrow `self.state.decisions`
        // immutably while the loop body mutates `state.audit` /
        // `governor_killed_pids` / `governor`. The clone is cheap —
        // few-dozen entries at most per tick.
        let candidates: Vec<(u32, String)> = self
            .state
            .decisions
            .iter()
            .filter_map(|(pid, action, reason)| match action {
                KillAction::SignalTermSent => Some((*pid, reason.clone())),
                _ => None,
            })
            .collect();

        for (pid, reason) in candidates {
            // GATE 2 — sustain. A SignalTermSent decision is the
            // governor's "this PID is breaching AND policy says
            // kill," but a momentary breach (e.g. model-load VRAM
            // spike) must never fire. Hold for `kill_sustain_secs`.
            let Some(&since) = self.breach_since.get(&pid) else {
                // No breach-since row: either the breach JUST appeared
                // (insert ran in tick() before decisions; first-seen
                // is `now`) and now.duration_since(now) = 0 < sustain,
                // OR the projection layer didn't surface a breach this
                // tick (race; possible if decisions and breaches drift).
                // Either way: do not kill.
                tracing::warn!(
                    pid,
                    "auto_actuate: SignalTermSent decision lacks a breach-since \
                     entry; skipping (sustain gate not satisfied)"
                );
                continue;
            };
            let observed_for = now.saturating_duration_since(since);
            if observed_for < kill_sustain {
                tracing::debug!(
                    pid,
                    observed_secs = observed_for.as_secs(),
                    sustain_secs = kill_sustain.as_secs(),
                    "auto_actuate: holding for sustain"
                );
                continue;
            }

            // Resolve the workload identity for the audit entry.
            // The annotated set is the authoritative per-PID source
            // for category + name; an annotated row exists for
            // every PID `evaluate()` saw (same upstream feed).
            let (name, category) = match self
                .state
                .annotated
                .iter()
                .find(|a| a.pid == pid)
            {
                Some(a) => (a.name.clone(), Some(a.category)),
                None => {
                    tracing::warn!(
                        pid,
                        "auto_actuate: SignalTermSent decision for PID with no \
                         annotated row; skipping"
                    );
                    continue;
                }
            };

            tracing::warn!(
                pid,
                name = %name,
                reason = %reason,
                "auto_actuate: firing SIGTERM (operator opted in via \
                 governor.auto_actuate = true)"
            );

            // THE ACTUATION. send_sigterm captures pidfd + starttime
            // BEFORE the libc::kill call (PID-reuse guard, v1.0.1).
            let kill_result = self
                .governor
                .send_sigterm(pid, name.clone(), category.unwrap_or(crate::model::AICategory::Inference));

            let entry = match &kill_result {
                Ok(()) => AuditLogEntry::automated_success(
                    ManualKillAction::SendSigterm,
                    pid,
                    name.clone(),
                    category,
                    reason.clone(),
                ),
                Err(e) => AuditLogEntry::automated_failure(
                    ManualKillAction::SendSigterm,
                    pid,
                    name.clone(),
                    category,
                    reason.clone(),
                    e.to_string(),
                ),
            };

            // Persist before pushing into the in-memory ring so a
            // crash between the two leaves the durable trail intact.
            if let Some(w) = &self.audit_writer
                && let Err(e) = w.append(&entry)
            {
                tracing::warn!(error = %e, "failed to persist automated-kill audit entry");
            }

            // Populate governor_killed_pids ONLY on success — same
            // discipline as `manual_kill`. A failed kill must not
            // pre-tag the PID; if the OS later assigns the PID to
            // an unrelated process before the lifecycle reaper
            // notices, the stale entry would mis-attribute that
            // process's exit as a governor kill (DISPATCH 70).
            if entry.success {
                self.governor_killed_pids.insert(pid, reason.clone());
            }

            // v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — mirror auto
            // SIGTERM kill into archive.
            self.history.record_event(crate::history::kill_event(&entry));
            self.state.audit.push_back(entry);
            while self.state.audit.len() > audit_history {
                self.state.audit.pop_front();
            }
        }

        // DISPATCH 81 / step-6 — SIGTERM → SIGKILL escalation.
        //
        // Pending kills from THIS tick (just-emitted SIGTERMs above)
        // and prior ticks (PIDs still alive past grace) are surfaced
        // by `execute_after_grace`: it walks `pending_kills`, filters
        // to entries whose `sigterm_time` is older than
        // `policy.sigterm_grace_period_secs`, and calls
        // `send_sigkill` on each. The activation here is what makes
        // `sigterm_grace_secs` a live config field (pre-D81 it had no
        // reader in production).
        //
        // GATING: same `auto_actuate` early-return above already
        // protects this branch — the default-off invariant extends to
        // SIGKILL escalation. `default_off_emits_no_sigterm_and_no_sigkill`
        // pins it.
        //
        // PID-REUSE GUARD: `send_sigkill` checks the captured pidfd
        // (preferred, kernel-race-free) or `/proc/<pid>/stat`
        // starttime against the value captured at SIGTERM time. If
        // the process exited (or was reaped + PID reassigned) during
        // grace, the SIGKILL is REFUSED with `PidReusedAborted`. The
        // v1.0.1 hazard — SIGKILLing a recycled PID — is structurally
        // impossible because this escalation goes THROUGH
        // `send_sigkill`, not around it.
        //
        // AUDITING: each result becomes its own AuditLogEntry with
        // `KillSource::Automated`. SignalKillSent → SendSigkill
        // success. PidReusedAborted → PidReusedAborted action,
        // `success=false` so the trail records the abort but the
        // entry is not counted as a "delivered kill" (mirrors
        // AuditLog::successful_kills which excludes Cancelled).
        // OS errors → SendSigkill failure with the OS error message.
        //
        // `pending_kills` cleanup happens at the exit drain
        // (`recent_exits` loop above near runtime.rs ~1230) — the
        // PID's eventual exit clears its `pending_kills` entry via
        // `clear_pending`, mirroring the existing
        // `governor_killed_pids` / `pid_to_model_path` cleanup. That
        // way a recycled PID later can re-enter
        // `pending_kills` clean.
        let escalation_results = self.governor.execute_after_grace();
        for (pid, result) in escalation_results {
            // Resolve identity. After SIGKILL, the PID may already be
            // gone from `state.annotated` (next tick's lifecycle pass
            // hasn't run yet though, so usually still present). Fall
            // back to the pending-kill record — that's the
            // authoritative name+category captured at SIGTERM time.
            let (name, category) = self
                .state
                .annotated
                .iter()
                .find(|a| a.pid == pid)
                .map(|a| (a.name.clone(), Some(a.category)))
                .or_else(|| {
                    self.governor
                        .get_pending_kills()
                        .iter()
                        .find(|p| p.pid == pid)
                        .map(|p| (p.name.clone(), Some(p.category)))
                })
                .unwrap_or_else(|| ("<unknown>".to_string(), None));

            let entry = match &result {
                Ok(KillAction::SignalKillSent) => {
                    tracing::warn!(
                        pid,
                        name = %name,
                        "auto_actuate: SIGKILL escalation succeeded \
                         (SIGTERM grace expired)"
                    );
                    AuditLogEntry::automated_success(
                        ManualKillAction::SendSigkill,
                        pid,
                        name.clone(),
                        category,
                        "SIGTERM grace expired; escalated to SIGKILL".to_string(),
                    )
                }
                Ok(KillAction::PidReusedAborted) => {
                    tracing::warn!(
                        pid,
                        name = %name,
                        "auto_actuate: SIGKILL escalation REFUSED \
                         (PID-reuse guard fired — process exited \
                         during grace or PID reassigned)"
                    );
                    AuditLogEntry::automated_failure(
                        ManualKillAction::PidReusedAborted,
                        pid,
                        name.clone(),
                        category,
                        "SIGTERM grace expired; SIGKILL refused by PID-reuse guard".to_string(),
                        "process exited during grace OR PID reassigned to an \
                         unrelated process — captured identity tokens (pidfd / \
                         starttime) no longer match the live PID"
                            .to_string(),
                    )
                }
                Ok(other) => {
                    // Defensive — send_sigkill returns either
                    // SignalKillSent or PidReusedAborted today. Any
                    // other variant is a future-self bug; audit it
                    // loudly so the gap is visible.
                    tracing::error!(
                        pid,
                        action = ?other,
                        "auto_actuate: unexpected KillAction from execute_after_grace"
                    );
                    AuditLogEntry::automated_failure(
                        ManualKillAction::SendSigkill,
                        pid,
                        name.clone(),
                        category,
                        "SIGTERM grace expired; unexpected escalation outcome".to_string(),
                        format!("unexpected KillAction: {other:?}"),
                    )
                }
                Err(e) => {
                    tracing::warn!(
                        pid,
                        name = %name,
                        error = %e,
                        "auto_actuate: SIGKILL escalation failed (OS error)"
                    );
                    AuditLogEntry::automated_failure(
                        ManualKillAction::SendSigkill,
                        pid,
                        name.clone(),
                        category,
                        "SIGTERM grace expired; SIGKILL OS call failed".to_string(),
                        e.to_string(),
                    )
                }
            };

            if let Some(w) = &self.audit_writer
                && let Err(e) = w.append(&entry)
            {
                tracing::warn!(error = %e, "failed to persist escalation audit entry");
            }

            // governor_killed_pids: only refresh on a confirmed
            // SignalKillSent. The PidReusedAborted case explicitly
            // means the OS already disposed of the process (or
            // reassigned the PID) — pre-tagging here could
            // mis-attribute a future, unrelated exit. The existing
            // SIGTERM-side governor_killed_pids entry (written by
            // the SIGTERM loop above, on the success path) stays
            // intact regardless: the SIGTERM was real, the exit was
            // still governor-caused, the attribution holds.
            if matches!(result, Ok(KillAction::SignalKillSent)) {
                self.governor_killed_pids
                    .insert(pid, "SIGKILL after SIGTERM grace".to_string());
            }

            // v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — mirror auto
            // SIGKILL escalation into archive.
            self.history.record_event(crate::history::kill_event(&entry));
            self.state.audit.push_back(entry);
            while self.state.audit.len() > audit_history {
                self.state.audit.pop_front();
            }
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

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 80 — auto-actuate (THE LINE CROSSING) test suite.
    //
    // The headline regression guard is `default_off_emits_zero_kills`.
    // Read first; the others nail down individual gates around it.
    // ─────────────────────────────────────────────────────────────

    /// Build a Runtime whose `state.decisions` and `state.annotated`
    /// look as if `tick()` just produced one `SignalTermSent` entry
    /// for `pid` (matching `name`, AICategory::Inference). The
    /// breach-since map is seeded with `breached_at_offset` ago so
    /// callers can choose "just-arrived" (sustain not yet met) or
    /// "long sustained" (sustain easily met).
    fn rt_with_signaltermsent_decision(
        cfg: Config,
        pid: u32,
        name: &str,
        breached_at_offset: std::time::Duration,
    ) -> Runtime {
        let mut rt = Runtime::new(cfg)
            .expect("Runtime::new must succeed with provided config");
        rt.state.annotated.push(AnnotatedProcess {
            pid,
            name: name.into(),
            category: crate::model::AICategory::Inference,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        });
        rt.state.decisions = vec![(
            pid,
            crate::governor::KillAction::SignalTermSent,
            "synthetic breach for D80 test".to_string(),
        )];
        let since = Instant::now() - breached_at_offset;
        rt.breach_since.insert(pid, since);
        rt
    }

    /// THE HEADLINE: when `auto_actuate` defaults to `false`, the
    /// actuation site is a no-op even when EVERYTHING ELSE points to
    /// "kill this PID." This pins layer 2 of the v1.0.1 phantom-kill
    /// scar: the operator must NAME the verb. Failing this test
    /// means a shipped binary could auto-kill out-of-the-box —
    /// the exact regression v1.0.1 closed.
    #[test]
    fn default_off_emits_zero_kills() {
        let cfg = Config::default();
        // Sanity: the default really is off. If a future commit
        // flips this, the dispatch's safety stance is broken at the
        // schema layer, not the runtime layer — catch it loudly.
        assert!(
            !cfg.governor.auto_actuate,
            "Config::default().governor.auto_actuate MUST be false. \
             Flipping the default ships auto-kill on a fresh install."
        );

        // Use a fake PID that's definitely not running so any
        // accidental kill attempt would surface as ESRCH (which we'd
        // also detect, but the gate must short-circuit BEFORE that).
        let pid = 1_000_000_001u32;
        let mut rt = rt_with_signaltermsent_decision(
            cfg,
            pid,
            "synthetic-llm",
            std::time::Duration::from_secs(3600), // long-sustained
        );

        let audit_before = rt.state.audit.len();
        let killed_before = rt.governor_killed_pids.contains_key(&pid);
        rt.record_governor_audit();
        let audit_after = rt.state.audit.len();
        let killed_after = rt.governor_killed_pids.contains_key(&pid);

        assert_eq!(
            audit_after, audit_before,
            "default-off MUST add ZERO audit entries — the actuation \
             site is gated on auto_actuate, which is false here. Even \
             with state.decisions carrying SignalTermSent, breach-since \
             long-sustained, and an annotated row matching the PID, \
             nothing should fire. v1.0.1 scar layer 2 holds.",
        );
        assert!(
            !killed_after && !killed_before,
            "default-off MUST NOT populate governor_killed_pids: would \
             pre-tag a never-killed PID for ExitReason::GovernorKill \
             at next drain. PID {pid} appeared.",
        );
    }

    /// Two-gate invariant. Even with `auto_actuate = true`, a
    /// state.decisions list with NO SignalTermSent entries (the
    /// shape the default Allow policy produces — layer 1 of the
    /// scar) yields zero actuations. Removing ONE gate without the
    /// other must still prevent kills.
    #[test]
    fn two_gate_invariant_holds_when_policy_is_allow() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true; // operator-opt-in

        // No SignalTermSent entries in state.decisions — this is
        // what the default Allow policy produces in production
        // (`default_allow_policy_emits_no_signaltermsent_even_with_breaches`
        // pins that at the policy layer). Here we simulate the
        // post-eval shape directly.
        let mut rt = Runtime::new(cfg).expect("Runtime::new with auto_actuate=true must succeed");
        rt.state.annotated.push(AnnotatedProcess {
            pid: 4321,
            name: "allow-policy-ai".into(),
            category: crate::model::AICategory::Inference,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        });
        // state.decisions has nothing matching SignalTermSent (here:
        // empty, but Whitelisted/Skipped/AlreadyExited would all
        // behave the same).
        rt.state.decisions = vec![(
            4321,
            crate::governor::KillAction::Whitelisted,
            "allowed by policy".to_string(),
        )];
        // Even if breach-since said sustained:
        rt.breach_since
            .insert(4321, Instant::now() - std::time::Duration::from_secs(3600));

        rt.record_governor_audit();
        assert_eq!(
            rt.state.audit.len(),
            0,
            "two-gate invariant: auto_actuate=true alone is not \
             enough — a kill-deciding policy must also fire \
             SignalTermSent. With state.decisions carrying only \
             Whitelisted, no actuation should occur.",
        );
        assert!(
            rt.governor_killed_pids.is_empty(),
            "no governor_killed_pids entry without a SignalTermSent decision",
        );
    }

    /// Sustain gate (C3). With `auto_actuate=true` and a real
    /// SignalTermSent decision, but the breach observed only a
    /// fraction of a second ago, the kill MUST NOT fire. The
    /// `kill_sustain_secs` default is 10 s; we offset by 0 s.
    #[test]
    fn sustain_gate_blocks_unsustained_breach() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true;
        assert!(
            cfg.governor.kill_sustain_secs >= 5,
            "kill_sustain_secs default must comfortably exceed the \
             test's 0 s offset; got {}",
            cfg.governor.kill_sustain_secs,
        );

        let pid = 1_000_000_002u32;
        let mut rt = rt_with_signaltermsent_decision(
            cfg,
            pid,
            "just-arrived-breach",
            std::time::Duration::from_secs(0), // freshly arrived
        );

        rt.record_governor_audit();
        assert_eq!(
            rt.state.audit.len(),
            0,
            "sustain gate: a freshly-arrived breach must NOT fire a \
             kill. The actuation site holds for kill_sustain_secs \
             (default 10 s) before signalling.",
        );
        assert!(
            rt.governor_killed_pids.is_empty(),
            "governor_killed_pids must stay empty when sustain gate blocks",
        );
    }

    /// Missing-breach-row guard. A SignalTermSent decision whose PID
    /// has NO breach-since entry (race: decisions and breaches drift
    /// across the tick boundary) MUST NOT fire. The actuation site
    /// treats absence as "sustain unknown ⇒ refuse" — safer than
    /// silently treating absence as sustained.
    #[test]
    fn missing_breach_since_blocks_actuation() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true;

        let pid = 1_000_000_003u32;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        rt.state.annotated.push(AnnotatedProcess {
            pid,
            name: "no-breach-row".into(),
            category: crate::model::AICategory::Inference,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        });
        rt.state.decisions = vec![(
            pid,
            crate::governor::KillAction::SignalTermSent,
            "decision without matching breach-since row".to_string(),
        )];
        // No breach_since.insert — that's the test condition.

        rt.record_governor_audit();
        assert_eq!(
            rt.state.audit.len(),
            0,
            "missing breach-since row MUST block the kill (safe default \
             on the unmeasured-sustain case, same discipline as \
             vram_pct=None ⇒ vram_breached=false in the projection)."
        );
    }

    /// OPT-IN actuation path: with both gates open and the sustain
    /// satisfied, the actuation site DOES reach `send_sigterm` and
    /// records an audit entry. We aim the signal at a non-existent
    /// PID so `libc::kill` returns ESRCH (no real process dies) but
    /// the wiring is exercised end-to-end: the failure is recorded
    /// as `KillSource::Automated`, `success=false`, and the PID is
    /// NOT pre-tagged for governor-kill attribution (mirrors the
    /// `manual_kill` failure discipline).
    #[test]
    fn opt_in_actuation_reaches_send_sigterm_and_records_automated_audit() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true;

        let pid = 1_000_000_004u32; // well above any real /proc PID
        let mut rt = rt_with_signaltermsent_decision(
            cfg,
            pid,
            "synthetic-target",
            std::time::Duration::from_secs(3600),
        );

        rt.record_governor_audit();

        assert_eq!(
            rt.state.audit.len(),
            1,
            "opt-in path: exactly ONE audit entry written for the \
             SignalTermSent decision; got {}",
            rt.state.audit.len()
        );
        let entry = rt.state.audit.front().expect("audit entry");
        assert_eq!(
            entry.source,
            crate::governor::manual::KillSource::Automated,
            "audit entry source MUST be Automated (not Manual): the \
             new automated path uses AuditLogEntry::automated_* \
             constructors that hardcode KillSource::Automated."
        );
        assert_eq!(entry.pid, pid);
        assert_eq!(
            entry.action,
            crate::governor::manual::ManualKillAction::SendSigterm,
        );
        // ESRCH → failure entry — but the wiring proves reach.
        assert!(
            !entry.success,
            "expected ESRCH failure on a non-existent PID; the test \
             host might have a process at PID {pid}, or the kernel's \
             pid_max is unexpectedly high. error_msg={:?}",
            entry.error_msg,
        );
        assert!(
            !rt.governor_killed_pids.contains_key(&pid),
            "a FAILED automated kill MUST NOT populate \
             governor_killed_pids — the failed-tag discipline must \
             mirror manual_kill's. Found {pid} pre-tagged.",
        );
    }

    /// Config validation: `kill_sustain_secs` < `alert_sustain_secs`
    /// must be rejected at load time with an operator-actionable
    /// error message. Pinned because silent acceptance would let the
    /// kill path undercut the alert window — the operator sees the
    /// alert AFTER (or simultaneously with) the kill, breaking the
    /// "alert first, kill only if sustained" stance.
    #[test]
    fn kill_sustain_below_alert_sustain_is_rejected() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.thresholds.alert_sustain_secs = Some(30);
        cfg.governor.kill_sustain_secs = 10; // < 30, must reject

        let err = cfg.validate().expect_err("validate must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("kill_sustain_secs") && msg.contains("alert_sustain_secs"),
            "rejection message must name both fields so the operator \
             can act on it; got: {msg}"
        );
        assert!(
            msg.contains("10") && msg.contains("30"),
            "rejection message must include both values so the \
             operator sees what to change; got: {msg}",
        );
    }

    /// Twin to the above: equal values validate, and a kill above
    /// the alert validates. The validator's boundary is `>=`, NOT
    /// `>`, matching the design (Q3: "validated >=
    /// alert_sustain_secs").
    #[test]
    fn kill_sustain_at_or_above_alert_sustain_validates() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.thresholds.alert_sustain_secs = Some(7);
        cfg.governor.kill_sustain_secs = 7;
        cfg.validate().expect("equal values must validate");

        cfg.governor.kill_sustain_secs = 8;
        cfg.validate().expect("kill > alert must validate");

        cfg.governor.kill_sustain_secs = 600;
        cfg.validate().expect("kill at upper bound must validate");
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 81 — SIGTERM → SIGKILL escalation (step-6) test
    // suite. Builds on the D80 actuation-site harness above; reuses
    // the auto_actuate gate. The headline guard
    // `default_off_emits_no_sigterm_and_no_sigkill` is the EXTENDED
    // form of D80's `default_off_emits_zero_kills` — same scar
    // discipline, broader coverage (now includes the SIGKILL path).
    // ─────────────────────────────────────────────────────────────

    /// Seed an expired pending-kill on the governor with arbitrary
    /// identity tokens. Test-only path — production callers MUST go
    /// through `send_sigterm` so the PID-reuse guard's identity
    /// tokens are captured BEFORE the signal (the v1.0.1
    /// protection). See `GovernorExecutor::insert_pending_kill_for_test`.
    fn seed_expired_pending_kill(
        rt: &mut Runtime,
        pid: u32,
        name: &str,
        starttime_ticks: Option<u64>,
        secs_since_sigterm: i64,
    ) {
        use crate::governor::PendingKill;
        use chrono::Duration as ChronoDuration;
        let mut pk = PendingKill::new(pid, name.to_string(), crate::model::AICategory::Inference);
        pk.sigterm_time = chrono::Utc::now() - ChronoDuration::seconds(secs_since_sigterm);
        pk.starttime_ticks = starttime_ticks;
        pk.pidfd = None;
        rt.governor.insert_pending_kill_for_test(pk);
    }

    /// THE HEADLINE (EXTENDED form of D80's
    /// `default_off_emits_zero_kills`). When `auto_actuate` defaults
    /// to false, NEITHER a SIGTERM (D80) NOR a SIGKILL escalation
    /// (D81) fires. Seeds an expired pending-kill so
    /// `execute_after_grace` WOULD return entries — but the gate
    /// short-circuits before that call. Tearing down this test
    /// (i.e. silently making it pass when it shouldn't) means the
    /// shipped binary can autonomously SIGKILL out-of-the-box; the
    /// v1.0.1 phantom-kill scar layer 2 is fully reopened.
    #[test]
    fn default_off_emits_no_sigterm_and_no_sigkill() {
        let cfg = Config::default();
        assert!(
            !cfg.governor.auto_actuate,
            "default must stay false — flipping it ships autonomous \
             kills (SIGTERM AND SIGKILL) on a fresh install."
        );

        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        // Seed an EXPIRED pending-kill: `execute_after_grace` would
        // surface this if reached, but the auto_actuate gate must
        // early-return before that call.
        seed_expired_pending_kill(
            &mut rt,
            1_000_000_010,
            "would-escalate",
            None,
            3600, // 1 hour past SIGTERM, well beyond default grace
        );

        let audit_before = rt.state.audit.len();
        let pending_before = rt.governor.pending_kills_count();
        rt.record_governor_audit();
        let audit_after = rt.state.audit.len();
        let pending_after = rt.governor.pending_kills_count();

        assert_eq!(
            audit_after, audit_before,
            "default-off MUST add ZERO audit entries — that covers \
             both SIGTERM (D80) and SIGKILL escalation (D81). The \
             single auto_actuate gate funnels both branches.",
        );
        assert_eq!(
            pending_after, pending_before,
            "default-off MUST leave pending_kills untouched (no \
             `send_sigkill` was called, so no `sigkill_time` should \
             have been set). Seeded={pending_before}, after={pending_after}.",
        );
    }

    /// GRACE RESPECTED. With `auto_actuate=true` and a pending-kill
    /// whose SIGTERM was sent JUST NOW (sustain not yet elapsed),
    /// `execute_after_grace` MUST NOT escalate. The
    /// `policy.sigterm_grace_secs` config field — dead code pre-D81
    /// — is now live; this test pins that activation.
    #[test]
    fn grace_period_blocks_premature_escalation() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true;
        // policy.sigterm_grace_secs default is 5 s.
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        seed_expired_pending_kill(
            &mut rt,
            1_000_000_011,
            "fresh-sigterm",
            None,
            0, // SIGTERM sent NOW; grace not elapsed
        );

        let audit_before = rt.state.audit.len();
        rt.record_governor_audit();
        assert_eq!(
            rt.state.audit.len(),
            audit_before,
            "grace period (default 5 s) MUST suppress escalation when \
             sigterm_time is now. `policy.sigterm_grace_secs` is the \
             gate; activating it without enforcement would mean \
             SIGKILL fires every tick.",
        );
    }

    /// IDENTITY GUARD via the escalation path. A pending-kill with
    /// NO captured identity tokens (no pidfd, no starttime — the
    /// "captured-nothing" failure mode for `send_sigterm`) MUST
    /// surface as `PidReusedAborted` from `send_sigkill`, audited
    /// with `KillSource::Automated`, `ManualKillAction::
    /// PidReusedAborted`, `success=false`. This is the v1.0.1
    /// recycled-PID hazard — non-negotiable.
    #[test]
    fn identity_guard_refuses_escalation_without_captured_tokens() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true;
        cfg.policy.sigterm_grace_secs = 1; // tight grace for the test

        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        // No pidfd, no starttime; grace expired (10s past SIGTERM).
        let target_pid = 1_000_000_012u32;
        seed_expired_pending_kill(&mut rt, target_pid, "no-id-tokens", None, 10);

        let audit_before = rt.state.audit.len();
        rt.record_governor_audit();

        let new_entries: Vec<_> = rt
            .state
            .audit
            .iter()
            .skip(audit_before)
            .filter(|e| e.pid == target_pid)
            .collect();
        assert_eq!(
            new_entries.len(),
            1,
            "exactly ONE escalation audit entry expected; got {}",
            new_entries.len(),
        );
        let entry = new_entries[0];
        assert_eq!(
            entry.action,
            crate::governor::manual::ManualKillAction::PidReusedAborted,
            "expected PidReusedAborted on a pending-kill with no \
             captured identity tokens; got {:?}",
            entry.action,
        );
        assert_eq!(
            entry.source,
            crate::governor::manual::KillSource::Automated,
        );
        assert!(
            !entry.success,
            "PidReusedAborted is a REFUSAL, not a successful kill — \
             must record success=false so AuditLog::successful_kills \
             doesn't count it.",
        );
        assert!(
            !rt.governor_killed_pids.contains_key(&target_pid),
            "a REFUSED SIGKILL MUST NOT populate governor_killed_pids \
             — the OS already disposed of the process (or PID was \
             reassigned); pre-tagging risks mis-attribution.",
        );
    }

    /// AUTO ESCALATION (the success path). Spawns a long-lived child
    /// that traps SIGTERM (so it survives the SIGTERM stage), seeds
    /// a pending-kill with the child's real starttime so the
    /// PID-reuse guard's identity check PASSES, sets grace=0, and
    /// drives `record_governor_audit`. The escalation must fire,
    /// kill the child via SIGKILL, and audit success.
    ///
    /// This is the end-to-end wiring proof: tick-loop → auto_actuate
    /// gate → execute_after_grace → send_sigkill → audit. Anything
    /// that breaks the chain (gate moved, escalation call dropped,
    /// audit constructor regressed) fires this test.
    #[test]
    fn auto_actuate_escalation_kills_surviving_child() {
        use std::process::Command;
        use std::time::Duration as StdDuration;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        // Capture the real starttime BEFORE we mess with anything.
        let starttime = crate::governor::pid_reuse::read_starttime(pid);

        let mut cfg = Config::default();

        cfg.web.allow_no_auth = true;
        cfg.governor.auto_actuate = true;
        cfg.policy.sigterm_grace_secs = 0; // immediate escalation

        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        seed_expired_pending_kill(&mut rt, pid, "trap-sigterm-child", starttime, 1);

        rt.record_governor_audit();

        // Find the escalation entry.
        let entry = rt
            .state
            .audit
            .iter()
            .find(|e| e.pid == pid)
            .cloned();
        let entry = match entry {
            Some(e) => e,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("expected an escalation audit entry for PID {pid}; got none");
            }
        };

        assert_eq!(
            entry.action,
            crate::governor::manual::ManualKillAction::SendSigkill,
            "expected SendSigkill action; got {:?}",
            entry.action,
        );
        assert_eq!(
            entry.source,
            crate::governor::manual::KillSource::Automated,
        );
        assert!(
            entry.success,
            "expected successful SIGKILL (identity check passed via \
             real starttime); got success=false, error_msg={:?}",
            entry.error_msg,
        );
        assert!(
            rt.governor_killed_pids.contains_key(&pid),
            "successful SIGKILL MUST populate governor_killed_pids so \
             the lifecycle drain attributes the exit as \
             ExitReason::GovernorKill (D70 bridge).",
        );

        // Reap the dead child to avoid a zombie. SIGKILL has by now
        // delivered; brief settle wait gives the kernel time to reap.
        std::thread::sleep(StdDuration::from_millis(50));
        let _ = child.wait();
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 83 — manual SIGTERM → Waiting → force-SIGKILL path.
    // Closes the live "k didn't kill ollama" symptom: pre-D83 the
    // manual SIGTERM bypassed `pending_kills`, so the operator had
    // no force-kill follow-through. Post-D83 the manual SIGTERM is
    // routed through `governor.send_sigterm` (identity tokens
    // captured) and a second Enter on the Waiting-state card calls
    // `manual_force_kill` which goes through the same PID-reuse
    // guard the D81 auto path uses.
    // ─────────────────────────────────────────────────────────────

    /// Seed a synthetic single-PID lifecycle snapshot onto
    /// `Runtime.state.last_lifecycle` so `manual_kill` / `manual_force_kill`
    /// can find the workload. The annotated row is also pushed so
    /// the audit code path resolves a name.
    fn seed_lifecycle_with_pid(rt: &mut Runtime, pid: u32, name: &str) {
        use crate::lifecycle::{LifecycleSnapshot, ProcessLifecycle};
        use crate::model::ProcessSample;
        use std::collections::HashMap;

        let sample = ProcessSample {
            pid,
            ppid: Some(1),
            name: name.to_string(),
            cmdline: vec![name.to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };
        let lc = ProcessLifecycle::new(&sample, Some(crate::model::AICategory::Inference));
        let mut snap = LifecycleSnapshot::new();
        snap.processes.insert(pid, lc);
        rt.state.last_lifecycle = Some(snap);
        rt.state.annotated.push(AnnotatedProcess {
            pid,
            name: name.into(),
            category: crate::model::AICategory::Inference,
            workload_category: crate::model::WorkloadCategory::Unknown,
            evidence: String::new(),
            model_name: None,
            cpu_pct: 0.0,
            rss_mb: 0,
            vram_bytes: None,
            first_observed_at: Instant::now(),
        });
    }

    /// THE ENABLER PIN: post-D83 `Runtime::manual_kill` routes the
    /// SIGTERM through `governor.send_sigterm`, which means the
    /// `pending_kills` entry is populated with the PID-reuse guard's
    /// identity tokens (pidfd + `/proc/<pid>/stat` starttime). Pre-D83
    /// this was 0 — manual SIGTERMs went through `libc::kill` directly
    /// and the D81 force-SIGKILL escalation had nothing to verify
    /// identity against.
    #[test]
    fn manual_kill_routes_through_send_sigterm_and_populates_pending_kills() {
        use std::process::Command;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();

        let cfg = Config::default();
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        seed_lifecycle_with_pid(&mut rt, pid, "trap-sigterm-child");

        let pending_before = rt.governor.pending_kills_count();
        let result = rt.manual_kill(pid, "test SIGTERM routing".to_string());

        // The SIGTERM is trapped by the child, so it returns success
        // (kernel says delivered).
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("manual_kill failed: {:?}", result);
        }
        let pending_after = rt.governor.pending_kills_count();

        assert_eq!(
            pending_after,
            pending_before + 1,
            "manual_kill MUST populate pending_kills (route through \
             send_sigterm). Pre-D83 the count would NOT have changed \
             because the path went through libc::kill directly. \
             before={pending_before}, after={pending_after}.",
        );

        // Identity tokens captured: starttime should be present
        // (NVidia-grade systems may or may not give us a pidfd, but
        // /proc/<pid>/stat starttime is universal on Linux).
        let pending_entry = rt
            .governor
            .get_pending_kills()
            .into_iter()
            .find(|p| p.pid == pid)
            .expect("pending kill entry must exist for the SIGTERM'd PID");
        assert!(
            pending_entry.starttime_ticks.is_some(),
            "starttime MUST be captured at SIGTERM time so send_sigkill \
             can verify identity later (the v1.0.1 PID-reuse guard). \
             pidfd={:?}",
            pending_entry.pidfd.is_some(),
        );

        // Manual source — NOT Automated. The auto-actuate audit
        // entries use KillSource::Automated; this path is operator-
        // initiated.
        let entry = rt
            .state
            .audit
            .iter()
            .find(|e| e.pid == pid)
            .expect("audit entry must exist for the manual SIGTERM");
        assert_eq!(
            entry.source,
            crate::governor::manual::KillSource::Manual,
            "manual_kill audit must record KillSource::Manual, not \
             Automated (which is reserved for the auto_actuate path).",
        );

        // Force-kill the child to clean up.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// THE IDENTITY GUARD PIN on the manual path. The dispatch
    /// rationale calls this the v1.0.1 hazard's WORST case: the
    /// operator may take seconds or minutes to press Enter between
    /// the SIGTERM and the force-SIGKILL, opening a long reuse
    /// window. `manual_force_kill` MUST refuse the SIGKILL when
    /// identity tokens no longer match — and the audit trail must
    /// record `PidReusedAborted` so the operator can correlate.
    #[test]
    fn manual_force_kill_refused_when_identity_tokens_missing() {
        use crate::governor::PendingKill;

        let cfg = Config::default();
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        let pid = 1_000_000_020u32;
        seed_lifecycle_with_pid(&mut rt, pid, "no-id-tokens");

        // Seed a pending_kill with NO identity (simulates "manual
        // SIGTERM was sent ages ago and the captured tokens are now
        // gone" OR "the PID has been reaped + reassigned"). The
        // identity check in `send_sigkill` MUST refuse.
        let mut pk = PendingKill::new(pid, "no-id-tokens".into(), crate::model::AICategory::NotAi);
        pk.pidfd = None;
        pk.starttime_ticks = None;
        rt.governor.insert_pending_kill_for_test(pk);

        let result = rt.manual_force_kill(pid);
        assert!(
            result.is_err(),
            "manual_force_kill MUST return Err on identity mismatch — \
             returning Ok would mean the audit shows success but no \
             signal landed (a phantom-kill on the manual path)."
        );
        let err_msg = result.err().unwrap();
        assert!(
            err_msg.contains("PID-reuse guard") || err_msg.contains("refused"),
            "error message must name the PID-reuse guard so the \
             operator can correlate; got: {err_msg}",
        );

        // Audit entry records the refusal.
        let entry = rt
            .state
            .audit
            .iter()
            .find(|e| e.pid == pid)
            .expect("audit entry must exist for the refused force-kill");
        assert_eq!(
            entry.action,
            crate::governor::manual::ManualKillAction::PidReusedAborted,
            "expected PidReusedAborted action on identity mismatch; got {:?}",
            entry.action,
        );
        assert_eq!(
            entry.source,
            crate::governor::manual::KillSource::Manual,
        );
        assert!(
            !entry.success,
            "PidReusedAborted is a REFUSAL — audit success MUST be false",
        );
        assert!(
            !rt.governor_killed_pids.contains_key(&pid),
            "a REFUSED SIGKILL MUST NOT populate governor_killed_pids",
        );
    }

    /// THE END-TO-END HAPPY PATH on the manual force-kill: spawn a
    /// SIGTERM-ignoring child, call `manual_kill` (now routes through
    /// `send_sigterm` — pending_kills + identity captured), then call
    /// `manual_force_kill` (routes through `send_sigkill` — identity
    /// re-verified). Child dies; audit records SendSigkill success
    /// with `KillSource::Manual`.
    ///
    /// This is the operator's "k ollama → wait → Enter" scenario
    /// from the dispatch verification list. Without C1's routing the
    /// `send_sigkill` would refuse with PidReusedAborted (no identity
    /// captured); without C3's wiring the operator would have no
    /// surface to trigger it.
    #[test]
    fn manual_force_kill_kills_surviving_child_end_to_end() {
        use std::process::Command;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();

        let cfg = Config::default();
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        seed_lifecycle_with_pid(&mut rt, pid, "trap-sigterm-child");

        // Step 1: manual_kill → SIGTERM via send_sigterm.
        rt.manual_kill(pid, "test step 1".to_string())
            .expect("manual_kill must succeed");

        // Step 2: manual_force_kill → SIGKILL via send_sigkill.
        // Identity captured at step 1 should pass the guard.
        let force_result = rt.manual_force_kill(pid);
        if force_result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("manual_force_kill failed: {:?}", force_result);
        }

        // Audit: TWO entries for this PID — the SIGTERM (success) and
        // the SIGKILL (success). Both KillSource::Manual.
        let entries: Vec<_> = rt.state.audit.iter().filter(|e| e.pid == pid).collect();
        assert_eq!(entries.len(), 2, "expected SIGTERM + SIGKILL entries");
        for e in &entries {
            assert_eq!(
                e.source,
                crate::governor::manual::KillSource::Manual,
                "both manual-path entries must record KillSource::Manual",
            );
            assert!(e.success, "both entries must succeed; got {:?}", e);
        }
        let actions: Vec<_> = entries.iter().map(|e| e.action).collect();
        assert!(
            actions.contains(&crate::governor::manual::ManualKillAction::SendSigterm),
            "must have SendSigterm entry; got {:?}",
            actions,
        );
        assert!(
            actions.contains(&crate::governor::manual::ManualKillAction::SendSigkill),
            "must have SendSigkill entry; got {:?}",
            actions,
        );
        assert!(
            rt.governor_killed_pids.contains_key(&pid),
            "successful force-kill MUST populate governor_killed_pids so \
             the lifecycle drain classifies the exit as GovernorKill.",
        );

        // Reap the dead child.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = child.wait();
    }

    /// THE AUTO INVARIANT PRESERVATION pin. C1's routing change
    /// (manual SIGTERM now uses `send_sigterm`) MUST NOT perturb the
    /// auto path's default-OFF invariant. A `manual_kill` from the
    /// operator populates `pending_kills` — but the auto path
    /// (`record_governor_audit` calling `execute_after_grace`) is
    /// still gated on `auto_actuate=false`, so no autonomous SIGKILL
    /// fires even with pending entries present.
    #[test]
    fn manual_kill_does_not_perturb_auto_default_off_invariant() {
        use crate::governor::PendingKill;

        let cfg = Config::default();
        assert!(
            !cfg.governor.auto_actuate,
            "default must stay false (the invariant under test)"
        );
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        // Simulate a prior manual SIGTERM by seeding pending_kills
        // with the entry shape `manual_kill` would produce (identity
        // tokens captured, grace already exceeded).
        let pid = 1_000_000_021u32;
        let mut pk = PendingKill::new(pid, "manual-sigterm".into(), crate::model::AICategory::Inference);
        pk.sigterm_time = chrono::Utc::now() - chrono::Duration::seconds(3600);
        rt.governor.insert_pending_kill_for_test(pk);

        let audit_before = rt.state.audit.len();
        rt.record_governor_audit();
        let audit_after = rt.state.audit.len();

        assert_eq!(
            audit_after, audit_before,
            "auto path MUST stay default-off even when pending_kills \
             carries entries from the manual path. The auto_actuate \
             gate is the single funnel — manual SIGTERM populating \
             pending_kills MUST NOT cause the auto escalation to fire.",
        );
    }

    /// Default config validates without operator intervention — a
    /// fresh install must not require the operator to set
    /// `kill_sustain_secs` just to launch the binary. The default
    /// (10 s) exceeds the contract's `ALERT_SUSTAIN_SECS` (5 s) so
    /// this passes; if the contract ever raises the alert sustain
    /// above 10 s, the GovernorConfig default must move in
    /// lockstep — this test fires loudly when that drift happens.
    #[test]
    fn default_config_validates_with_default_sustains() {
        // v1.3.2 / DISPATCH 85 — Config::default() alone now FAILS
        // validation (empty auth_token + allow_no_auth=false). Test
        // the sustain pin against a default that has explicitly
        // opted into open access — the auth gate is a separate
        // invariant, pinned by its own tests.
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.validate().expect(
            "Config::default() + allow_no_auth must validate: \
             kill_sustain_secs default (10) >= ALERT_SUSTAIN_SECS \
             contract default (5)",
        );
        assert!(
            cfg.governor.kill_sustain_secs >= ux_contract::thresholds::ALERT_SUSTAIN_SECS,
            "GovernorConfig::default().kill_sustain_secs ({}) MUST \
             stay >= ux_contract::thresholds::ALERT_SUSTAIN_SECS ({}). \
             If the contract raised the alert default, raise this \
             default to match.",
            cfg.governor.kill_sustain_secs,
            ux_contract::thresholds::ALERT_SUSTAIN_SECS,
        );
    }

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
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
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
        cfg.web.allow_no_auth = true;
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
            samples: 30,            trajectory: None,
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

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 90 / PHASE 5 step 3+4 — history capture + exit hand-off.
    // ─────────────────────────────────────────────────────────────

    /// Runtime::new constructs `History` with the doc-locked caps
    /// pulled straight from `RuntimeConfig`. A future refactor that
    /// silently swaps in a different cap source (e.g. a hardcoded
    /// 1800 ignoring the operator's TOML) fires this test.
    #[test]
    fn runtime_new_constructs_history_with_config_caps() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_trajectory_samples_per_pid = 333;
        cfg.runtime.history_event_archive_cap = 77;
        let rt = Runtime::new(cfg).expect("Runtime::new");
        assert_eq!(rt.history.trajectory_cap_per_pid(), 333);
        assert_eq!(rt.history.event_archive_cap(), 77);
        assert_eq!(rt.history.live_pid_count(), 0);
        assert_eq!(rt.history.event_count(), 0);
    }

    /// Hand-off site (PHASE 5 step 4): when a PID's ring is
    /// populated and we call `drain_trajectory`, the returned
    /// trajectory carries the samples AND the HashMap entry is
    /// removed (the dead-PID-ring leak the step-4 hand-off closes).
    #[test]
    fn drain_trajectory_returns_samples_and_removes_entry() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        let pid = 8675309;
        for i in 0..5 {
            rt.history.record_sample(
                pid,
                crate::history::Sample {
                    timestamp: chrono::DateTime::from_timestamp(i, 0).unwrap(),
                    cpu_pct: i as f32,
                    rss_mb: 10 + i as u32,
                    vram_mb: None,
                },
            );
        }
        assert_eq!(rt.history.live_pid_count(), 1);

        let traj = rt
            .history
            .drain_trajectory(pid)
            .expect("non-empty ring freezes to Some");
        assert_eq!(traj.samples.len(), 5);
        assert_eq!(traj.first_sample_at.timestamp(), 0);
        assert_eq!(traj.last_sample_at.timestamp(), 4);
        assert_eq!(
            rt.history.live_pid_count(),
            0,
            "exit hand-off MUST remove the per-PID ring — without \
             this, dead PIDs linger in History.trajectories forever",
        );
    }

    /// VRAM honesty (PHASE 5 step 3): when a sample's vram_mb is
    /// `None` (unmeasured — the operator's driver-unloaded case),
    /// the frozen trajectory MUST preserve `None`, NOT silently
    /// substitute 0. Mirrors the D74/D78 VRAM_UNMEASURED display
    /// discipline.
    #[test]
    fn capture_preserves_vram_unmeasured_as_none() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        let pid = 9_001;
        rt.history.record_sample(
            pid,
            crate::history::Sample {
                timestamp: chrono::Utc::now(),
                cpu_pct: 1.0,
                rss_mb: 100,
                vram_mb: None,
            },
        );
        let traj = rt
            .history
            .drain_trajectory(pid)
            .expect("trajectory exists");
        assert_eq!(traj.samples[0].vram_mb, None);
    }

    /// Disk discipline (PHASE 5 Q3-C): the LifecycleSummary's
    /// `trajectory: None` field is `skip_serializing_if =
    /// "Option::is_none"`, so a serialized RunRecord clone with
    /// `trajectory = None` produces JSON byte-identical to pre-D90
    /// records. The runtime explicitly sets `None` before
    /// `rs.append`; this test pins the serde shape that backs that
    /// discipline.
    #[test]
    fn lifecycle_summary_with_none_trajectory_omits_field_from_json() {
        let summary = LifecycleSummary {
            pid: 1,
            name: "p".into(),
            category: None,
            model_name: None,
            spawn_time: chrono::Utc::now(),
            exit_time: chrono::Utc::now(),
            uptime_secs: 1,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 0.0,
            peak_cpu_pct: 0.0,
            peak_rss_mb: 0,
            peak_vram_mb: 0,
            samples: 0,
            trajectory: None,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            !json.contains("trajectory"),
            "trajectory=None MUST be omitted from JSON \
             (skip_serializing_if). Pre-D90 disk records stay \
             byte-identical. Got: {json}",
        );

        // Conversely, Some(traj) MUST round-trip in.
        let mut summary_with = summary.clone();
        summary_with.trajectory = Some(crate::history::Trajectory {
            samples: vec![crate::history::Sample {
                timestamp: chrono::Utc::now(),
                cpu_pct: 1.0,
                rss_mb: 5,
                vram_mb: Some(10),
            }],
            first_sample_at: chrono::Utc::now(),
            last_sample_at: chrono::Utc::now(),
        });
        let json_with = serde_json::to_string(&summary_with).unwrap();
        assert!(
            json_with.contains("trajectory"),
            "Some(trajectory) MUST serialize. Got: {json_with}",
        );
        let round: LifecycleSummary = serde_json::from_str(&json_with).unwrap();
        assert!(round.trajectory.is_some());
        assert_eq!(round.trajectory.unwrap().samples.len(), 1);
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 91 / PHASE 5 step 5 — event archive write sites.
    // ─────────────────────────────────────────────────────────────

    /// A manual kill via `Runtime::manual_kill` pushes BOTH into
    /// `state.audit` (existing behavior) AND into the cross-PID
    /// `history.event_archive` (D91 mirror). The archive carries a
    /// `Kill`-kind event with the same PID + name + summary the
    /// wire activity feed renders, but it lives in a deeper
    /// (cap-500) store the future history view will query.
    #[test]
    fn manual_kill_mirrors_into_history_event_archive() {
        use crate::governor::PendingKill;

        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        // Seed a synthetic lifecycle so manual_kill can resolve.
        seed_lifecycle_with_pid(&mut rt, 1_000_000_030, "manual-victim");

        // The send_sigterm call will return Err on the fake PID,
        // but the audit entry (failure) is still recorded. That's
        // fine for this test: the archive mirror runs on EVERY
        // audit push, success or failure.
        let _ = rt.manual_kill(1_000_000_030, "test mirror".to_string());

        assert_eq!(
            rt.history.event_count(),
            1,
            "manual_kill MUST mirror exactly one event into the \
             archive (success or failure — both audit pushes mirror)."
        );
        let ev = rt.history.event_archive.front().expect("event present");
        assert!(matches!(ev.kind, crate::history::HistoryEventKind::Kill));
        assert_eq!(ev.pid, 1_000_000_030);
        let _ = PendingKill::new; // proof: import path stable
    }

    /// THE Q4 INVARIANT: the live wire activity feed cap stays
    /// UNCHANGED at `ACTIVITY_FEED_WIRE_MAX = 50`. The history
    /// archive's cap is a SEPARATE 500. Raising the live cap would
    /// bloat every `/api/snapshot` payload (1 Hz polling); the
    /// archive is on-demand only.
    #[test]
    fn live_wire_activity_cap_unchanged_by_history_archive() {
        // The wire constant is set on the contract side; this test
        // pins that the consumer hasn't quietly raised it as part of
        // D91. The archive cap (default 500) is materially larger.
        assert_eq!(
            ux_contract::limits::ACTIVITY_FEED_WIRE_MAX,
            50,
            "Q4 invariant: the wire's activity feed cap is locked at \
             50. The archive's 500 cap is a SEPARATE store (read on \
             demand by the future history endpoint), NOT a raise of \
             the live wire cap."
        );
        let cfg = Config::default();
        assert!(
            cfg.runtime.history_event_archive_cap
                > ux_contract::limits::ACTIVITY_FEED_WIRE_MAX,
            "archive cap ({}) MUST exceed the wire cap ({}); otherwise \
             we'd be duplicating the wire feed at the same depth, \
             which is the no-op shape the Q4 rationale rejects",
            cfg.runtime.history_event_archive_cap,
            ux_contract::limits::ACTIVITY_FEED_WIRE_MAX,
        );
    }

    /// FIFO eviction at the archive cap. Push more events than the
    /// configured cap and confirm the OLDEST is evicted (newest
    /// retained). Tests the cap enforcement directly on the
    /// `History` container; the runtime's call sites use the same
    /// `record_event` path.
    #[test]
    fn history_event_archive_caps_at_configured_size_fifo() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        cfg.runtime.history_event_archive_cap = 3; // tiny for the test
        let mut rt = Runtime::new(cfg).expect("Runtime::new");
        for i in 0..7 {
            rt.history
                .record_event(crate::history::HistoryEvent {
                    timestamp: chrono::DateTime::from_timestamp(i, 0).unwrap(),
                    pid: 100 + i as u32,
                    name: format!("p{i}"),
                    kind: crate::history::HistoryEventKind::Exit,
                    summary: format!("ev #{i}"),
                });
        }
        assert_eq!(
            rt.history.event_count(),
            3,
            "archive MUST cap at the configured size"
        );
        // FIFO: the LAST 3 events survive (timestamps 4, 5, 6).
        let timestamps: Vec<i64> = rt
            .history
            .event_archive
            .iter()
            .map(|e| e.timestamp.timestamp())
            .collect();
        assert_eq!(timestamps, vec![4, 5, 6]);
    }

    // ─────────────────────────────────────────────────────────────
    // DISPATCH 92 / PHASE 5 verification — capture end-to-end.
    //
    // These tests exercise the same call sequence the runtime tick
    // path uses (`self.history.record_sample(...)` at the capture
    // site, `self.history.drain_trajectory(...)` at the exit-drain
    // site, `summary_for_completed.trajectory = trajectory` +
    // `state.completed.push_back(...)`). The tests REPRODUCE those
    // call sites — they do NOT extract or refactor production code
    // (the tripwire forbids adding a read path in runtime.rs). The
    // guarantee is thin (a refactor could shift the call site
    // without breaking these tests) but the tripwire counts + names
    // the sites, so drift is caught THERE; D92 answers a different
    // question: does the DATA that flows through those sites carry
    // real, non-defaulted values?
    // ─────────────────────────────────────────────────────────────

    /// Build a `LifecycleSummary` shaped like the runtime's exit
    /// drain would surface. Used across the V1-V3 tests to keep the
    /// synthetic-exit shape consistent.
    fn synthetic_summary(pid: u32, name: &str) -> LifecycleSummary {
        LifecycleSummary {
            pid,
            name: name.into(),
            category: Some(crate::model::AICategory::Inference),
            model_name: Some("llama3-8b".into()),
            spawn_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            exit_time: chrono::DateTime::from_timestamp(60, 0).unwrap(),
            uptime_secs: 60,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 30.0,
            peak_cpu_pct: 80.0,
            peak_rss_mb: 500,
            peak_vram_mb: 600,
            samples: 60,
            trajectory: None,
        }
    }

    /// V1 — trajectory end-to-end. Push N samples via the SAME
    /// `self.history.record_sample(...)` call the runtime uses at
    /// runtime.rs's capture site, then reproduce the SAME
    /// drain-and-attach sequence the runtime's exit-drain uses.
    /// Assert the trajectory lands with N samples AND the metric
    /// values are REAL — not zeros / defaults.
    #[test]
    fn v1_trajectory_end_to_end_carries_real_metrics() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        let pid = 42_100u32;
        // Push 5 distinct samples with rising metric values so a
        // zero-fill / default-only bug would be caught by the
        // per-sample assertions below.
        for i in 0..5 {
            let sample = crate::history::Sample {
                timestamp: chrono::DateTime::from_timestamp(i as i64, 0).unwrap(),
                cpu_pct: (i as f32) * 10.0 + 5.0, // 5, 15, 25, 35, 45
                rss_mb: 100 + i,
                vram_mb: Some(500 + i * 10),
            };
            rt.history.record_sample(pid, sample);
        }
        assert_eq!(rt.history.live_pid_count(), 1);

        // Reproduce the runtime's exit-drain sequence.
        let summary = synthetic_summary(pid, "llama-server");
        let trajectory = rt.history.drain_trajectory(pid);
        assert!(
            trajectory.is_some(),
            "V1 FAILURE: drain_trajectory returned None even though \
             5 samples were pushed — capture path is broken."
        );
        let mut summary_for_completed = summary.clone();
        summary_for_completed.trajectory = trajectory;
        rt.state.completed.push_back(summary_for_completed);

        // Verify the trajectory landed with real metrics.
        let landed = rt
            .state
            .completed
            .back()
            .expect("state.completed populated");
        let traj = landed.trajectory.as_ref().expect(
            "V1 FAILURE: state.completed[last].trajectory is None — \
             the Q3-C hand-off is broken; step 4 didn't attach.",
        );
        assert_eq!(
            traj.samples.len(),
            5,
            "V1 FAILURE: expected 5 samples on the completed \
             trajectory; got {}. Capture is dropping samples between \
             record_sample and drain_trajectory.",
            traj.samples.len(),
        );
        // Real metric flow — verify each sample carries the value
        // it was pushed with, in order.
        for (i, s) in traj.samples.iter().enumerate() {
            let expected_cpu = (i as f32) * 10.0 + 5.0;
            assert!(
                (s.cpu_pct - expected_cpu).abs() < 0.01,
                "V1 FAILURE: sample {i}.cpu_pct = {} — expected {expected_cpu}. \
                 Metric threading through capture is broken (zeroed / \
                 default value).",
                s.cpu_pct,
            );
            assert_eq!(
                s.rss_mb,
                100 + i as u32,
                "V1 FAILURE: sample {i}.rss_mb = {} — expected {}. \
                 RSS metric is not threading.",
                s.rss_mb,
                100 + i,
            );
            assert_eq!(
                s.vram_mb,
                Some(500 + i as u32 * 10),
                "V1 FAILURE: sample {i}.vram_mb — VRAM metric not \
                 threading.",
            );
        }
        // Post-drain hygiene: the live PID map should have shed this
        // PID (dead-PID-ring leak closure — D90 step 4).
        assert_eq!(
            rt.history.live_pid_count(),
            0,
            "V1 FAILURE: drain didn't remove the PID from \
             History.trajectories — dead-PID-ring leak."
        );
    }

    /// V2 — VRAM honesty end-to-end. When capture pushes samples
    /// with `vram_mb: None` (the operator's driver-unloaded case),
    /// the FROZEN trajectory MUST preserve `None`, never fabricate a
    /// 0 that reads as real. Same discipline as the D74/D78
    /// VRAM_UNMEASURED display path — pinned end-to-end here.
    #[test]
    fn v2_vram_unmeasured_survives_full_capture_to_completed_path() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        let pid = 42_200u32;
        for i in 0..3 {
            rt.history.record_sample(
                pid,
                crate::history::Sample {
                    timestamp: chrono::DateTime::from_timestamp(i, 0).unwrap(),
                    cpu_pct: 12.5,
                    rss_mb: 200,
                    vram_mb: None, // unmeasured
                },
            );
        }
        let summary = synthetic_summary(pid, "phi3-mini");
        let trajectory = rt.history.drain_trajectory(pid);
        let mut summary_for_completed = summary.clone();
        summary_for_completed.trajectory = trajectory;
        rt.state.completed.push_back(summary_for_completed);

        let traj = rt
            .state
            .completed
            .back()
            .and_then(|s| s.trajectory.as_ref())
            .expect("trajectory attached");
        for (i, s) in traj.samples.iter().enumerate() {
            assert!(
                s.vram_mb.is_none(),
                "V2 FAILURE: sample {i}.vram_mb = {:?} — expected None. \
                 VRAM honesty violated end-to-end (a fabricated 0 or a \
                 non-None value would fool the future view into \
                 rendering a fake reading).",
                s.vram_mb,
            );
        }
    }

    /// V3 — event archive end-to-end for all three source types.
    /// Reproduces the runtime call sites at each event's push point
    /// and verifies the corresponding `HistoryEvent` lands with
    /// correct kind + pid + name + non-empty summary.
    ///
    /// KILL is already end-to-end covered by
    /// `manual_kill_mirrors_into_history_event_archive` (D91). This
    /// test covers EXIT + REGRESSION.
    #[test]
    fn v3_exit_and_regression_events_land_in_archive_end_to_end() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        // EXIT push — reproduces the runtime's exit-drain call site:
        //   if summary.category.is_some() {
        //       self.history.record_event(crate::history::exit_event(summary));
        //   }
        let exit_summary = synthetic_summary(9_001, "vllm-worker");
        assert!(exit_summary.category.is_some(), "test premise: AI-classified");
        rt.history.record_event(crate::history::exit_event(&exit_summary));

        // REGRESSION push — reproduces the runtime's regression-count
        // loop's `.record_event(regression_event(ev))` call.
        use crate::analysis::compare::{Regression, Severity};
        use crate::analysis::RegressionEvent;
        let regression = RegressionEvent {
            timestamp: chrono::Utc::now(),
            model: "llama3-8b".into(),
            regression: Regression {
                metric: "tokens_per_sec".into(),
                baseline: 100.0,
                current: 78.0,
                delta_pct: -22.0,
                severity: Severity::Critical,
            },
            baseline_size: 12,
        };
        rt.history
            .record_event(crate::history::regression_event(&regression));

        assert_eq!(rt.history.event_count(), 2, "expected 2 events");

        // The archive is FIFO (newest at the back). Iterate and pin
        // each event's shape.
        let events: Vec<_> = rt.history.event_archive.iter().collect();
        let exit_ev = &events[0];
        assert!(
            matches!(exit_ev.kind, crate::history::HistoryEventKind::Exit),
            "V3 FAILURE: first event must be Exit; got {:?}",
            exit_ev.kind,
        );
        assert_eq!(exit_ev.pid, 9_001);
        assert_eq!(exit_ev.name, "vllm-worker");
        assert!(
            exit_ev.summary.contains("exit pid=9001") && exit_ev.summary.contains("vllm-worker"),
            "V3 FAILURE: exit summary must carry pid+name; got {:?}",
            exit_ev.summary,
        );

        let reg_ev = &events[1];
        assert!(
            matches!(reg_ev.kind, crate::history::HistoryEventKind::Regression),
            "V3 FAILURE: second event must be Regression; got {:?}",
            reg_ev.kind,
        );
        assert_eq!(
            reg_ev.pid, 0,
            "regressions use PID 0 sentinel — matches wire (D71)"
        );
        assert_eq!(reg_ev.name, "llama3-8b");
        assert!(
            reg_ev.summary.contains("REGRESSION")
                && reg_ev.summary.contains("llama3-8b")
                && reg_ev.summary.contains("-22.0%"),
            "V3 FAILURE: regression summary must carry the metric delta; got {:?}",
            reg_ev.summary,
        );
    }

    /// V4 — leak check. Drive many spawn+exit cycles through the
    /// SAME `record_sample` + `drain_trajectory` calls the runtime
    /// uses; the `History.trajectories` HashMap MUST NOT grow with
    /// cumulative PIDs — it should track LIVE PIDs only (dead ones
    /// removed by the drain, per D90 step 4). This pins the leak
    /// closure end-to-end under a realistic PID-churn pattern (the
    /// operator's ROS2 short-lived-nodes shape).
    #[test]
    fn v4_high_pid_churn_does_not_leak_history_trajectories() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        const CYCLES: u32 = 100;
        // Spawn + exit each PID one at a time — this is the classic
        // "short-lived process" pattern (unit test proxy for a ROS2
        // graph doing lots of ephemeral python3 nodes).
        for i in 0..CYCLES {
            let pid = 50_000 + i;
            rt.history.record_sample(
                pid,
                crate::history::Sample {
                    timestamp: chrono::DateTime::from_timestamp(i as i64, 0).unwrap(),
                    cpu_pct: 5.0,
                    rss_mb: 50,
                    vram_mb: None,
                },
            );
            assert_eq!(
                rt.history.live_pid_count(),
                1,
                "V4 FAILURE (cycle {i}): live_pid_count should be 1 \
                 during the single-PID-active phase; got {}. Sample \
                 accumulation across cycles indicates the drain is \
                 not clearing prior PIDs.",
                rt.history.live_pid_count(),
            );
            let drained = rt.history.drain_trajectory(pid);
            assert!(
                drained.is_some(),
                "V4 FAILURE (cycle {i}): drain returned None"
            );
            assert_eq!(
                rt.history.live_pid_count(),
                0,
                "V4 FAILURE (cycle {i}): drain didn't remove the PID \
                 from the HashMap — dead-PID-ring LEAK. HashMap size \
                 is {}.",
                rt.history.live_pid_count(),
            );
        }

        // Final invariant: after CYCLES × (record + drain), the
        // HashMap is empty. If any cycle's drain leaked, the count
        // would be > 0 here.
        assert_eq!(
            rt.history.live_pid_count(),
            0,
            "V4 FAILURE: after {CYCLES} spawn+exit cycles, \
             History.trajectories has {} entries — a leak of \
             cumulative PIDs.",
            rt.history.live_pid_count(),
        );
    }

    /// V4b — the concurrent-live variant. Multiple PIDs active at
    /// once, each drained after its own captured samples. Ensures
    /// the HashMap holds N live PIDs at peak and shrinks to 0 after
    /// all drain, even when their lifetimes overlap.
    #[test]
    fn v4b_concurrent_live_pids_shrink_to_zero_after_all_drain() {
        let mut cfg = Config::default();
        cfg.web.allow_no_auth = true;
        let mut rt = Runtime::new(cfg).expect("Runtime::new");

        const N: u32 = 32;
        // Phase 1: N PIDs all alive with samples.
        for i in 0..N {
            rt.history.record_sample(
                60_000 + i,
                crate::history::Sample {
                    timestamp: chrono::Utc::now(),
                    cpu_pct: 1.0,
                    rss_mb: 10,
                    vram_mb: None,
                },
            );
        }
        assert_eq!(
            rt.history.live_pid_count(),
            N as usize,
            "V4b FAILURE: expected {N} live PIDs; got {}",
            rt.history.live_pid_count(),
        );

        // Phase 2: drain them all, one by one.
        for i in 0..N {
            let _ = rt.history.drain_trajectory(60_000 + i);
        }
        assert_eq!(
            rt.history.live_pid_count(),
            0,
            "V4b FAILURE: after draining all {N} PIDs, HashMap should be \
             empty; got {}. LEAK.",
            rt.history.live_pid_count(),
        );
    }
}
