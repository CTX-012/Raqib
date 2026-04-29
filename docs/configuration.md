# Configuration

`edge_monitor` loads TOML configuration from, in order of precedence:

1. The path passed to `--config <PATH>`.
2. `./edge_monitor.toml` in the current working directory.
3. Built-in safe defaults (dry-run, allowlist covers shells + init,
   1 Hz ticks, in-memory-only audit).

`--dry-run` on the command line always wins over whatever the file
specifies for `policy.enforce`.

See [../edge_monitor.toml.example](../edge_monitor.toml.example) for a
commented starting point.

## `[runtime]`

| Field                | Type   | Default | Meaning |
|----------------------|--------|---------|---------|
| `tick_interval_ms`   | u64    | `1000`  | Sample-and-decide cadence. Must be >0. |
| `render_interval_ms` | u64    | `100`   | UI redraw rate. Must be >0. |
| `completed_history`  | usize  | `50`    | Bounded ring of finished processes shown in the Completed panel. |
| `audit_history`      | usize  | `100`   | Bounded ring of audit entries shown in the Audit panel. |
| `audit_log_path`     | String | `""`    | If non-empty, every governor decision (manual + automated) is appended as JSONL to this file. |
| `summary_log_path`   | String | `""`    | If non-empty, every `LifecycleSummary` is appended as JSONL to this file. |

Both paths default to empty strings, meaning "disabled — rely on the
in-memory ring buffers only." Set them when running under systemd or in
production so the governor decision trail survives restarts.

## `[policy]`

| Field                    | Type    | Default | Meaning |
|--------------------------|---------|---------|---------|
| `allowlist`              | \[str\] | shells + init | Process names that the automated governor never kills. Manual kills prompt for an explicit override. |
| `blocklist`              | \[str\] | `[]`    | Process names always treated as kill candidates, even when not classified as AI. |
| `default_ai_action`      | enum    | `"Kill"` | `"Allow"` or `"Kill"`. What to do with AI-classified processes that match neither list. |
| `sigterm_grace_secs`     | u64     | `5`     | Seconds between SIGTERM and SIGKILL. Must be ≥1 (validated at load time). |
| `enforce`                | bool    | `false` | `true` = send real signals. `false` = log only. `--dry-run` forces this to `false`. |
| `rate_limit_max_kills`   | u32     | `3`     | Max automated kills inside `rate_limit_window_secs`. `0` disables. |
| `rate_limit_window_secs` | u64     | `60`    | Sliding-window size for the rate limit. |

## `[storage]`

The typed run store backs both the `edge_monitor history` subcommand and
the Tier 1.3 regression detector. It writes one JSONL file per day under
`run_store_path`, plus a small index file for fast `recent(model, N)`
queries.

| Field                  | Type   | Default                              | Meaning |
|------------------------|--------|--------------------------------------|---------|
| `run_store_path`       | String | `"~/.local/share/edge_monitor"`      | Directory the run store writes into. `~/` expands to `$HOME`. Set to `""` to disable persistence — the history subcommand returns no rows and the regression detector stays silent. |
| `fingerprint_cache`    | String | `"~/.cache/edge_monitor/fingerprints.json"` | JSON cache so the model-fingerprinter doesn't re-hash multi-gigabyte weight files on every AI process exit. `""` disables the cache (every fingerprint re-computes from disk). |
| `keep_runs_per_model`  | u32    | `200`                                | Hard cap on retained `RunRecord` entries per model name. Older entries are pruned at the next exit. Increase if you want a longer baseline window for regression detection. Must be `> 0`. |

## `[regression]`

Tier 1.3 baseline-vs-current detector. Runs at every AI process exit and
compares the fresh `RunRecord` against the rolling baseline of prior
runs for the same model.

| Field                    | Type | Default | Meaning |
|--------------------------|------|---------|---------|
| `warn_pct`               | f32  | `10.0`  | Percent worse than baseline that promotes a metric drift to a `Warn` severity regression. Must be finite and ≥0. |
| `critical_pct`           | f32  | `25.0`  | Same idea for `Critical` severity. Must be finite, ≥0, and ≥`warn_pct`. |
| `baseline_window`        | u32  | `10`    | Number of prior runs (per model) folded into the rolling baseline. Larger → smoother but slower to track real drift. Must be `> 0`. |
| `min_baseline_samples`   | u32  | `3`     | Minimum prior runs required before the detector fires. Below this the detector stays silent (small samples produce noisy false positives). Set to `u32::MAX` (`4294967295`) to disable regression detection without removing the section. |
| `baseline_strategy`      | String | `"mean"` | Per-metric central-tendency strategy. `"mean"` (historical default) or `"median"` (robust to a single bad run). Case-insensitive; any other value is rejected at load time. |
| `drop_outliers`          | bool | `false` | Drop runs whose key metric is >2σ from the median before computing the baseline. The flagged run ids still surface on `Baseline.outlier_run_ids` for review. Most useful paired with `baseline_strategy = "median"` on noisy fleets. |

## `[telemetry]`

Toggles for the optional samplers driven by `telemetry::Dispatcher`
(latest.md Tier 1.2 + 2.3 + 3.6). All HTTP samplers fail fast on
connection refused, and the dispatcher's `applies_to` gate keeps
non-AI processes from being touched, so the defaults are safe even
on hosts that don't run any of these runtimes.

| Field                  | Type   | Default | Meaning |
|------------------------|--------|---------|---------|
| `vllm_scrape`          | bool   | `true`  | vLLM Prometheus scraper (Tier 1.2a). Detects `vllm serve` / `vllm.entrypoints.*` / `python -m vllm` / `VLLM_*` env vars; scrapes `http://127.0.0.1:<port>/metrics`. |
| `llamacpp_scrape`      | bool   | `true`  | llama.cpp `llama-server` scraper (Tier 1.2b). Default port 8080. |
| `ollama_api`           | bool   | `true`  | Ollama `/api/ps` poller (Tier 1.2c). Confirms which model is loaded so the dispatcher can promote the model name onto the `RunRecord`. |
| `prometheus_bind`      | String | `""`    | Built-in Prometheus exporter (Tier 2.3). Empty disables. Otherwise `host:port` (e.g. `127.0.0.1:9472`). The exporter does no auth or TLS itself; bind to loopback and put a reverse proxy in front for either. |
| `vision_probe_socket`  | String | `""`    | Vision probe Unix-domain socket (Tier 3.6). Empty disables. Otherwise a filesystem path the vision-inference loop can push line-delimited `{"pid": <u32>, "frame_at_ns": <u64>}` JSON events to. Each event aggregates into a per-PID rolling 1s window and instantaneous fps flows into the telemetry accumulator. |

NVML GPU power/°C and Intel RAPL CPU power are unconditionally read
when the underlying interface is present (latest.md Tier 2.1). There
is no `[power]` config section today; the dispatcher falls through to
`None` watts on hosts where neither interface is available.

## Minimal production example

```toml
[runtime]
tick_interval_ms = 1000
audit_log_path   = "/var/log/edge_monitor/audit.jsonl"
summary_log_path = "/var/log/edge_monitor/summaries.jsonl"

[policy]
allowlist = ["sshd", "systemd", "bash", "zsh", "roslaunch", "my_perception_node"]
enforce = true
sigterm_grace_secs = 10
rate_limit_max_kills = 3
rate_limit_window_secs = 60
```

## Changing policy without restarting

Not supported in v1. Phase 2 includes config hot-reload on `SIGHUP`.
For now, restart the binary to pick up a new config.
