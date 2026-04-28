# edge_monitor — Linux Implementation Spec

> **Audience:** AI coding agent (Claude Code, Cursor, etc.) implementing the next phase of `edge_monitor` on Linux.
> **Assumption:** Phase 0 + Phase 1 already complete. `LifecycleSummary`, governor, audit log, ratatui TUI, and JSONL persistence exist.
> **Goal:** Add per-model history, AI-runtime telemetry (tokens/sec, fps, latency, power), regression detection, and the differentiating features that make this notable at launch.
> **Non-negotiables:** Dry-run remains default. All safety invariants in `CLAUDE.md` hold. No feature ships without tests.

---

## How to use this document

1. Read each tier in order. Do not skip ahead — Tier 1 features unblock Tier 2.
2. For every feature: implement → unit test → integration test → manual smoke test → update FEATURES.md.
3. After each feature lands, run `cargo test --all && cargo clippy --all-targets -- -D warnings`. CI must stay green.
4. Commit per feature, not per tier. Conventional commits: `feat(history): ...`, `feat(telemetry): ...`, `test(governor): ...`.
5. When a step says "verify manually," create a reproducible script under `scripts/manual/` so the verification can be repeated.

---

## Architectural additions (build these first, they support everything)

Before any feature work, three foundational pieces must exist. These are not user-visible but every later feature depends on them.

### A. The `RunRecord` store

Today: `LifecycleSummary` is appended to a single JSONL file on exit. That's fine for forensics, terrible for queries like "show me the last 5 runs of phi3-mini."

Build: `src/storage/run_store.rs` — a typed read/write layer over the existing JSONL plus a per-model index.

```
struct RunStore {
    root: PathBuf,                       // e.g. ~/.local/share/edge_monitor/
    index: HashMap<String, Vec<RunId>>,  // model_name -> run ids, newest first
}

impl RunStore {
    fn append(&mut self, record: RunRecord) -> Result<RunId>
    fn list_models(&self) -> Vec<String>
    fn recent(&self, model: &str, n: usize) -> Vec<RunRecord>
    fn get(&self, id: RunId) -> Option<RunRecord>
    fn baseline(&self, model: &str, window: usize) -> Option<Baseline>  // rolling avg of last N
}
```

Storage layout on disk:

```
~/.local/share/edge_monitor/
  runs/
    2026-04-28/
      run-<uuid>.json     # one file per run, full record
  index.jsonl             # append-only, one line per run, for fast scan on startup
```

Why per-file plus an index: the existing single JSONL can grow unbounded and rewriting it is risky. Per-file lets users delete individual runs; the index gives O(N) startup scan without parsing every full record.

**`RunRecord`** is a superset of `LifecycleSummary`. Extend, don't replace:

```rust
struct RunRecord {
    // From LifecycleSummary (existing)
    pid, name, category, model_name, spawn_time, exit_time,
    uptime_secs, exit_code, signal,
    avg_cpu_pct, peak_cpu_pct, peak_rss_mb, peak_vram_mb, samples,

    // New
    run_id: Uuid,
    model_fingerprint: Option<String>,    // see Tier-3 feature
    runtime: Option<RuntimeKind>,         // Vllm | LlamaCpp | Ollama | Ultralytics | Unknown
    quantization: Option<String>,         // "Q4_K_M", "FP16", parsed from filename
    metrics: RunMetrics,                  // see below
    exit_reason: ExitReason,              // see Tier-3 feature
    cold_start: Option<ColdStartStats>,   // see Tier-3 feature
}

struct RunMetrics {
    // LLM
    tokens_total: Option<u64>,
    tokens_per_sec_avg: Option<f32>,
    tokens_per_sec_peak: Option<f32>,
    kv_cache_peak_pct: Option<f32>,
    concurrent_requests_peak: Option<u32>,

    // Vision
    frames_total: Option<u64>,
    fps_avg: Option<f32>,
    inference_latency_ms_avg: Option<f32>,
    inference_latency_ms_p99: Option<f32>,

    // Power
    gpu_watts_avg: Option<f32>,
    gpu_watts_peak: Option<f32>,
    cpu_watts_avg: Option<f32>,
    energy_joules_total: Option<f32>,

    // I/O
    disk_read_bytes: Option<u64>,
    cold_load_seconds: Option<f32>,
}
```

**Tests for `RunStore`:**
- Append 100 runs, restart, verify index rebuilds correctly.
- Append runs for 5 different models, `list_models()` returns all 5.
- `recent("phi3-mini", 5)` returns newest first.
- Corrupted index line: skip with warn-log, do not crash.
- `baseline()` with N=0 or N>available returns sensible defaults.

### B. The telemetry sampler trait

Different runtimes expose metrics differently. Don't hardcode any of them into the main loop. Define a trait, ship implementations as plugins.

```rust
#[async_trait]
trait TelemetrySource: Send + Sync {
    fn name(&self) -> &str;                              // "vllm", "llama-cpp", "ultralytics-stdout"
    fn applies_to(&self, proc: &ProcessSnapshot) -> bool; // detection logic
    async fn sample(&mut self, proc: &ProcessSnapshot) -> Result<TelemetryFrame>;
}

struct TelemetryFrame {
    pid: u32,
    timestamp: SystemTime,
    tokens_per_sec: Option<f32>,
    fps: Option<f32>,
    latency_ms: Option<f32>,
    kv_cache_pct: Option<f32>,
    concurrent_requests: Option<u32>,
    extras: HashMap<String, f64>,  // for runtime-specific metrics
}
```

The sampler runs on its own task (Tokio). Frames flow into a per-PID accumulator that updates `RunMetrics` on every tick. Decoupling means Prometheus scraping, stdout parsing, and stderr tailing can coexist without blocking the 1-second tick.

**Tests:**
- Mock `TelemetrySource` — verify `applies_to` is called before `sample`.
- Verify samples flow into the right PID accumulator (no cross-contamination across PIDs).
- Verify a panicking sampler does not bring down the runtime — wrap in `tokio::task::spawn` with crash isolation.

### C. Comparison & baseline engine

`src/analysis/compare.rs`:

```rust
struct Baseline {
    model: String,
    sample_size: usize,
    metrics: BaselineMetrics,  // mean + stddev for each numeric field
    computed_at: SystemTime,
}

struct Regression {
    metric: &'static str,      // "tokens_per_sec_avg"
    baseline: f32,
    current: f32,
    delta_pct: f32,            // +18.0 means 18% worse
    severity: Severity,        // Info | Warn | Critical
}

fn detect_regressions(record: &RunRecord, baseline: &Baseline) -> Vec<Regression>
```

Rules: warn at >10% degradation, critical at >25% (configurable). Always include sample size; do not flag regressions with baseline n<3.

**Tests:**
- Stable baseline + matching record = no regressions.
- Single 30%-slower record = critical regression.
- Tiny sample baseline (n=2) returns no regressions even if values differ wildly.
- Regression direction matters: faster is not a regression even if magnitude is large.

---

## TIER 1 — ship-blockers

### 1.1 Per-model run history viewer

**User story:** I run `edge_monitor history phi3-mini` and see my last 10 runs with peak metrics, sorted newest first. Or I press `h` in the TUI and a history overlay opens for the focused model.

**Pseudocode (CLI):**

```
edge_monitor history [MODEL] [--limit N] [--json]

if MODEL is None:
    print table of (model_name, run_count, last_run_at, last_status)
    return

records = run_store.recent(MODEL, limit or 20)
if --json: emit JSON array, return

for r in records:
    line = format!(
        "{idx:>3}  {when:%Y-%m-%d %H:%M}  {dur:>6}s  cpu {avg_cpu:>5.1}%  rss {rss:>6}MB  vram {vram:>6}MB  {tps_or_fps}  {exit}",
        ...
    )
    color line by exit_reason (green=clean, yellow=manual kill, red=oom/crash)
```

**Pseudocode (TUI overlay):**

```
on key 'h' in any process panel:
    model = selected_row.model_name or selected_row.process_name
    overlay = HistoryOverlay::new(run_store.recent(model, 20))
    app.push_overlay(overlay)

overlay renders:
  ┌ History: phi3-mini ────────────────────────────────────┐
  │  #  When           Dur   Avg CPU  Peak VRAM  tok/s  Exit│
  │ 14  2026-04-28 10  4m22s  38.2%   4093 MB    37.4   ✓   │
  │ 13  2026-04-27 18  6m11s  41.8%   4101 MB    36.9   ✓   │
  │ ...                                                     │
  │                                                         │
  │ [b]aseline · [c]ompare · [Esc] close                    │
  └─────────────────────────────────────────────────────────┘
```

**Tests:**
- Unit: history command with no runs prints "no history" and exits 0.
- Unit: --json output validates against schema (`schemars` derive).
- Integration: spawn fake process with known model, kill it, run history command, assert record appears.
- TUI: overlay renders correctly with 0, 1, 20, 100 runs (vertical scroll for >20).
- TUI: pressing 'h' on a row with no model_name shows "no history" message, not a crash.

### 1.2 Tokens/sec for LLMs (vLLM + llama.cpp + Ollama)

**Strategy:** Three samplers, all using the `TelemetrySource` trait.

#### 1.2a vLLM Prometheus sampler

Detection:
```
applies_to(proc):
    return proc.cmdline contains "vllm" or proc.name == "vllm"
           or proc has env VLLM_*
```

Endpoint discovery:
```
fn discover_endpoint(proc) -> Option<Url>:
    1. parse --port and --host from cmdline (default 8000, 0.0.0.0)
    2. try http://127.0.0.1:{port}/metrics with 500ms timeout
    3. cache the URL on the sampler keyed by pid
```

Scrape:
```
async fn sample(proc):
    text = http_get(endpoint, timeout=500ms)?
    metrics = prometheus_parse::parse(text)?
    frame = TelemetryFrame {
        tokens_per_sec: metrics.get("vllm:avg_generation_throughput_toks_per_s"),
        kv_cache_pct: metrics.get("vllm:gpu_cache_usage_perc") * 100,
        concurrent_requests: metrics.get("vllm:num_requests_running"),
        ...
    }
    return frame
```

#### 1.2b llama.cpp server sampler

Same shape, different metric names. `llama_server_*` namespace. Parse `llama_server_n_decode_total` and divide by elapsed wall time for tok/s if direct gauge isn't exposed.

#### 1.2c Ollama API sampler

Ollama doesn't expose Prometheus. It has `/api/ps` (list loaded models) and embeds tok/s in response JSON during generation, which we cannot intercept. So:
- `/api/ps` confirms which model is loaded → enriches `model_name` and `model_fingerprint`.
- For tok/s, fall through to stdout parsing (next bullet).

#### 1.2d Stdout/stderr line-parser sampler

Many runtimes print tok/s lines. Tail `/proc/<pid>/fd/1` and `/fd/2` is unreliable across kernels — instead, attach via `bpftrace` if available, or fall back to wrapping the process. Realistic v1: only enable this when the user runs the process under `edge_monitor exec -- vllm serve ...` (we own the stdio).

```
edge_monitor exec [--name LABEL] -- COMMAND...
   forks COMMAND with piped stdio
   tees stdout/stderr to original tty AND to a parser
   parser regexes:
       LLAMA_TPS = r"eval time = .* (\d+\.\d+) tokens per second"
       VLLM_TPS  = r"Avg generation throughput: ([\d.]+) tokens/s"
       ULTRA_FPS = r"Speed: ([\d.]+)ms inference"  # latency, derive fps = 1000/lat
   each match → TelemetryFrame
```

This `exec` wrapper is also useful for vision (next feature) and is the only honest way to get tok/s out of llama.cpp CLI.

**Tests:**
- Unit (vLLM): feed canned `/metrics` text to the parser, assert `tokens_per_sec=37.4`.
- Unit: HTTP timeout returns `Err`, sampler logs warn but does not panic.
- Unit (regex): test fixture with 50 lines of real llama.cpp output, all tok/s values extracted.
- Integration: spin up a tiny mock HTTP server returning canned metrics, attach sampler, verify frame in store.
- Manual: run `vllm serve` for real with a tiny model, verify tok/s matches what vLLM logs.

### 1.3 Regression warning on exit

**User story:** A run finishes. If any tracked metric is materially worse than the rolling baseline, print a warning to stderr and surface it in the Audit panel of the TUI.

**Pseudocode:**

```
on lifecycle.exit(pid):
    record = build_run_record(pid)
    run_store.append(record)

    if record.model_name is None: return
    baseline = run_store.baseline(record.model_name, window=10)
    if baseline is None or baseline.sample_size < 3: return

    regressions = detect_regressions(&record, &baseline)
    if regressions.is_empty(): return

    for r in regressions where severity >= Warn:
        emit_event(Event::Regression { record_id, regression: r })
        if headless: tracing::warn!(...)
        else: app.audit_panel.push(format!("⚠ {} regressed {:+.1}% vs baseline", r.metric, r.delta_pct))
```

**Tests:**
- Unit: 10-run baseline at 40 tok/s, new run at 28 tok/s → critical regression on `tokens_per_sec_avg`.
- Unit: 10-run baseline at 40 tok/s, new run at 41 tok/s → no regression (improvement).
- Unit: 2-run baseline → no regressions emitted (sample too small).
- Integration: spawn → kill → spawn slower → kill → assert TUI audit panel shows regression line.

---

## TIER 2 — high-value, ship soon after launch

### 2.1 Power & thermals

**Sources, priority order:**
1. NVML — `nvmlDeviceGetPowerUsage`, `nvmlDeviceGetTemperature`. Already attached.
2. RAPL — `/sys/class/powercap/intel-rapl:*/energy_uj` for CPU package power. Read at tick boundaries, divide delta by interval.
3. tegrastats — for Jetson, parse output. Wrap as a separate `TegraSampler`.

Add to `RunMetrics`: `gpu_watts_*`, `cpu_watts_*`, `gpu_temp_c_peak`, `energy_joules_total = ∫ (gpu_watts + cpu_watts) dt`.

Surface in the Vitals panel as `GPU 142W / 71°C`. In history, add a column "energy" in joules — this becomes "watts per token" downstream which is the metric that nobody else publishes.

**Tests:**
- Unit: feed canned RAPL counter values across two ticks, assert wattage calculation.
- Unit: counter wraparound (RAPL is 32-bit on some CPUs) handled correctly.
- Manual: run `stress-ng --cpu 8` and verify CPU watts climbs.

### 2.2 Disk I/O during model load

`/proc/<pid>/io` gives per-process `read_bytes`. On process start, sample every tick. When `read_bytes` plateaus for >2s after a sustained burst, declare cold-load complete and record:

```
ColdStartStats {
    duration_seconds,
    bytes_read,
    avg_throughput_mbps,
    peak_throughput_mbps,
}
```

Display: "Loaded 4.2GB in 12.1s (3.5 GB/s)" in the registry row's tooltip and in the run record.

**Tests:**
- Unit: synthetic byte counter trajectory → correct cold-load detection.
- Unit: process never plateaus (streaming inference) → cold-load reported as None after 60s timeout.
- Integration: load a real GGUF model, verify cold_load_seconds is populated.

### 2.3 Prometheus exporter

Expose `edge_monitor`'s own metrics over HTTP. New crate dep: `prometheus` or `metrics-exporter-prometheus`.

```
[telemetry]
prometheus_bind = "127.0.0.1:9472"   # 0 disables
```

Metrics to export:
```
edge_monitor_processes_total{category="inference"}
edge_monitor_run_tokens_per_sec{model="phi3-mini",pid="..."}
edge_monitor_run_vram_bytes{model="phi3-mini",pid="..."}
edge_monitor_governor_kills_total{reason="..."}
edge_monitor_regressions_total{model="...",metric="..."}
```

This is the lowest-effort highest-leverage feature for fleet users. Ship it.

**Tests:**
- Unit: format a snapshot, verify against a golden Prometheus text file.
- Integration: bind to ephemeral port, GET /metrics, parse with `prometheus-parse`, assert family exists.

---

## TIER 3 — the differentiating features

These are the features competitors don't have. Build them after Tier 1 and 2 are solid.

### 3.1 Model fingerprinting

```rust
fn fingerprint_model_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = Sha256::new();
    // Hash size + first 1MB + last 64KB. Full hash on big models is too slow.
    hasher.update(&len.to_le_bytes());
    let mut head = [0u8; 1_048_576];
    let n = file.read(&mut head)?;
    hasher.update(&head[..n]);
    if len > 1_048_576 + 65_536 {
        file.seek(SeekFrom::End(-65_536))?;
        let mut tail = [0u8; 65_536];
        file.read_exact(&mut tail)?;
        hasher.update(&tail);
    }
    Ok(format!("sha256-head1m-tail64k:{:x}", hasher.finalize()))
}
```

Cache fingerprints by inode+mtime so we don't re-hash a 40GB file every run. Persist cache to `~/.cache/edge_monitor/fingerprints.json`.

Use it: `recent(model_name, n)` filtered by fingerprint = "show me runs of *this exact* phi3-mini Q4_K_M, not the Q8 variant."

**Tests:**
- Unit: same file → same fingerprint.
- Unit: file modified by 1 byte at start → different fingerprint.
- Unit: file with same head+tail but different middle → same fingerprint (acceptable: document this as design tradeoff).
- Integration: cache is reused across runs (assert no second hash call).

### 3.2 Cold-start vs steady-state separation

After cold-load completes (see 2.2), restart the metrics accumulators with a `steady_state_started_at` watermark. Final `RunMetrics` reports both:

```
RunMetrics {
    tokens_per_sec_avg_overall,
    tokens_per_sec_avg_steady_state,
    ...
}
```

UI shows both: `tok/s 32.1 (38.7 steady)`. History comparison uses the steady-state value by default.

**Tests:**
- Unit: synthetic frame stream with low tps for first 10s then high tps → steady-state value > overall value.
- Unit: process exits before reaching steady state → steady-state metrics are None.

### 3.3 KV cache pressure

Already partially covered by vLLM sampler (1.2a). Extend:
- Add to `RunMetrics`: `kv_cache_peak_pct`, `kv_cache_avg_pct`, `kv_cache_evictions_total` (vLLM exposes this).
- In TUI registry row, show `KV 87%` in red when >80%.
- In history, flag runs where peak hit 100% — that's a saturation event.

**Tests:**
- Unit: synthetic vLLM metrics with KV at 95% → row colored red.
- Unit: KV cache evictions > 0 → flagged in run record exit summary.

### 3.4 Concurrent-request awareness

Pull from vLLM `vllm:num_requests_running` and `vllm:num_requests_waiting`. Track both peaks and time-weighted averages.

History view distinguishes:
```
#14  serving 8 concurrent (peak)  →  20.1 tok/s/req · 161 tok/s aggregate
#13  serving 1 concurrent (peak)  →  158 tok/s/req · 158 tok/s aggregate
```

This makes "is my server scaling well" answerable.

**Tests:**
- Unit: time-weighted average computation on a step function (1 req for 10s, 8 for 50s) — verify result.
- Unit: division by zero guarded when concurrent=0.

### 3.5 "Why did this die?" classification

```rust
enum ExitReason {
    CleanExit,                          // exit_code == 0
    UserSignal { signal: i32 },         // SIGTERM/SIGINT from a real terminal
    GovernorKill { reason: String },    // we did it
    OutOfMemory { ram: bool, vram: bool },
    Segfault,
    CudaError { last_msg: Option<String> },
    Crash { exit_code: i32 },
    Unknown,
}

fn classify_exit(record: &PartialRecord, recent_logs: &[LogLine]) -> ExitReason {
    if record.exit_code == Some(0): return CleanExit
    if record.killed_by_governor: return GovernorKill { reason }
    if record.signal == Some(SIGSEGV): return Segfault
    if record.signal == Some(SIGKILL):
        // Check dmesg for OOM in last 5s
        if dmesg_oom_killed_pid(record.pid, within=5s): return OutOfMemory { ram: true, vram: false }
    if recent_logs contains "CUDA out of memory": return OutOfMemory { ram: false, vram: true }
    if recent_logs contains "CUDA error": return CudaError { last_msg }
    if record.exit_code != Some(0) and record.exit_code is Some(_): return Crash { exit_code }
    return Unknown
}
```

Reading dmesg: parse `/var/log/kern.log` or `journalctl -k --since "10 seconds ago"`. Fall back gracefully if neither is readable.

Surface in history with icons: ✓ ⚠ ☠ 🔥. In tooltip, show last 3 log lines before exit.

**Tests:**
- Unit: each `ExitReason` arm has a fixture-driven test.
- Unit: dmesg parse handles standard Ubuntu/Debian/RHEL formats.
- Integration: spawn a process that allocates until OOM (`stress-ng --vm 1 --vm-bytes 100%`) — assert OutOfMemory classification.

### 3.6 Vision: fps & latency

Honest scope: only Ultralytics/YOLO is auto-detectable via stdout. Everything else needs the `exec` wrapper or user instrumentation.

Ultralytics sampler:
```
regex: r"Speed: ([\d.]+)ms preprocess, ([\d.]+)ms inference, ([\d.]+)ms postprocess"
fps = 1000 / (pre + inf + post)
inference_latency_ms = inf
```

Optional: provide a tiny Python helper `edge_monitor_probe` users can `import` to push frame timestamps to a Unix socket the daemon listens on. Document but don't require.

**Tests:**
- Unit: regex against a 200-line Ultralytics log fixture, all values extracted.
- Unit: probe socket protocol — fixture sends 100 frames in 1s, daemon reports ~100 fps.

### 3.7 Comparison mode CLI

```
edge_monitor compare phi3-mini --runs=5
edge_monitor compare phi3-mini llama-3.1-8b --runs=10 --metric=tokens_per_sec_avg
```

Output:
```
              phi3-mini (n=5)        llama-3.1-8b (n=10)
tok/s avg     38.4 ± 2.1            21.7 ± 0.8
peak VRAM     4.1 GB                15.3 GB
W/token       0.082                 0.341
cold load     3.2s                  18.6s
```

`--json` emits structured output for scripting.

**Tests:**
- Unit: golden output comparison against a fixture run store.
- Unit: --json validates against schema.

---

## Cross-cutting requirements

### Configuration additions

Extend `edge_monitor.toml`:

```toml
[storage]
run_store_path = "~/.local/share/edge_monitor"
fingerprint_cache = "~/.cache/edge_monitor/fingerprints.json"
keep_runs_per_model = 200      # hard cap, oldest pruned

[telemetry]
prometheus_bind = ""            # empty disables
vllm_scrape = true
llamacpp_scrape = true
ollama_api = true
stdout_parse = true             # only for processes started via `exec`

[regression]
warn_pct = 10.0
critical_pct = 25.0
baseline_window = 10
min_baseline_samples = 3

[power]
rapl_enabled = true
nvml_power_enabled = true
tegrastats_enabled = false
```

`config.validate()` must reject negative percentages, baseline_window < 1, etc.

### Test discipline

For each feature in this doc:
1. **Unit tests** for all pure logic. Aim ≥ 90% line coverage on new modules.
2. **Property tests** for anything with safety invariants (governor, fingerprint stability).
3. **Integration tests** under `tests/` — spawn fake processes via fixtures, assert end-to-end behavior.
4. **Manual smoke scripts** under `scripts/manual/` — one per feature, runnable on a real GPU box.

Run before every commit:
```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo deny check        # license + advisory check
```

### Observability of the monitor itself

Every new feature emits structured tracing events:

```
tracing::info!(target: "history", model = %m, runs = n, "history queried")
tracing::warn!(target: "regression", metric = %r.metric, delta = r.delta_pct, "regression detected")
```

The Prometheus exporter (2.3) automatically exposes feature usage counters.

### Documentation

After each tier completes:
1. Update `FEATURES.md` — move features from "out of scope" to "implemented."
2. Add a section to `README.md` with a screenshot or asciinema cast.
3. Update `docs/configuration.md` for any new TOML keys.
4. Append to `CHANGELOG.md` under the next version.

---

## Suggested execution order

Two-week sprint per tier is realistic for a focused agent. Do not parallelize tiers — the foundations matter.

```
Week 1-2:  Foundations A, B, C
Week 3:    Tier 1.1 — history viewer (CLI + TUI)
Week 4:    Tier 1.2 — vLLM + llama.cpp samplers + exec wrapper
Week 5:    Tier 1.3 — regression detection
           >>> LAUNCH HERE if Tier 1 quality is high <<<
Week 6:    Tier 2.1 — power & thermals
Week 7:    Tier 2.2 — cold-start I/O
Week 8:    Tier 2.3 — Prometheus exporter
Week 9-10: Tier 3 — pick 2-3 differentiators based on user feedback
```

The launch gate is end of Week 5. Do not push to Tier 2 if Tier 1 has known bugs or coverage gaps.

---

## Anti-goals (do not implement, even if tempting)

- Web UI — Prometheus + Grafana is the answer.
- Cloud cost tracking ($/inference) — pricing data goes stale.
- ROS2 detection — separate project, separate launch.
- Multi-host fleet aggregation — Prometheus solves it.
- Anomaly detection via ML — premature, no data shape yet.
- Automatic regression remediation — too risky, governor scope is clear.

If a user asks for these post-launch, log it. Do not build it without ≥10 independent requests.

---

## Definition of done for each feature

A feature is "done" when:
- [ ] Unit tests pass with ≥90% coverage on new modules
- [ ] Integration test exercises the feature end-to-end
- [ ] Manual smoke script committed under `scripts/manual/`
- [ ] FEATURES.md updated
- [ ] README.md has a usage example or screenshot
- [ ] CHANGELOG.md has an entry
- [ ] `cargo clippy -- -D warnings` clean
- [ ] No new `unwrap()` outside tests
- [ ] No new `unsafe` without a `// SAFETY:` comment justifying it
- [ ] No regression in existing test suite (173 tests must still pass)

---

## Final note to the implementing agent

Resist scope creep. Every feature in this doc is here because it earns its keep. Every feature *not* in this doc was deliberately cut. If you find yourself adding "while I'm here, let me also..." — stop. Open an issue. Move on.

The launch gate is Tier 1. The differentiator is the governor + run history + regression detection working together. Everything else is gravy.