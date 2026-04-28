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
