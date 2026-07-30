# PENDING — things waiting on the human

## [SAFETY-INVESTIGATION] Governor kill-target selection — PRE-ARM verification for live-fire test — 2026-07-29

**READ-ONLY dispatch.** No code touched. Findings for the operator before arming `auto_actuate=true` on a host running real workloads (claude agent + ~20 ROS2 nodes).

### 🚨 VERDICT: The plan "kill the highest VRAM consumer" is WRONG. The plan is UNSAFE without additional protections. 🚨

**The governor does NOT select "the single highest VRAM consumer over threshold."** It fires on EVERY policy-Kill PID that has ANY of three breach signals — one of which (host thermal) is SYSTEM-WIDE and would sweep every AI-classified PID simultaneously.

### Q1 — What triggers a kill? (`src/governor/executor.rs:200-282`)

A PID becomes a kill candidate on this tick when **ALL** of:
1. `[governor] auto_actuate = true` (config) — Gate 1, `runtime.rs:2111`
2. Policy `evaluate(name, category) == Kill` — `policy.rs:68-92`
3. **At least ONE** of these three breach signals (widened D84):
   - Per-PID VRAM: `pid.vram_bytes / total_device_vram >= vram_critical_pct` (default 95.0%) — `threshold_breach.rs:181-191`
   - Per-PID RAM: `pid.rss_mb / system_total_ram_mb >= ram_critical_pct` (default 95.0%) — `threshold_breach.rs:200-203`
   - **Host thermal: `max(thermal_zones) >= thermal_red_c`** (default 95.0°C) — `threshold_breach.rs:241-266`. **HOST-WIDE — applies to EVERY AI-classified PID on the same tick.**
4. `(now - first_breached_at) >= kill_sustain_secs` (default 10s) — Gate 2, `runtime.rs:2117, 2141-2164`

Any one of `vram_breached || ram_breached || host_thermal` satisfies (3) — the `any_breach` line is `executor.rs:229`.

### Q2 — Which process gets killed? (THE safety question)

**NOT the highest VRAM consumer.** Every PID that satisfies Q1(1-4) becomes a candidate. When the rate limit forces a subset:
- **Ordering: `sorted_pids.sort_unstable()` — ASCENDING PID.** `executor.rs:82-84`. Lowest-numbered PID wins the budget when there's contention.
- Comment at `executor.rs:66-75` explicitly calls this a "Q4 STOPGAP" — "the long-term tiebreaker" (least-recent-activity) is a `KILL_ARM_WINDOW_SECS` CAR item in DEFERRED (PENDING.md, above).
- Rate limit: `rate_limit_max_kills = 3` per `rate_limit_window_secs = 60` (default; `policy.rs:62-63`). So **up to 3 kills per minute**, and if 20+ PIDs are all candidates, 3 will die per minute in LOWEST-PID-FIRST order until the breach clears.

### Q3 — Allowlist / exclusion (`src/governor/policy.rs:35-92`, `src/config.rs:512-528`)

Yes. `[policy] allowlist` (TOML) → `whitelist_names: HashSet<String>` → checked **FIRST** in `policy.evaluate` at `policy.rs:70-72`, returns `PolicyAction::Allow` → decision becomes `KillAction::Whitelisted` → actuation site at `runtime.rs:2130-2134` **filters SignalTermSent only** → whitelisted PIDs are structurally unreachable by the actuation loop.

**But: the default whitelist is minimal** (`policy.rs:37-46`):
```
sshd, bash, zsh, sh, systemd, init, kworker, kthreadd
```
This list DOES NOT include: `claude`, `ros2`, `robot_state_pub`, `parameter_bridg`, `range_converter`, `async_slam_tool`, `ekf_node`, `controller_server`, `smoother_server`, `planner_server`, `behavior_server`, `bt_navigator`, `waypoint_follow`, `velocity_smooth`, `lifecycle_manag`, `docker`, or ANY of the operator's live workloads.

**All of the operator's live workloads currently classify as `AICategory::Inference`** (confirmed via `/api/snapshot` — `workload_category: "agent"` for claude and `workload_category: "ros2"` for the rest, but internal `category` is `Inference` for all → `policy.rs:86-91` gates on `AICategory` → `default_ai_action` applies to them all).

### Q4 — Kill sequence (`src/governor/executor.rs:322-489`)

1. `send_sigterm(pid, name, cat)` — captures `pidfd_open(pid)` + `/proc/<pid>/stat` starttime BEFORE `libc::kill(pid, SIGTERM)`. Stores as `PendingKill` — `executor.rs:328-355`.
2. Wait `[policy] sigterm_grace_secs` (default **5s**, min 1s) — `policy.rs:60`, `executor.rs:481-488`.
3. `execute_after_grace()` walks pending PIDs whose grace expired → `send_sigkill(pid, name)`:
   - **PID-reuse guard**: re-checks pidfd (kernel-race-free) OR re-reads starttime; mismatch → **REFUSE the SIGKILL** with `KillAction::PidReusedAborted`. `executor.rs:378-410`.
   - On success: `pidfd_send_signal(fd, SIGKILL)` (preferred) or `libc::kill(pid, SIGKILL)` fallback — `executor.rs:416-430`.
4. A process that handles SIGTERM and exits cleanly within `sigterm_grace_secs` **will not receive SIGKILL** — the lifecycle reaper drops the entry, `execute_after_grace` sees nothing pending. A stubborn SIGTERM-ignoring process (ollama runners are the documented case, PENDING.md above §"HARD-BLOCKING follow-up") **will get SIGKILLed after grace**.

### Q5 — Exact arm / disarm config keys

**ARM** (all four required; two independent operator opt-ins per `runtime.rs:2059-2063`):
```toml
[governor]
auto_actuate = true               # THE opt-in. Default: false.
kill_sustain_secs = 10            # optional; default 10. Breach must persist this long.

[policy]
default_ai_action = "Kill"        # Second opt-in. Default: "Allow".
# The 3 optional protections:
allowlist = [                     # names → structurally kill-unreachable
    "claude", "ros2", "robot_state_pub",
    # ... etc for every real workload
]
blocklist = ["target_process_name"]   # names → Kill regardless of category
sigterm_grace_secs = 5            # SIGTERM→SIGKILL delay; default 5, min 1
rate_limit_max_kills = 3          # default 3
rate_limit_window_secs = 60       # default 60

[thresholds]
vram_critical_pct = 95.0          # per-PID VRAM% cutoff; default 95
ram_critical_pct = 95.0           # per-PID RAM% cutoff; default 95
thermal_red_c = 95.0              # HOST-WIDE thermal cutoff; default 95
```

**DISARM**: **config is NOT hot-reloaded** (verified: no inotify/SIGHUP-reload path in `config.rs`/`main.rs`). To disarm:
- Edit `edge_monitor.toml` → set `[governor] auto_actuate = false` → **RESTART edge_monitor** (SIGTERM the process, then relaunch).
- OR just SIGTERM `edge_monitor` (`kill $(pgrep edge_monitor)`) — immediate hard-stop of the governor.

Note: web `/api/settings` POST **cannot** flip `auto_actuate` — it's schema-firewalled out (`policy.rs`, `config.rs:GovernorConfig`, and the D86 SettingsPanel boundary at `web/src/components/SettingsPanel.svelte:5-18`). The wire cannot arm/disarm the killer.

### Q6 — Applied to this operator's specific host: **UNSAFE without protections**

**Current host state (from live `/api/snapshot`):**
- 22 workloads: 1 `agent` (claude) + 19 `ros2` + potentially 2 `llm` (only if the fake-ollama smoke is still running; likely dead by now)
- All classify as `AICategory::Inference` on the wire
- **CPU Package thermal: 90-93°C** (I saw 93°C repeatedly this session; the alert `Thermal at 93.0°C — system thermal pressure` was firing)
- **`thermal_red_c` default: 95.0°C — a 2-4°C temperature rise crosses this threshold**

**If operator arms with `auto_actuate=true` + `default_ai_action=Kill` + defaults elsewhere:**

Scenario A — the intended path (disposable 2.5GB target on 12GB card = 20.8% VRAM):
- 20.8% < 95.0% `vram_critical_pct` → **the disposable does NOT breach VRAM at default thresholds.** The operator MUST lower `vram_critical_pct` (e.g. to 15) to make it fire.
- Once threshold is 15%: the disposable target breaches. Real workloads with 0 or unmeasured VRAM don't. If a bystander LLM (say a claude subprocess) transiently uses >15% of 12GB (~1.8GB) — it becomes a co-candidate.

Scenario B — thermal spike (the FIRE risk):
- **Host is already at 93°C.** Any load-inducing action (test workload, browser opening, background compile) that pushes past 95°C fires `host_thermal_breached = true`.
- On that tick, **EVERY AI-classified PID** (all 20+ ROS2 nodes + claude, minus the whitelist which is only shell/init) satisfies breach gate (3).
- If `default_ai_action = Kill`, every one becomes `SignalTermSent`.
- Rate-limited to 3 kills / 60s, **ordered ascending PID.** In practice this means the lowest-PID robot nodes die first — often the ROS2 daemon or the earliest-launched control node.
- 3 killed ROS2 nodes = broken robot stack.

**The operator's plan assumption "the governor kills the highest VRAM consumer" is FALSE.** The governor kills EVERY qualifying PID; the sort is lowest-PID-first for rate-limit ties; and thermal is a system-wide grenade that catches everyone.

### Recommended safe-test config (operator to apply BEFORE arming)

```toml
[governor]
auto_actuate = true
kill_sustain_secs = 30              # LONG — extra reaction window before actuation

[policy]
default_ai_action = "Allow"         # <<<< KEEP DEFAULT ALLOW.
# Force target via blocklist instead — belt AND braces.
blocklist = ["<disposable_process_name>"]
# Explicit safety net if default_ai_action is later flipped:
allowlist = [
    "claude", "ros2", "robot_state_pub", "parameter_bridg",
    "range_converter", "async_slam_tool", "ekf_node",
    "controller_server", "smoother_server", "planner_server",
    "behavior_server", "bt_navigator", "waypoint_follow",
    "velocity_smooth", "lifecycle_manag", "docker",
]
sigterm_grace_secs = 5              # default
rate_limit_max_kills = 1            # <<<< LOWERED to 1/window — one shot only
rate_limit_window_secs = 60

[thresholds]
vram_critical_pct = 15.0            # tuned to catch a 2.5GB target on 12GB card (~20.8%)
ram_critical_pct = 95.0             # default; no real workload approaches this
thermal_red_c = 120.0               # <<<< RAISED to prevent thermal-triggered mass-kill.
                                    # Host is at 93°C; default 95°C is unsafe.
```

**Post-test disarm sequence:**
1. `kill $(pgrep -f "target/release/edge_monitor")` — immediate stop.
2. Edit `edge_monitor.toml` → set `[governor] auto_actuate = false` (and revert `thermal_red_c = 95.0` if raised).
3. Restart edge_monitor (no auto-reload).

### Belt-and-braces additional check (recommended before arming)

Before arming, run this one-liner to confirm the whitelist would actually match every live workload name:
```
curl -s http://127.0.0.1:7070/api/snapshot \
  | python3 -c "import json,sys; d=json.load(sys.stdin); wl=['claude','ros2','robot_state_pub','parameter_bridg','range_converter','async_slam_tool','ekf_node','controller_server','smoother_server','planner_server','behavior_server','bt_navigator','waypoint_follow','velocity_smooth','lifecycle_manag','docker']; unmatched=[w for w in d['workloads'] if w['name'] not in wl]; print('UNPROTECTED:' if unmatched else 'ALL COVERED'); [print(f'  {w[\"pid\"]} {w[\"name\"]}') for w in unmatched]"
```
If any workload prints under `UNPROTECTED:`, add its exact name to the allowlist before arming.

**HARD STOP #1 stays intact throughout this dispatch** — no governor code touched, no arming, no config change. This is READING the governor, which is permitted.

---

## [COMPLETION SUMMARY — Autonomous Completion + Hardening run] 2026-07-16

The **completion+hardening** run finished. All autonomously-completable
work is landed; the branch is green across all three gates and ready
for the operator's next milestone check-in.

### What shipped this run (7 commits on `l14-top-processes-sort`)

| Commit | Phase | What |
| --- | --- | --- |
| `344b184` | 1.1-1.3 | Phase 1: TUI-essentials FINDING (D107 already closed it) + CHANGELOG catch-up (D107/D108/D109) + post-hoc `docs/GPU_TILE_DESIGN.md` design record. |
| `b676b1c` | 2.5 | Phase 2: full-project audit sweep → `docs/state/AUDIT.md`. 0 blockers; 11 SHOULD-FIX (fixed below); 7 DEFERRED (all human/hardware/CAR-blocked). |
| `b7dc630` | 3.1 | Phase 3.1: 5 doc-drift fixes surfaced by AUDIT §§4.3-4.5 — PHASE5_HISTORY / PHASE4_AUTOKILL / PHASE4_DESIGN status headers + BOARD_AUDIT §2.6 numeric drifts + PENDING STOP #3 stale text. |
| `7057844` | 3.2A | Phase 3.2 landing A: 4 tests pinning `column_header_line` (D107 FIX 2) + `LABEL_WIDTH` (D107 FIX 4). Bonus: `render_thermal_summary` no longer hard-codes 12 — uses the module `LABEL_WIDTH` const. |
| `968ccc8` | 3.2B | Phase 3.2 landing B: 12 tests pinning ollama runner friendly-name preference (D107 FIX 3) + runtime `promote_sha_blob_hints` promotion (D107 FIX 3) + D109 TUI GPU row honesty + aggregation. Two extractions (`promote_sha_blob_hints`, `format_gpu_vitals_line`) make the load-bearing invariants directly testable. |
| `f91742a` | 3.3 | Phase 3 re-sweep lint fix — clippy caught a `collapsible_if` in the new helper (converted to let-chain, rustc 1.95) and a `doc_lazy_continuation` in a doc-comment (reflowed to prose). Behavior identical. |
| (this) | EXIT | Completion summary + BOARD HEAD update. |

### Gate state at EXIT

- `cargo test --workspace` — **1200 passed / 0 failed** (was 1184 at
  Phase-2 audit baseline; +16 tests, all coverage additions).
- `cargo clippy --workspace --all-targets -- -D warnings` — **clean**.
- `npm --prefix web run test:browser` — **223 passed / 0 failed**
  (unchanged — no web-facing changes this run).
- All 11 named invariant tripwires green (verified individually
  in Phase 2 and left green through Phase 3 by construction —
  the Phase-3 landings are docs / tests / behavior-neutral extractions).

### AUDIT categories, resolved

- **BLOCKER — 0**: unchanged; nothing was ever red.
- **SHOULD-FIX — 11**: all closed in Phase 3.
  - 5 doc-drift findings → `b7dc630`.
  - 2 D107 FIX 2/4 coverage → `7057844`.
  - 4 D107 FIX 3 + D109 coverage → `968ccc8` (2 ollama tests + 5 runtime
    promotion tests + 5 GPU-line tests).
- **DEFERRED — 7**: all still deferred; none are autonomously fixable:
  1. Versioning tag (v2.0.0 vs v1.4.x) — HUMAN DECISION.
  2. observer→supervisor decision — HUMAN DECISION.
  3. Auto-kill tiebreaker — HARD STOP #1 (governor).
  4. `KILL_ARM_WINDOW_SECS` const removal — HARD STOP #2 (CAR).
  5. Unmeasured VRAM/GPU live-verification — needs driver reload
     (hardware). Still pinned by wire-honesty tests + D98 gate
     `data-testid-unmeasured` assertions on F1/F2/F3.
  6. Follow-on TUI candidates — HARD STOP #3 (each needs its own
     ratification): hardware identity, AlertState-on-wire, classifier
     consistency, top-processes on web, activity content parity.
  7. `WireAlertEntry.timestamp` — potential future CAR.

### Two honest disclosures

1. **Unmeasured VRAM/GPU path is NOT live-verified this session.**
   The NVML driver is loaded on the dev host so every smoke shows
   the measured branch. Test layers pin the unmeasured branch — the
   three wire honesty tests + D98 gate assertions on `data-testid-
   unmeasured` — but a real-data live-verification awaits a driver
   reload. AUDIT.md §3.4 states exactly this.
2. **Origin sync verification ceiling.** `git fetch` failed with
   "could not read Username for 'https://github.com'" — the audit
   shell has no cached credentials. Local `origin/l14-top-processes-
   sort` shows `729bdf7` (pre-D109). Operator confirmed the D109 push
   happened; the Phase-1/2/3 work sitting on top (`344b184` through
   `f91742a`) is unpushed and needs to be pushed manually. AUDIT.md
   §4.2 records the ceiling.

### What the operator sees on next open

- 7 unpushed commits on `l14-top-processes-sort` (D109 pushed;
  everything after it is local).
- BOARD.md shows "no open items — everything remaining is
  human-blocked or hardware-blocked."
- No hot HARD STOPs. STOP #3 remains marked RESOLVED with a full
  ship-record + design doc pointer. This EXIT block is above it.
- Test count 1184 → 1200; gate count unchanged at 223.

### What's safe to work on next (if the operator opens another loop)

Everything remaining is either human-decision or hardware-blocked
(see DEFERRED list above). Any of the follow-on TUI candidates would
be HARD STOP #3 — the loop would immediately propose options and
stop for ratification.

---


*When you (the agent) hit a HARD STOP, write it here LOUDLY and stop. The human reads this at milestone check-ins. Clear an item when it's resolved (move the resolution to JOURNAL.md).*

*Format:*
```
## [STOP #N] <title> — <date>
**What I was doing:** ...
**Why I stopped:** (which HARD STOP rule)
**What I need from you:** (a decision / a CAR / a governor review / driver reload / etc.)
**My recommendation (if any):** ...
**What's safe to do meanwhile:** (other work I can proceed with, or "nothing — blocked")
```

---

## [FINDING] Connectivity indicator — "derive endpoint ourselves" is FEASIBLE for exactly the workload types that have HTTP endpoints; recommend hybrid — 2026-07-16

**What I was asked to do:** determine whether we can derive per-workload health-probe endpoints from what the classifier + samplers already know, for each detected workload type, and recommend an approach for the connectivity indicator build.

**Short answer:** **YES for ollama / vLLM / llama.cpp** — the derivation code ALREADY EXISTS as `discover_port()` + `endpoint_for()` helpers on the corresponding samplers. **NO endpoint exists for embeddings / agent / ROS2** — those are structurally non-HTTP and should be EXCLUDED from the probe (rendering nothing, not "DOWN"). The "derive ourselves" path is not fragile — it's a reuse of shipped, tested code — for the ~3 workload types where an HTTP endpoint is even a coherent concept. **Recommend: (a) derive-only for those 3 types, (b) show N/A (no chip) for the others.** No config knob needed. No CAR needed.

### Q1 — Per workload type, CAN we know the endpoint?

Verified against the shipped sampler code:

| Workload type | Has HTTP endpoint? | Derivation available? | Cite | Verdict |
|---|---|---|---|---|
| **ollama** | ✅ yes, `http://127.0.0.1:{port}/api/ps` | ✅ [`OllamaSource::endpoint_for(cmdline, environ)`](../../src/telemetry/samplers/ollama_api.rs#L169-L174) — honors `OLLAMA_HOST` env var + `--host` cmdline flag; default 11434 | [`ollama_api.rs:145-174`](../../src/telemetry/samplers/ollama_api.rs#L145-L174) | **derive** |
| **vLLM** | ✅ yes, `http://127.0.0.1:{port}/metrics` | ✅ [`VllmPrometheusSource::endpoint_for(cmdline)`](../../src/telemetry/samplers/vllm_prometheus.rs#L80-L82) — parses `--port` / `--port=`; default 8000 | [`vllm_prometheus.rs:58-82`](../../src/telemetry/samplers/vllm_prometheus.rs#L58-L82) | **derive** |
| **llama.cpp** (`llama-server`) | ✅ yes, `http://127.0.0.1:{port}/metrics` | ✅ [`LlamaCppServerSource::endpoint_for(cmdline)`](../../src/telemetry/samplers/llama_cpp_server.rs#L78-L80) — same `--port` parser as vLLM; default 8080 | [`llama_cpp_server.rs:61-80`](../../src/telemetry/samplers/llama_cpp_server.rs#L61-L80) | **derive** |
| **embeddings** (sentence-transformers, BGE, GTE, E5, nomic, MiniLM, jina) | ❌ no HTTP endpoint | n/a — embeddings sampler is CPU-signal-only per [`embeddings_cpu.rs:1-8`](../../src/telemetry/samplers/embeddings_cpu.rs#L1-L8): *"Embeddings workloads don't expose a Prometheus endpoint and don't have a daemon-style API to poll."* | [`embeddings_cpu.rs:1-8`](../../src/telemetry/samplers/embeddings_cpu.rs#L1-L8) | **exclude** — no chip |
| **agent** (claude, cursor, aider, continue) | ❌ no HTTP endpoint | n/a — these are CLI processes that TALK to a remote LLM; they have no local server to probe | [`agent_claude.rs`](../../src/telemetry/samplers/agent_claude.rs) — sampler detects via ppid + bash-child observation, not by scraping the agent | **exclude** — no chip |
| **ROS2** | ❌ no HTTP endpoint | n/a — ROS2 uses DDS (multicast pub/sub), sampler shells out to `ros2 topic echo --once` per [`ros2_shellout.rs:1-25`](../../src/telemetry/samplers/ros2_shellout.rs#L1-L25). There is nothing to `GET` | [`ros2_shellout.rs:1-25`](../../src/telemetry/samplers/ros2_shellout.rs#L1-L25) | **exclude** — no chip |
| **Vision** (whisper-server, ComfyUI, YOLO, stable-diffusion) | ⚠️ mixed | whisper-server / ComfyUI DO expose HTTP but no `discover_port()` shipped; YOLO / SD are Python scripts (usually no server) | — | **exclude for v1** (add per-server derivation in a later dispatch if operator asks) |
| **Triton / TorchServe** | ⚠️ HTTP but complex (multi-endpoint) | no shipped derivation | — | **exclude for v1** |
| **Training** (torchrun, deepspeed, accelerate) | ❌ no HTTP endpoint | n/a — batch jobs | — | **exclude — no chip** |

**Score**: 3 workload types have shipped derivation + defined endpoints (ollama, vLLM, llama.cpp). Every other workload type is either structurally non-HTTP (embeddings, agent, ROS2, training) or has HTTP but needs a fresh derivation function per server (Vision variants, Triton) — **defer those**.

### Q2 — What does the classifier already capture?

`ProcessSample` at [`src/model.rs:8-38`](../../src/model.rs#L8-L38) carries **`cmdline: Vec<String>`** AND **`environ: HashMap<String, String>`** — every field the three `discover_port()` functions need.

BUT — and this is the ONE gap worth flagging — **`AnnotatedProcess` (the wire-side per-tick shape) does NOT carry cmdline/environ.** [`src/runtime.rs:49-82`](../../src/runtime.rs#L49-L82) drops them. So the derivation cannot happen at wire-build time from `AnnotatedProcess` alone; it needs to happen either (a) at classification time and be stored, or (b) re-read from `/proc/<pid>/cmdline` at probe time. The current samplers use (a) via a per-PID `endpoint_cache: HashMap<u32, Option<String>>` (see [`vllm_prometheus.rs:40`](../../src/telemetry/samplers/vllm_prometheus.rs#L40)) — cache the endpoint on first classification, reuse.

**Fix shape (for the eventual build):** add `probe_endpoint: Option<String>` to `AnnotatedProcess`, populated at classification/annotation time via `endpoint_for()` for the 3 supported types, `None` for everything else. Wire it through to `WireWorkload`. Frontend renders a chip only when `probe_endpoint.is_some()`.

Estimated cost: ~40 LoC on the runtime side (add field + wire it via a `pub fn endpoint_for_workload(sample) -> Option<String>` dispatcher in `src/telemetry/` that matches on `WorkloadCategory` + name and calls into the existing samplers), ~30 LoC on the wire side (add field + serde), ~20 LoC of TS mirror, ~100 LoC for the frontend chip component + probe loop.

### Q3 — Recommended approach: **DERIVE ONLY (option a)**, no config

Ranked options from the dispatch, honestly assessed:

- **(a) Derive where cleanly possible; exclude non-HTTP types.** ✅ **RECOMMENDED.** Reuses already-shipped, already-tested `discover_port()` / `endpoint_for()` helpers. Zero fragility for the 3 supported types (the samplers themselves rely on this derivation working — if it were fragile, tokens/sec scraping would already be broken). Non-HTTP types render no chip — honest.
- **(b) Hybrid derive + config override.** REJECTED unless operator specifically asks for it. YAGNI — the derivation already handles `OLLAMA_HOST`, `--port`, `--host` cmdline forms. The only case config would help is *"I ran ollama behind a reverse proxy on a weird port"* — an edge case not worth the config surface. If it appears, revisit; don't build it now.
- **(c) Hardcode ollama + config override for the rest.** REJECTED. The dispatch flagged this as the fallback "if derivation isn't cleanly possible" — but derivation IS cleanly possible for the 3 types we care about. Hardcoding when we have a working parser would be regression, not simplification.

### Q4 — Probe mechanics (design notes for the eventual build)

- **Interval**: probe every **5 seconds**, NOT every 1 Hz tick. Rationale: an HTTP GET to a stalled ollama can block 500 ms (the samplers set 500 ms timeouts — see [`vllm_prometheus.rs:33`](../../src/telemetry/samplers/vllm_prometheus.rs#L33)). Multiply by N workloads at 1 Hz and you're melting the tick loop. 5 s is empirically fine for a "backend reachable" signal (a 5-second dead-server delay before UI update is acceptable — this is a monitor, not a load balancer).
- **Startup state**: `"checking..."` for the first probe, THEN either `"ok"` or `"unreachable"`. **NEVER show "DOWN" before the first probe completes.** Match the daemon-status pattern the ollama sampler already uses at [`ollama_api.rs:176-194`](../../src/telemetry/samplers/ollama_api.rs#L176-L194) — log-once on transition, don't spam.
- **Debounce**: two consecutive failures before flipping to `unreachable` (matches the sampler's "poison after 2 failures" pattern at [`vllm_prometheus.rs:39-40`](../../src/telemetry/samplers/vllm_prometheus.rs#L39-L40)). Prevents a single dropped packet from flashing the chip red.
- **Timeout**: 500 ms per probe (match the sampler timeouts).
- **Cache key**: per PID. Reuses the sampler cache pattern.
- **Where the probe LOOP runs**: NOT the tick loop. Spawn a dedicated async task at `Runtime::new` (or reuse the existing telemetry-dispatcher's task pool) — the 5 s cadence + probe I/O are async-native and belong on tokio, not on the sync tick loop.
- **Reachability state on the wire**: add `probe_status: Option<"ok" | "checking" | "unreachable">` to `WireWorkload` alongside `probe_endpoint`. Frontend renders a small color chip next to the workload row (green / neutral / red). `None` for excluded types = no chip.

### Verdict — is "derive ourselves" achievable?

**Yes, for the 3 HTTP workload types operators actually deploy in this project's target scope.** Not because we invent complex derivation, but because the *samplers already do this correctly and have tests to prove it* — the connectivity chip reuses their `endpoint_for()` output. For the other types (embeddings, agent, ROS2, training) — no chip at all is the honest answer. Showing "DOWN" for a ROS2 node that publishes at 10 Hz would be a lie in exactly the shape the CLAUDE.md VRAM-honesty rule forbids: *"NEVER a 0 or 0-line ... reads as 'GPU idle'"* — same principle, same restraint.

**Contract impact**: adding `probe_endpoint: Option<String>` + `probe_status: Option<String>` to `WireWorkload` — the wire type lives ENTIRELY in `src/web/wire.rs` (`WireWorkload` at [`wire.rs:476-514`](../../src/web/wire.rs#L476-L514) is NOT in `../ux_contract`, same as `WireGpu` per the GPU-tile design record). **No CAR needed** — additive consumer-side change, mirrors D109's precedent.

**HARD STOP status**: this is a design decision (Q3 asks operator to pick derive / hybrid / hardcode). The recommendation is clear and low-risk, but the CHOICE belongs to the human per HARD STOP #3. Also flags a decision on the 5-second probe cadence and the exclusion list (embeddings / agent / ROS2 / training / Vision / Triton).

**What I need from the operator to build it:**
1. Ratify **(a) derive-only** (my lean) vs **(b) hybrid** vs **(c) hardcode+config** for the endpoint discovery approach.
2. Ratify **5-second probe cadence + 500 ms timeout + 2-failure debounce** (matches shipped sampler patterns).
3. Ratify **exclusion list**: no chip for embeddings / agent / ROS2 / training / Vision-variants / Triton. Only ollama / vLLM / llama.cpp get chips in v1.
4. Ratify the wire additions (`probe_endpoint`, `probe_status`) — additive to `WireWorkload`, no CAR.

**What's safe to do meanwhile:** nothing on this specific arc without operator ratification (HARD STOP #3). Other work: the AUDIT DEFERRED items in the completion summary above are still human/hardware-blocked, and the loop is at EXIT until operator opens the next milestone.

---

## [FINDING] "TUI essentials-only" is ALREADY DONE as originally scoped — 2026-07-15

**What I was asked to do:** Phase 1's plan called for an investigator-pass on the "TUI essentials-only rework" (the last unstarted Phase-5 item per BOARD.md), then propose a design.

**What I found:** BOARD_AUDIT §3 (the source-of-truth ratified scope for the phrase) enumerates the "TUI-essentials rework" as EXACTLY four defects, all of which shipped in DISPATCH 107:

| BOARD_AUDIT §3 item | D107 FIX | Verifiable at |
| --- | --- | --- |
| Duplicate "AI Workloads" panel (unconditional at 5+ workloads) | FIX 1 | `src/ui/panels/mod.rs:249` — `render_workloads_two_col` fn removed, comment explains the change |
| No column headers on AI Workloads rows | FIX 2 | `src/ui/panels/workloads.rs:98,538` — new `column_header_line()` fn + call site |
| `sha256-…` digest leaking into workload NAME field | FIX 3 | `src/telemetry/samplers/ollama_api.rs` + `src/runtime.rs` — hint prefers friendly name, runtime promotes onto AnnotatedProcess.model_name |
| Vitals no aligned column grid / stranded RAM | FIX 4 | `src/ui/panels/vitals.rs` — LABEL_WIDTH=12 grid across every row |

**BOARD.md is stale on this point.** It says the phrase is "unstarted" but the phrase-as-defined shipped 2 dispatches ago. The BOARD update is a small doc landing I'll take as part of Phase 1 (not a HARD STOP).

**No design proposal needed for TUI-essentials-as-defined.** The phrase's originally-ratified scope is closed. Writing a proposal would be scope-invention — inspector's HARD STOP #3 discipline says "if no doc settles it, propose OPTIONS not decide" — but here the doc DOES settle it (BOARD_AUDIT §3), and it says done.

**If you WANT more TUI work — the candidate follow-ons that AREN'T shipped:**
These would each need their own scope decision (each is HARD STOP #3 if you want me to build any of them — I'd write a proposal per item). Enumerated for your reference; NOT proposing to build without ratification:

- **Hardware identity (`HostInfo`)** — BOARD_AUDIT §2.1 marks this as NEW / v1.4.x. Show GPU name (NVML `nvmlDeviceGetName`), CPU name (`/proc/cpuinfo`, no shellout), RAM identity. Open question: RAM = capacity label (free, procfs) vs DIMM part/speed (needs root `dmidecode` — footgun on an unprivileged tool). Open question: TUI-only vs wire to web.
- **AlertState raise/ack events into RuntimeState** — BOARD_AUDIT §3 surfacing gap V7. Signal exists internally; not accumulated onto the wire/UI.
- **Classifier consistency** — BOARD_AUDIT §2.2: "same binary (`claude`) lands in both Agent and Unknown; `bash` shows as a workload. Partial." Not TUI-cosmetic; classifier-logic scope.
- **Top Processes card on web** — BOARD_AUDIT §2.3 / §2.6: exists on TUI, missing from web. Web-parity gap.
- **Activity content parity (TUI vs web)** — BOARD_AUDIT §3 "Tester gate to confirm" — needs a diff pass to enumerate.

None of these are "TUI-essentials-only" per the ratified phrase. All are follow-on scope. Your call which (if any) to open.

**Autonomous action I took:** none for this item beyond writing this finding. BOARD update lands in the next commit. No landing 1.x needed.

---

## [STOP #3 — RESOLVED 2026-07-15] GPU temp/power tile — design ratified + SHIPPED

Operator confirmed inspector lean **1c / 2a / 3a**: VitalsPanel + KioskView
(skip Strip); one combined kiosk tile `62°C · 45W`; MAX temp / SUM watts
across devices. Backend + wire honesty landed in commit `814c1b3` (landing 3).
Web consumers landed in `e4772d3` (landing 4). Post-hoc design record at
[`docs/GPU_TILE_DESIGN.md`](../GPU_TILE_DESIGN.md). Resolution recorded in
JOURNAL.md.

---

## [STOP #3] GPU temp/power tile — design ratification needed — 2026-07-15

**What I was doing:** Landing 2 of this run — the BOARD-flagged "GPU temp/power tile (read + Prometheus exist, not surfaced), low-risk, buildable" item.

**Why I stopped:** No design doc exists for this feature. Investigation surfaced real design choices with materially different tradeoffs (placement scope, kiosk tile shape, aggregation). HARD STOP #3 fires — I propose, you decide.

**Signal availability — confirmed live:**
- Temp: NVML `device.temperature(TemperatureSensor::Gpu)` → `GpuDeviceMetrics.temp_c: Option<f32>` (degrees C) at [`src/platform/gpu_nvidia.rs:224-227`](../../src/platform/gpu_nvidia.rs#L224-L227).
- Power: NVML `device.power_usage()` (milliwatts) → `GpuDeviceMetrics.power_watts: Option<f32>` (watts) at [`src/platform/gpu_nvidia.rs:220-223`](../../src/platform/gpu_nvidia.rs#L220-L223).
- Prometheus surface exists: `edge_monitor_gpu_watts{pid=...}` and `edge_monitor_gpu_temp_celsius` at [`src/telemetry/exporter.rs:191-207`](../../src/telemetry/exporter.rs#L191-L207).
- **NOT on the TUI** ([`src/ui/panels/vitals.rs`](../../src/ui/panels/vitals.rs) reads `snap.gpu` for VRAM gauge only).
- **NOT on the web wire** — [`WireGpu`](../../src/web/wire.rs#L466-L472) has only `vram_pct` / `vram_used_mb` / `vram_total_mb` / `device_count`.

**Wire-type gap analysis (HARD STOP #2 test):** `WireGpu` is defined ENTIRELY in `src/web/wire.rs`, NOT in `../ux_contract`. Adding `temp_c: Option<f32>` + `power_w: Option<f32>` fields is a pure consumer-side additive change — **NO CAR needed** (HARD STOP #2 does NOT fire). Web `types.ts:145` mirror updates in lockstep.

**Design questions — needing your call:**

1. **Placement scope (which surfaces):**
   - **(a)** VitalsPanel + VitalsStrip + KioskView — everywhere. Most consistent, most work.
   - **(b)** VitalsPanel only (dashboard) — minimum, where the operator sits.
   - **(c) *Inspector lean:*** VitalsPanel + KioskView. Kiosk wall-monitor deserves it; VitalsStrip stays tight per D103's "chronology-first" intent.

2. **Kiosk tile shape (if included):**
   - **(a) *Inspector lean:*** One "GPU" tile showing `62°C · 45W` — one tile, two numbers, same signal source belong together.
   - **(b)** Two separate tiles "GPU TEMP" and "GPU POWER" — more granular, uses more space.
   - **(c)** Extend the existing "THERMAL" tile — mixes system/GPU thermals, blurs the signal boundary.

3. **Aggregation across devices:**
   - **(a) *Inspector lean:*** Max temp / sum watts across all `GpuDeviceMetrics` devices. Honest for 99% single-GPU hosts; sensible for multi-GPU.
   - **(b)** Primary device only — loses info on multi-GPU.
   - **(c)** Per-device rendering — more info, more UI space.

4. **Unmeasured handling — no choice, VRAM honesty rule applies:** NVML returns `None` for temp/power when Unsupported. Render as "—" with `data-testid-unmeasured="true"`, NEVER "0°C" or "0W". Same D95/D102 pattern that governs VRAM.

**My recommendation (all three "*Inspector lean*" defaults):**
- Scope: VitalsPanel (TUI + web dashboard) + KioskView. Skip VitalsStrip.
- Kiosk shape: one combined "GPU" tile — `62°C · 45W`. Grows kiosk from 3 to 4 big tiles.
- Aggregation: max temp / sum watts across devices.
- Unmeasured: "—" everywhere, honest.

**Build sequence if ratified (5 landings, ~2 hours):**
1. Wire additions to `WireGpu` — `temp_c: Option<f32>` + `power_w: Option<f32>`. Mirror `web/src/lib/types.ts`. Serialization site at `wire.rs:863`. Rust test pinning Some→field-present / None→field-absent (VRAM honesty on the wire).
2. TUI 6th row in `vitals.rs` — `GPU         62°C · 45W` on the 12-char label grid; unmeasured branch shows `—`.
3. Web `VitalsPanel.svelte` — extend GPU section with temp + watts + unmeasured branch.
4. Web `KioskView.svelte` — 4th tile with combined display + `data-testid-unmeasured` + D98 gate extension.
5. D98 matrix cells that assert kiosk tile count update from 3 to 4. New `F8_gpu_unmeasured.json` fixture pins the honesty discriminator at the wire boundary.

**What I need from you:** ratify (or redirect) the 3 design questions. A one-line "1c / 2a / 3a" (my lean) or your alternative gets me building landing 3.

**What's safe to do meanwhile:** the loop's other autonomously-completable work is thin — TUI essentials-only ALSO needs HARD STOP #3, and everything else in BOARD is human-blocked. If you don't want to ratify right now, I hit the EXIT condition — write a completion summary here and wait. Ratify at your leisure and I resume.

---

### Reference — the HARD STOP rules (from CLAUDE.md)
1. Governor / kill / actuation path touched — surface, never auto-proceed
2. A contract change (`../ux_contract`, new wire type, new endpoint) is needed — write a CAR, stop
3. An unratified design/UX decision (materially different approaches, no doc settles it) — propose options, don't decide
4. A destructive/irreversible action permissions didn't catch
5. About to arm the killer / enable auto_actuate / make a kill fire — never, surface
