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

(none yet)

## Recently completed (last 24h)

- builder_id: builder-claude
  feature: Tier 3.3 KV cache pressure
  branch: builder-claude/tier-3-3-kv-cache-pressure
  finished: 2026-04-28T10:48:00Z
  commits:
    - 83e299f feat(telemetry): KV cache pressure (latest.md Tier 3.3)
  notes: 309 unit tests + 13 integration tests pass; clippy clean.
         RunMetrics gained kv_cache_avg_pct + kv_cache_evictions_total.
         TUI registry row now shows "KV NN%" red at >=80%, history
         overlay flags runs with peak >=99.5% with a "KV!" badge.

## Locked files

(none — use this section for multi-file refactors)
