# edge_monitor — Linux Audit Report

- **Generated:** 2026-04-28 10:03:00
- **Duration:** 1.5 minutes
- **Project:** /home/faisal/edge_monitor
- **Evidence:** `audit_results/evidence`

## Summary

| Result | Count |
|---|---|
| PASS | 16 |
| FAIL | 5 |
| WARN | 8 |
| SKIP | 2 |
| **Total** | **31** |

## ⚠ Launch blockers

2 S0/S1 failure(s). Cannot launch until resolved.

- **[F.1]** (S1) --no-ui --ticks 5 produces ≥5 tick lines — Only 0 tick markers. Headless mode silent or broken.
- **[G.1]** (S1) Prometheus /metrics serves valid output — Endpoint responds but content sparse: 4 metrics, 9 HELP.

## Findings

| ID | Status | Sev | Description | Notes |
|---|---|---|---|---|
| A.1 | ✓ PASS |  | cargo build --release succeeds |   |
| A.2 | ✗ FAIL | S2 | cargo clippy clean | See A2_clippy.txt for diagnostics.  |
| A.3 | ⚠ WARN |  | cargo fmt clean | Formatting drift; not a blocker.  |
| A.4 | ✓ PASS |  | --version runs and returns semver | edge_monitor 0.1.0  |
| B.1 | ✓ PASS |  | cargo test runs all green | 300 tests across 6 binaries  |
| B.2 | ⚠ WARN |  | no empty test binaries | 2 binaries have ZERO tests (scaffolds without content). 4 have real tests.  |
| B.3 | ✓ PASS |  | test count via --list | 300 test functions enumerated.  |
| B.4 | ⚠ WARN |  | tests appear to exercise real behavior | avg per-test runtime: 0.00 ms (300 tests in 0s) — under 0.5ms/test suggests assertions on constants. Spot-check test bodies.  |
| C.1 | ⚠ WARN | S3 | no .unwrap() in production code | 230 unwrap() calls found. See C1_unwraps.txt for file:line list.  |
| C.2 | ⚠ WARN | S3 | no .expect() in production code | 32 expect() calls. Linux audit flagged audit.rs:76 and stdout_parser.rs:40,49,62. Document or refactor.  |
| C.3 | ⚠ WARN | S4 | no TODO/FIXME markers | 0 0 markers in production code.  |
| D.1 | ✓ PASS |  | no VATCH branding leakage |   |
| D.2 | ✓ PASS |  | no old Windows vocabulary |   |
| D.3 | ✓ PASS |  | config is TOML (no stray config.json) |   |
| D.4 | ✓ PASS |  | ratatui dependency present |   |
| D.5 | ✗ FAIL | S2 | raw ANSI escapes in production | 0 0 files use raw ANSI. Suggests TUI not ported to ratatui.  |
| E.1 | ✓ PASS |  | history subcommand exists (Tier 1.1) |   |
| E.2 | ⚠ WARN |  | compare subcommand exists | Tier 3 feature; may not be implemented yet.  |
| E.3 | ⚠ WARN |  | exec wrapper subcommand (Tier 1.2) | Required for stdout-parsing tok/s. May rely only on Prometheus scraping.  |
| E.4 | ✗ FAIL | S2 | --no-ui headless mode |   |
| E.5 | ✓ PASS |  | --dry-run flag (governor safety) |   |
| F.1 | ✗ FAIL | S1 | --no-ui --ticks 5 produces ≥5 tick lines | Only 0 tick markers. Headless mode silent or broken.  |
| F.2 | ✓ PASS |  | bad config rejected with non-zero exit |   |
| G.1 | ✗ FAIL | S1 | Prometheus /metrics serves valid output | Endpoint responds but content sparse: 4 metrics, 9 HELP.  |
| H.1 | — SKIP |  | Grafana dashboards directory exists | No grafana/ or dashboards/ directory found.  |
| I.1.CLAUDE | ✓ PASS |  | CLAUDE.md freshness | 0.1 days old  |
| I.1.HANDOFF | ✓ PASS |  | HANDOFF.md freshness | 4.0 days old  |
| I.1.README | ✓ PASS |  | README.md freshness | 4.1 days old  |
| I.1.FEATURES | ✓ PASS |  | FEATURES.md freshness | 0.1 days old  |
| I.2 | ✓ PASS |  | no misleading commit messages in last 20 |   |
| J.1 | — SKIP |  | tokens/sec ground-truth vs Ollama | --skip-slow specified.  |

## Reproduction

```bash
./verify-edge-monitor.sh --project-root '/home/faisal/edge_monitor'
```

Raw evidence in `audit_results/evidence`.
