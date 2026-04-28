# Architecture

`edge_monitor` is a single-binary Linux daemon with a ratatui TUI layered
on top of a deterministic tick pipeline.

## The tick pipeline

```
Platform → Classifier → Lifecycle → Governor → UI/Headless
 (sample)   (annotate)   (track)    (decide)    (render/log)
```

One tick per second by default (`runtime.tick_interval_ms`). The TUI
renders at 10 Hz using cached state between samples so input feels
responsive even if the underlying sample rate is slow.

### Platform layer ([src/platform/](../src/platform/))

- `linux_proc.rs` — reads `/proc/<pid>/status`, `/proc/<pid>/stat`,
  `/proc/<pid>/cmdline`, `/proc/<pid>/environ`, `/proc/<pid>/cwd` via a
  mix of direct reads and `sysinfo::System`.
- `gpu_nvidia.rs` — optional NVML per-device + per-process VRAM. Returns
  `GpuSnapshot::default()` when NVML fails to initialise; the rest of the
  pipeline treats missing VRAM as `None`, never as 0.
- Produces a `PlatformSnapshot { system, processes, gpu }` every tick.

### Classifier ([src/classifier/](../src/classifier/))

Pure logic, no I/O except `script_sniff` reading user-supplied `.py`
files (capped at 64 KiB to avoid stalling on pathological inputs).

Priority order:

1. **Model file in cmdline** (`--model /path/foo.gguf`) — strongest
   signal, yields a concrete `ClassificationResult::ai_with_model` with
   the file-stem as `model_name`.
2. **Strong model env var** (`MODEL_PATH`, `LLAMA_MODEL_PATH`,
   `GGUF_MODEL`, `OLLAMA_MODELS`).
3. **Process name table** (`ollama`, `llama-server`, `vllm`, `deepspeed`,
   …).
4. **Cmdline keyword table** (`vllm.entrypoints`, `whisper`, `ultralytics`,
   …).
5. **Python source sniff** — imports + model-literal constructor calls
   (`YOLO("yolov8n.pt")`, `Llama(model_path="...gguf")`,
   `AutoModelForCausalLM.from_pretrained("...")`). Extracts the literal
   string so the UI shows the real model name.

### Lifecycle ([src/lifecycle/](../src/lifecycle/))

- `tracker.rs` — PID-based spawn/exit diff across snapshots. Handles PID
  reuse by promoting a "revived" PID into a fresh `ProcessLifecycle`.
- `mod.rs` — `ProcessLifecycle` carries `ResourceStats` (CPU sum/peak,
  RSS peak, VRAM peak, sample count) so run summaries at exit report
  avg/peak footprints without the runtime keeping full history.

### Governor ([src/governor/](../src/governor/))

- `policy.rs` — allowlist, blocklist, default AI action, grace period,
  rate limit.
- `executor.rs` — evaluates decisions per tick. Tracks a sliding
  `rate_limit_window_secs` deque of kill timestamps; once `rate_limit_max_kills`
  is hit, further candidates yield `KillAction::RateLimited` until old
  entries age out.
- `manual.rs` — user-initiated kills. Same executor surface as
  automated, but `source = Manual` in the audit trail.
- `audit.rs` — append-only JSONL writer with `replay()` helper. One
  line per decision.

### Storage ([src/storage/](../src/storage/))

- `log_store.rs` — append-only JSONL for `LifecycleSummary`. Separate
  file from the audit log so operators can tail one without drowning in
  the other.

### Runtime ([src/runtime.rs](../src/runtime.rs))

Owns the per-tick pipeline and the `RuntimeState` the UI renders. Folds
classifier output (`model_name`) and metrics (`cpu_pct`, `rss_mb`,
`vram_bytes`) into the lifecycle tracker each tick. Mirrors manual-kill
and automated-governor decisions into a bounded in-memory audit buffer
for the UI and into the persistent JSONL file when configured.

### UI / headless

- `ui/` — ratatui TUI with 6 panels (vitals, registry, rogues,
  culprits, completed, audit). 10 Hz render, 1 Hz sample.
- `main.rs::run_headless` — one `tick` per interval, one `INFO`-level
  log line per tick plus one per AI process (pid, name, category, model,
  cpu%, rss MB, vram).

## Safety invariants

| Rule                                  | Enforced by                            |
|---------------------------------------|----------------------------------------|
| Allowlisted processes never killed    | `GovernorPolicy::evaluate`             |
| Dry-run never emits signals           | `GovernorExecutor::send_sigterm/kill`  |
| Dry-run is the default                | `Config::default` + `safe_default`     |
| SIGTERM before SIGKILL                | `PendingKill::should_send_kill`        |
| Max 3 automated kills / 60s           | `GovernorExecutor::rate_limit_exceeded`|
| All decisions audited                 | `AuditWriter::append` + JSONL file     |

## Design decisions worth remembering

- `Platform` is an enum, not a trait. We have two backends planned for
  v1 (Linux x86, Jetson). Refactor to `Box<dyn PlatformMetrics>` at
  4+ backends.
- `#![allow(dead_code)]` at the crate root — many `pub` items are
  exercised only by `#[cfg(test)]` blocks, and a binary crate warns
  otherwise. Drop the attribute when splitting into `lib + bin`.
- No `std::thread::sleep`. The TUI parks on `event::poll(remaining)`;
  the headless loop parks on `mpsc::Receiver::recv_timeout`.
- Manual kill is two-step in the TUI (`k` arms, second `k` confirms).
  Allowlist still applies; the UI banners the armed state.
- Permission-denied on `/proc/<pid>/environ` yields an empty
  `HashMap`, not a dropped process — PID 1 and root daemons would
  otherwise disappear from the snapshot.
