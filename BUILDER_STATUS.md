# Builder coordination state

This file is the single source of truth for builder activity.
Every builder must read this BEFORE starting work and update it
AFTER claiming a task.

Stale entries (>4 hours old) may be reclaimed.

Note: this repository has no git remote configured at the moment, so
"push" steps in the builder protocol are local-only commits. Builders
on the same machine still coordinate via this file (which is committed
to history) and via reading concurrent worktrees.

## Active claims

- builder_id: builder-claude
  feature: Tier 3.3 KV cache pressure (avg_pct, evictions_total, TUI red >80%, history saturation flag)
  files:
    - src/storage/run_store.rs (extend RunMetrics)
    - src/telemetry/source.rs (extend TelemetryFrame)
    - src/telemetry/accumulator.rs (avg + evictions tracking)
    - src/telemetry/samplers/vllm_prometheus.rs (scrape vllm:num_preemptions_total)
    - src/ui/panels/registry.rs (KV column + red >80%)
    - src/ui/panels/history_overlay.rs (KV saturation icon)
  started: 2026-04-28T10:35:58Z
  branch: builder-claude/tier-3-3-kv-cache-pressure
  eta: 2 hours

## Recently completed (last 24h)

(none yet)

## Locked files

(none — use this section for multi-file refactors)
