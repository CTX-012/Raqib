# AUDIT — full-project verification sweep, 2026-07-15

*Phase 2 output of the completion+hardening run. Every finding is
categorized BLOCKER / SHOULD-FIX / DEFERRED / CLEAN. Findings written
here are the honest state, not the aspirational one; if the tester
didn't run a check, the check is marked "not run," not "clean."*

**Baseline at audit start:** `v1.3.2-40-ga22bcba` + local commit
`344b184` (Phase 1 docs; unpushed). Rust `cargo test --workspace`
1184 pass / 0 fail. `cargo clippy --workspace --all-targets --
-D warnings` clean. `npm --prefix web run test:browser` 223 / 0.

---

## 1. Mechanical gates — CLEAN

| Gate | Command | Result |
| --- | --- | --- |
| Rust tests | `cargo test --workspace` | **1184 passed, 0 failed** |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | **clean, 0 warnings** |
| Browser render-gate | `npm --prefix web run test:browser` | **223 passed, 0 failed** |
| Release build | `cargo build --release` (after `touch src/web/assets.rs`) | **built, embedded bundle etag `be2dcacf...`** |

---

## 2. Invariant tripwires — CLEAN (11 named + accessory pins green)

Every tripwire enumerated + run individually. Each returned 1 passed / 0 failed inside its test binary.

- `history_capture_is_wired_exactly_once_in_runtime` (`src/history/mod.rs`) — runtime.rs write-only invariant. OK.
- `send_sigterm_actuation_site_is_auto_actuate_gated` (`src/governor/executor.rs`) — every SIGTERM inside gated branch. OK.
- `send_sigkill_callers_are_gated` (`src/governor/executor.rs`) — same for SIGKILL escalation. OK.
- `no_chart_symbols_in_history_events_panel` (`src/ui/panels/history_events.rs`) — TUI overlay STOP #4 pin. OK.
- `render_reads_from_frozen_snapshot_not_runtime_history` (`src/ui/panels/history_events.rs`) — overlay reads frozen state. OK.
- `schema_paths_covers_every_schema_defining_file` (`tests/config_schema_firewall.rs`) — D108 completeness pin. OK.
- `config_schema_has_no_action_verb_fields` (`tests/config_schema_firewall.rs`) — original schema firewall. OK.
- `guard_would_catch_a_real_breach` (`tests/config_schema_firewall.rs`) — negative proof. OK.
- `auto_actuate_is_not_flagged` (`tests/config_schema_firewall.rs`) — deliberate exception. OK.
- `workload_rule_has_exactly_three_fields` (`tests/workload_rule_field_count_guard.rs`) — schema-count firewall. OK.
- `help_keys_match_action_variants` (`ux_contract/src/lib.rs`) — contract audit. OK.

Accessory pins in `tests/config_schema_firewall.rs` (5 total), `tests/recommendation_observe_only_guard.rs` (3), `tests/expect_rule_guard.rs` (1) — all green inside their integration binaries.

---

## 3. Live smoke — every surface exercised on release binary

Release binary launched on ephemeral port; puppeteer-core with system Chrome for the web probes.

### 3.1 Backend surfaces — CLEAN

- `GET /api/health` → HTTP 200
- `GET /api/snapshot` → vitals.gpu.temp_c=52.0, power_w=46.866, devices=1, vram_pct=15%; 18 AI workloads; 0 activity; 0 alerts
- `GET /api/history` → events=0, dead_pids=0 (fresh session)
- `GET /assets/index.js` → etag `be2dcacfdbe29b9d`, content-length 106452 (matches fresh D109 bundle)

### 3.2 Web modes — CLEAN

Each mode probed by puppeteer against the live release binary:

- **Dashboard** (`/`) — `theme-dark`, AI Workloads h2 + `vitals-panel-gpu-line` testid, GPU line reads `50°C / 46W` (measured, live).
- **History** (`?mode=history`) — `theme-dark`, `history-view` testid, 0 events/dead-pids.
- **Kiosk** (`?mode=kiosk`) — **`theme-hc`** (auto-hc fired), `kiosk-view` + `kiosk-tile-gpu`, GPU tile reads `50°C / 41W`. **0 interactive nodes** inside the view — glance-only invariant holds.
- **Timeline** (`?mode=timeline`) — `theme-dark`, `timeline-view` testid, alerts + activity + workloads side rail render.
- **Focus (no pid)** (`?mode=focus`) — `focus-view` + `focus-nopid` graceful state, picker shown.
- **Focus (pid)** (`?mode=focus&pid=52846`) — deep-dive works.
- **Invalid mode** (`?mode=garbage`) — URL rewritten to `/`; dashboard renders. Silent fallback works.

No page errors on any surface. No each_key_duplicate signals.

### 3.3 VRAM / GPU honesty — CLEAN, measured only (see 3.4)

Live values: Wire=52°C/46W, TUI=52°C/47W, VitalsPanel=50°C/46W, KioskView=50°C/41W. Measured path clean end-to-end.

### 3.4 TESTABILITY CEILING — HONEST DISCLOSURE

**The unmeasured VRAM/GPU rendering path is NOT live-verified on this host in this session.** The NVML driver is currently loaded and returning real values, so every live smoke shows the measured branch. The unmeasured branch (VRAM/GPU absent → `—` + `data-testid-unmeasured="true"`) is **pinned by tests** (`wire_gpu_temp_and_power_none_omit_fields_from_json`, `wire_sample_vram_none_omits_field_from_json`, D98 gate assertions on F1/F2/F3 fixtures with `gpu: null`), but real-data live-verification awaits driver unload.

**This audit does NOT claim "VRAM/GPU rendering fully verified live." It claims: measured half is live-verified; unmeasured half is code-tested only (multiple test layers pin it).**

---

## 4. Discipline sweep — investigator findings

### 4.1 Dead code / TODO / FIXME / HACK — CLEAN

- `rustc` dead-code warnings: **0**.
- `clippy` unused-code warnings: **0**.
- `grep -rn "TODO\|FIXME\|XXX\|HACK" src/` returned 3 hits, all `sha256-XXX` example strings inside doc-comments — NOT genuine unfinished work.
- `web/src/` TODO/FIXME sweep: **zero hits**.

### 4.2 Origin sync verification — CEILING

`git fetch origin` from this credential-less shell fails: `could not read Username for 'https://github.com'`. Local remote-tracking ref `origin/l14-top-processes-sort` shows `729bdf7` (pre-D109). Operator confirmed the D109 push happened; audit relies on that verbal confirmation. Known ceiling, not a defect.

### 4.3 Design-doc status drift — SHOULD-FIX

- **`docs/PHASE5_HISTORY_DESIGN.md`** — says "design draft 2026-06-23"; History SHIPPED (D88-D97). Should be SHIPPED.
- **`docs/PHASE4_AUTOKILL_DESIGN.md`** — says "design locked 2026-06-05"; auto-kill SHIPPED (dormant + hardware-verified inert). Also flags "Manual-k force-SIGKILL UX (C4) deferred" — status unclear from a scan.
- **`docs/PHASE4_DESIGN.md`** — **no status header**. Could add pointer to CHANGELOG [1.3.x].

### 4.4 BOARD_AUDIT.md drift — SHOULD-FIX

- §2.6 line 111: "v1.3.2 in progress" — v1.3.2 tagged 2026-06-05.
- §2.6 line 117: gate is "221 assertions" — actually 223 post-D109.
- Phase table line 32: "TUI essentials-only pass NOT started" — contradicts §3 (4 defects, all shipped D107).

### 4.5 PENDING.md house-keeping — SHOULD-FIX

The resolved STOP #3 block contains "Landing 4 (web consumers) in progress" — outdated.

### 4.6 Test-coverage gaps — SHOULD-FIX

- **D107 FIX 1** — covered by `at_160x50_tier_is_wide_and_renders_without_panic`. OK.
- **D107 FIX 2** (`column_header_line`) — **gap**, no test.
- **D107 FIX 3** (ollama sampler `loaded.len() == 1` friendly-name preference) — **gap**.
- **D107 FIX 3** (runtime `looks_like_sha_blob` promotion at `runtime.rs:1187`) — **gap**.
- **D107 FIX 4** (vitals `LABEL_WIDTH=12` alignment) — **gap**.
- **D109 TUI GPU row** (vitals.rs 6th row rendering) — **gap**.

### 4.7 Deferred/unshipped follow-ons — DEFERRED

- **Versioning (v2.0.0?)** — HUMAN DECISION.
- **observer→supervisor** — HUMAN DECISION.
- **Auto-kill tiebreaker** — HARD STOP #1 (governor).
- **`KILL_ARM_WINDOW_SECS` removal** — HARD STOP #2 (CAR).
- **Live unmeasured-VRAM path verification** — hardware (driver reload).
- **Follow-on TUI candidates** — hardware identity, AlertState-on-wire, classifier consistency, top-processes on web, activity content parity.
- **`WireAlertEntry.timestamp`** — potential future CAR for Timeline unified stream.

---

## 5. Findings summary

| Category | Count | Details |
| --- | --- | --- |
| **BLOCKER** | **0** | Nothing broken; nothing regressed; no gate red. |
| **SHOULD-FIX** | **11** | 3 design-doc + 3 BOARD_AUDIT + 1 PENDING + 5 test-coverage. All autonomously fixable. |
| **DEFERRED** | **7** | Human/hardware/CAR-blocked. |
| **CLEAN** | Mechanical gates + 11 tripwires + backend surfaces + all 5 web modes + fallback + measured VRAM/GPU + zero dead code. | |

**Bottom line:** functionally clean. Zero blockers. Audit surfaces a docs-drift cluster + a test-coverage cluster from D107/D109; both autonomously fixable in Phase 3.

---

## 6. Phase 3 plan

1. **Doc-drift fixes** (1 landing) — PHASE5_HISTORY / PHASE4_AUTOKILL / PHASE4_DESIGN status; BOARD_AUDIT §2.6 + phase table; PENDING.md cleanup.
2. **Test coverage** (2 landings):
   - Landing A: `column_header_line` + `LABEL_WIDTH` tests.
   - Landing B: ollama friendly-name test + runtime `looks_like_sha_blob` test + TUI GPU row render test.
3. **Re-sweep** — cargo test / clippy / gate; confirm no regressions.
4. **PENDING.md completion summary + EXIT.**
