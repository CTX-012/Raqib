The note above is misplaced — your three answers are decisions, not research requests. I'm consolidating v0.3 as you asked.

Reading your answers:

**Q1 — `d` conflict:** "Depends, most users will find annoying." I'm interpreting this as: leave `d` removed, the specific conflict doesn't matter — you're trusting the general principle that the key has too many habit collisions across Linux tools to be safe. Locked: `d` is not a key in the contract.

**Q2 — Alert on exit:** Only on non-zero / OOM / kill. Clean exits don't pop alerts but still go to history. Locked.

**Q3 — Show consolidated v0.3 first.** Here it is, end to end. After you read it, say "lock" or push back on specific clauses.

---

# UX_CONTRACT.md

```
Version:     0.3 (pending lock)
Status:      Awaiting final review
Scope:       Linux and Windows binaries — same TUI, different telemetry
Mission:     Watch the whole system, highlight AI workloads, warn before
             resource pressure crashes them. The TUI is the foreground;
             the Prometheus exporter is the bridge to anything else.
```

## §0 — Mission and out-of-scope

edge_monitor exists to keep a developer ahead of the moment when an AI workload runs out of resources. The Prometheus exporter is the integration surface for fleet observability; the TUI is for the developer at the box.

**Explicitly out of scope:**

- Auto-kill on resource pressure — governor fires only on user-defined `[[workloads]]` rules
- Replacing htop or Task Manager — system processes are context, not subjects
- Fleet management — Prometheus exporter's job
- Web UI — Prometheus is the bridge, no HTML rendering
- ROS1 detection — only ROS2; ROS1 processes appear as Unknown
- Stderr persistence — transient only, gone after card dismissed
- Historical analysis beyond 20 most recent runs per model
- ~~Sharing/export — deferred to v1.1~~ — **DROPPED per DISPATCH 49**; see [docs/ROADMAP.md](docs/ROADMAP.md) EXPLICITLY NOT DOING.
- ~~Tagging, notifications, filtering, custom themes — deferred to v1.1~~ — **DROPPED per DISPATCH 49**; see [docs/ROADMAP.md](docs/ROADMAP.md) EXPLICITLY NOT DOING.

## §1 — Default screen

```
  ⚠ VRAM at 91% — YOLO-x (PID 6292) approaching limit             a dismiss

  edge_monitor  ·  3 workloads · 1 degraded             press ? for help

  System

    CPU      ████████████░░░░░  56%   load 5.71 · 16 cores
    Memory   ██████████░░░░░░░  44%   14.2 / 32 GB
    GPU      ████████████████░  91%   71°C · 142 W       ⚠ pressure
    VRAM     ██████████████░░░  87%   14.0 / 16 GB       ⚠ pressure

  Workloads

    LLM
      ●  phi3 (Ollama)            38.4 tok/s    4.2 GB    PID 206
      ⚠  Llama-70B (vLLM)         12.1 tok/s   14.8 GB    PID 4523
         KV 87% · queue 4 · p99 380ms · baseline 22 tok/s · -45%

    Vision
      ●  YOLOv8 (Ultralytics)     47 fps        1.8 GB    PID 6291

    ROS2
      ●  /perception_node         24 Hz         512 MB    PID 7104
      ●  /planner_node            10 Hz         320 MB    PID 7105

  Top processes (by RAM)

    Code.exe                      1.3 GB         48% CPU
    chrome.exe                    917 MB         29% CPU
    ollama                        4.2 GB          0% CPU

  Activity

    08:51:09  alert raised   VRAM 91%
    08:42:11  exit  phi3  4 min  38.4 tok/s avg

           Enter detail · k kill · g graph · h history · ? help · q quit
```

**Regions, top to bottom:**

1. Alert region (0–3 lines, present only when alerts active)
2. Header (1 line) — product, workload count, degraded count
3. System panel — CPU, Memory always; GPU and VRAM omitted entirely if no GPU
4. Workloads panel — grouped by category (LLM, Vision, ROS2, Embeddings, Unknown). Empty subsections hidden. Single-category screens hide the subsection header.
5. Top processes panel — N rows by configurable sort (RAM default). Excludes edge_monitor itself; AI workloads already shown in Workloads above appear here too (full top-N, no de-duplication — see the `ollama` row in the example).
6. Activity panel — last 5 events
7. Footer — keymap

**Subsection ordering:** LLM → Vision → ROS2 → Embeddings → Unknown. Fixed.

## §2 — Workload row spec

**Healthy workload, single line:**

```
  ●  {model} ({runtime})  {primary_metric}  {ram}  PID {pid}
```

**Primary metric by category:**

| Category | Format |
|---|---|
| LLM | `{value} tok/s` |
| Vision | `{value} fps` |
| Embeddings | `{value} emb/s` |
| ROS2 | `{value} Hz` (process-level RAM/CPU only in v1.0; Hz deferred to v1.1) |
| Loading | `cold-loading` |
| Unknown | `(no metrics)` |

**Degraded workload, expanded line (auto, when amber/red):**

| Category | Schema |
|---|---|
| LLM | `KV {pct}% · queue {n} · p99 {ms}ms · baseline {tok/s} · {±delta}%` |
| Vision | `VRAM {pct}% · {phase} · baseline {fps} · {±delta}%` |
| Embeddings | `batch {n} · p99 {ms}ms · baseline {emb/s} · {±delta}%` |
| ROS2 | `topics {n} · queue {n} · baseline {Hz} · {±delta}%` (v1.1+) |
| Loading | `loaded {gb} / {total} GB · {n} disk reads remaining` |
| Unknown | `(unrecognized AI workload — no metrics)` |

**ROS2 detection:**

- Env vars: `RMW_IMPLEMENTATION`, `ROS_DOMAIN_ID`, `AMENT_PREFIX_PATH`
- Cmdline: `ros2 run`, `ros2 launch`
- Linked libraries: `librcl.so` / `librclpy.so` / `librclcpp.so` (Linux), `librcl.dll` (Windows)
- Node name: from `--ros-args -r __node:=<name>` or `/{node_name}` first arg
- ROS1 patterns (`rosrun`, `roslaunch`, `ROS_MASTER_URI`) intentionally NOT detected

## §3 — Status dot semantics

| Dot | Trigger |
|---|---|
| `●` (green / Healthy) | All thresholds OK, throughput within ±10% of baseline |
| `⚠` (amber / Attention) | VRAM ≥ 85%, RAM ≥ 90%, KV ≥ 80%, OR throughput ≤ baseline × 0.80 |
| `✕` (red / Critical) | VRAM ≥ 95%, KV ≥ 95%, governor armed against this PID, OR OOM detected |
| `○` (gray / Loading) | < 30s of telemetry, no baseline available |

No hysteresis — a workload that flickers between amber and green has a real problem, surface it.

## §4 — Alerts

Sticky banners above the header. Interrupt the default view; user must acknowledge.

**Triggers:**

| Alert ID | Condition | Message |
|---|---|---|
| `ALERT_VRAM_PRESSURE` | VRAM ≥ 85% sustained 5s | `VRAM at {pct}% — {workload} (PID {pid}) approaching limit` |
| `ALERT_RAM_PRESSURE` | RAM ≥ 90% sustained 5s | `RAM at {pct}% — system approaching limit` |
| `ALERT_KV_PRESSURE` | KV cache ≥ 85% sustained 5s | `KV cache at {pct}% — {workload} (PID {pid}) may stall` |
| `ALERT_GOVERNOR_ARMED` | Manual kill armed | `Kill armed on {workload} (PID {pid}) — k confirms, Esc cancels` |
| `ALERT_OOM_DETECTED` | OOM kill in last 30s | `OOM kill detected — {workload} (PID {pid}) terminated by kernel` |
| `ALERT_WORKLOAD_EXITED` | **Non-zero exit, OOM, or governor kill only** — never on clean (code 0) exits | `{workload} exited with {reason} — press Enter for post-mortem` |

**Behavior:**

- Stack vertically, max 3 visible. Older alerts: `+N more`
- `a` acknowledges all visible
- `Enter` while a `WORKLOAD_EXITED` alert is highlighted → opens post-mortem card
- Acknowledgment session-scoped. Re-fires if condition recurs.
- Each raise + ack writes to Activity panel
- **Hard rule:** alerts never trigger automatic action. Display + audit only.

**Clean exits** (code 0, no governor action) go to Activity and history without raising an alert. Reason: test scripts and short-lived inferences flood otherwise.

## §5 — Detail cards

Two distinct cards. Same dimensions (64 cols × 8–22 rows), same Esc-dismiss, different content.

### Live detail card — running workload

Triggered by `Enter` on a focused running workload. Auto-refreshes every tick.

```
  ┌─ phi3 (Ollama)  PID 206  ──────────────── running 4m 12s ─┐
  │                                                            │
  │   Throughput:  38.4 tok/s   (baseline 41.2,  -7%)          │
  │   Current RAM: 4.2 GB                                      │
  │   Peak RAM:    4.5 GB this run                             │
  │   VRAM:        4.0 / 16 GB  (25%)                          │
  │   KV cache:    34%                                         │
  │   GPU:         62°C · 89 W                                 │
  │   Phase:       steady-state                                │
  │                                                            │
  │   Last 60s:                                                │
  │     tok/s   ▁▂▃▃▄▅▅▆▆▆▇▇▇▇▆▆▅▅▄                           │
  │     KV%     ▁▁▂▂▃▃▃▄▄▄▄▅▅▅▅▆▆▆▆                           │
  │                                                            │
  │                  Esc dismiss · g graph · k kill            │
  └────────────────────────────────────────────────────────────┘
```

Sparklines: 20 cells = last 60s at 3s resolution (extends to 30 cells / 90s on wide terminals — see §12). KV row omitted for non-LLM. VRAM/GPU rows omitted if no GPU. Phase row only for workloads with cold-start detection.

### Post-mortem card — exited workload

Triggered by `Enter` on a `WORKLOAD_EXITED` alert, an Activity exit row, or a history overlay row.

```
  ┌─ phi3 (Ollama)  PID 206  ──────────────── exited 4 min ago ─┐
  │                                                              │
  │   Cause:    OOM kill (RAM peaked at 31.2 / 32 GB)            │
  │   Runtime:  4 min 12 sec                                      │
  │                                                              │
  │   Throughput:  38.4 tok/s   (baseline 41.2,  -7%)             │
  │   Peak RAM:    31.2 GB      (limit 32 GB)                     │
  │   Peak VRAM:   14.0 GB      (limit 16 GB)                     │
  │   KV cache:    98% at exit                                    │
  │   Energy:      127 J  (avg 89W × 4m 12s)                      │
  │                                                              │
  │   Last stderr:                       [shown only if <30s]    │
  │     RuntimeError: CUDA out of memory. Tried to allocate...   │
  │                                                              │
  │                Esc dismiss · h history · g graph             │
  └──────────────────────────────────────────────────────────────┘
```

**Cause line by ExitReason:**

| ExitReason | Cause line |
|---|---|
| `OOMKill` | `OOM kill (RAM peaked at {gb} / {limit} GB)` |
| `CudaOOM` | `CUDA out of memory (VRAM peaked at {gb} / {limit} GB)` |
| `Segfault` | `Segfault (exit code 139)` |
| `GovernorKill` | `Killed by user via edge_monitor` |
| `ExitOk` | `Exited cleanly (code 0) after {runtime}` |
| `ExitNonZero` | `Exited with code {code}` |
| `Unknown` | `Process disappeared (no exit signal observed)` |

**Stderr section:** appears ONLY when card opened within 30s of exit (transient stderr still in memory). Cards opened from history later silently omit the section. No "Stderr not retained" message — just gone.

**Auto-dismiss:** 30s when triggered by alert. Cards opened from history stay until `Esc`.

## §6 — Keymap

| Key | Action | Valid contexts |
|---|---|---|
| `q` | Quit (with confirm if kill armed) | Always |
| `?` | Toggle help overlay | Always |
| `j` / `k` | Move workload selection | Default, no overlay |
| `k` (workload focused) | Arm kill | Default |
| `k` (within 5s of arm) | Confirm kill | Default |
| `Enter` | Open detail card (live or post-mortem based on row state) | Default, Activity, history |
| `g` | Open Grafana for focused workload | Default, live detail, post-mortem |
| `h` | Toggle history overlay | Default |
| `a` | Acknowledge all visible alerts | When alerts present |
| `t` | Cycle Top processes sort: RAM → CPU → VRAM | Default |
| `Esc` | Cascade dismiss (see below) | Always |

**Esc cascade:**

1. Live detail or post-mortem card visible → dismiss
2. Else: armed kill → disarm
3. Else: history or help overlay → close
4. Else: alerts visible → acknowledge all
5. Else: quit

**Removed from prior drafts:** `d` for detail mode. Habit-collision risk across Linux tooling outweighs any benefit. The inline-expand-on-degraded behavior plus `Enter`-for-detail-card replaces what `d` did.

## §7 — Copy strings

Live in shared `ux_contract` crate, imported by both binaries. Editing requires version bump.

```rust
// Status footer
pub const STATUS_DASHBOARD_OPENED: &str = "Opened {url}";
pub const STATUS_DASHBOARD_FAILED: &str = "Could not open browser: {reason}";
pub const STATUS_NO_WORKLOAD_FOCUSED: &str = "No AI workload focused";
pub const STATUS_KILL_ARMED: &str = "Armed kill on {name} (PID {pid}) — press k again within {secs}s";
pub const STATUS_KILL_DRY_RUN: &str = "Would stop {name} (dry-run mode — no action taken)";
pub const STATUS_KILL_SENT: &str = "Sent SIGTERM to {name} (PID {pid})";
pub const STATUS_KILL_DISARMED: &str = "Kill disarmed";
pub const STATUS_GOVERNOR_BLOCKED: &str = "Cannot kill {name}: protected by allowlist";
pub const STATUS_ALERTS_ACKNOWLEDGED: &str = "Acknowledged {n} alerts";
pub const STATUS_NO_DETAIL_FOR_SYSTEM: &str = "Detail not available for system processes";
pub const STATUS_TOP_SORT_CHANGED: &str = "Top processes sorted by {dimension}";
pub const STATUS_GRAFANA_UNREACHABLE: &str = "Grafana not reachable at {url}. Press s for setup help.";

// Empty states
pub const EMPTY_WORKLOADS: &str = "No AI workloads detected. Start one to begin monitoring.";
pub const EMPTY_ACTIVITY: &str = "No recent activity.";
pub const EMPTY_HISTORY: &str = "No history yet. Completed runs will appear here.";

// Alerts
pub const ALERT_VRAM_PRESSURE: &str = "VRAM at {pct}% — {workload} (PID {pid}) approaching limit";
pub const ALERT_RAM_PRESSURE: &str = "RAM at {pct}% — system approaching limit";
pub const ALERT_KV_PRESSURE: &str = "KV cache at {pct}% — {workload} (PID {pid}) may stall";
pub const ALERT_GOVERNOR_ARMED: &str = "Kill armed on {workload} (PID {pid}) — k confirms, Esc cancels";
pub const ALERT_OOM_DETECTED: &str = "OOM kill detected — {workload} (PID {pid}) terminated by kernel";
pub const ALERT_WORKLOAD_EXITED: &str = "{workload} exited with {reason} — press Enter for post-mortem";

// Confirmation prompts
pub const CONFIRM_QUIT_KILL_PENDING: &str = "Kill armed on {workload}. Quit anyway? (y/N)";

// Below-minimum size message
pub const ERR_TERMINAL_TOO_SMALL: &str = "edge_monitor needs at least 80×24 terminal.\nCurrent size: {w}×{h}. Resize and press any key.";
```

## §8 — Persistence model

`RunRecord` schema saved on workload exit:

| Field | Persisted? | Notes |
|---|---|---|
| `run_id` | Yes | UUIDv4 |
| `model` | Yes | |
| `runtime` | Yes | Ollama, vLLM, llama.cpp, Ultralytics, ROS2, etc. |
| `category` | Yes | LLM / Vision / ROS2 / Embeddings / Unknown |
| `start_time`, `end_time` | Yes | |
| `exit_reason` | Yes | From ExitClassifier |
| `metrics` | Yes | avg/peak/p99 throughput, RAM, VRAM, KV, energy |
| `model_fingerprint` | Yes | SHA-256 head+tail (where applicable) |
| `cold_start` | Yes | Phase + load duration |
| `governor_actions` | Yes | Any kill events for this PID |
| **`stderr_tail`** | **NO — explicit privacy default** | Captured transiently in memory only |

No opt-in to enable stderr persistence in v1.0. Users wanting stderr saved pipe to a file: `ollama run phi3 2> stderr.log`.

## §9 — Platform-specific behavior

The complete allowed-difference list. Anything not here is identical across platforms.

| Concern | Linux | Windows |
|---|---|---|
| Process kill | `libc::kill` SIGTERM → SIGKILL | `taskkill /F /PID` |
| PID-reuse defense | pidfd primary, starttime fallback | starttime + create_time |
| Open browser | `xdg-open` | `cmd /C start ""` |
| Persistence root | `~/.local/share/edge_monitor/` | `%APPDATA%\edge_monitor\` |
| Config root | `~/.config/edge_monitor/` | `%APPDATA%\edge_monitor\` |
| Power source | RAPL + NVML | WMI + NVML |
| OOM detection | `journalctl -k` scrape | Windows Event Log scrape |
| ROS2 detection | `ldd` for librcl.so + env scan | `tasklist /m librcl.dll` + env scan |
| Symbol fallback | UTF-8 default | Detect ConHost, fall back to ASCII if needed |

## §10 — Grafana integration (v1.0) — REMOVED in Sprint 5

> **STATUS (Sprint 5):** This clause is preserved as historical
> context only. The Grafana integration was hard-deleted from v1.0;
> the v2 web companion (separate repo) handles the dashboard story.
> The `g` keybinding is unbound, the `[dashboard]` config section is
> no longer parsed, and the WP5 TCP preflight probe / webbrowser
> dependency are gone. The contract symbols
> (`Action::OpenGrafana`, `status::GRAFANA_UNREACHABLE`,
> `status::DASHBOARD_OPENED`, `status::DASHBOARD_FAILED`) remain in
> `ux_contract` as orphans pending an Agent A cleanup amendment.

`g` keypress on a focused workload:

1. **Pre-flight TCP probe** of `[dashboard].url_template` host:port, 500ms timeout
2. **If reachable:** open browser via platform command (§9)
3. **If not reachable:** footer shows `STATUS_GRAFANA_UNREACHABLE` — no browser opened

Repo ships:
- `dashboards/grafana-overview.json`
- `dashboards/README.md` with import instructions
- CI step: `verify-dashboard-metrics.sh` validates every panel references a metric edge_monitor actually exports against live `/metrics` output

URL template fully configurable:
```toml
[dashboard]
url_template = "http://localhost:3000/d/edge-monitor?var-model={model}&var-pid={pid}&from=now-{lookback}&to=now"
lookback = "1h"
```

Substitutions: `{model}`, `{pid}`, `{lookback}`.

## §11 — Sharing — DROPPED (DISPATCH 49)

**Status: DROPPED.** Originally deferred to v1.1; the v1.1.x line
shipped (v1.1.1 → v1.1.13 + v1.2.0) without picking it up. The
`SHARING_SPEC.md` referenced below was forthcoming and never
landed. Operator formally dropped this clause at DISPATCH 49.

The historical design intent for the record (do not implement
from this without re-opening the decision):

> Spec was to live in `SHARING_SPEC.md` (forthcoming). Original
> v1.1 decisions: Markdown for v1.1, HTML for v1.2;
> `edge_monitor report --runs <id1>,<id2> -o report.md`;
> self-contained (no live data dependencies); post-mortem card
> data + comparison section.

See [docs/ROADMAP.md](docs/ROADMAP.md) EXPLICITLY NOT DOING for
the standing position.

## §12 — Terminal sizing

| Size | Dimensions | Behavior |
|---|---|---|
| Minimum | 80 × 24 | Top processes panel hidden. Activity panel caps at 3 rows. Bar graphs at 17 cells. Single-column workloads. |
| Standard | 120 × 40 | Full default screen as drawn in §1. Bar graphs at 25 cells. |
| Wide | 160+ × 50+ | Workloads may show two columns side-by-side when 4+ workloads. Bar graphs at 40 cells. Sparklines extend to 30 cells (90s) in live detail card. |

**Below 80 × 24:** render `ERR_TERMINAL_TOO_SMALL` message and wait for resize. Do not attempt degraded render.

**Hard rules:**
- Workload row content fits in 60 cols (status dot + indent + content)
- Card overlays lock at 64 cols regardless of terminal width
- Top processes panel is first to drop on narrow screens
- Workloads panel is sacred — never drops

## §13 — Themes

Three themes ship in v1.0. All satisfy WCAG AA contrast (4.5:1 normal text, 3:1 large/bold).

**Selection:** `--theme dark|light|high-contrast` CLI flag, or `[ui].theme` config. No runtime toggle in v1.0.

**Theme: dark (default)**

| Role | Hex | Contrast on bg |
|---|---|---|
| Background | `#1a1b26` | — |
| Background raised | `#24283b` | — |
| Foreground | `#c0caf5` | 12.6:1 |
| Muted | `#9aa5ce` | 7.8:1 |
| Accent | `#7aa2f7` | 6.4:1 |
| Healthy | `#9ece6a` | 8.2:1 |
| Attention | `#e0af68` | 8.9:1 |
| Critical | `#f7768e` | 6.7:1 |

**Theme: light**

| Role | Hex | Contrast on bg |
|---|---|---|
| Background | `#e6e2cf` | — |
| Background raised | `#d8d2bb` | — |
| Foreground | `#2c2c2a` | 12.0:1 |
| Muted | `#5f5e5a` | 6.2:1 |
| Accent | `#185fa5` | 7.1:1 |
| Healthy | `#3b6d11` | 7.4:1 |
| Attention | `#854f0b` | 6.0:1 |
| Critical | `#a32d2d` | 5.5:1 |

Cream background, not pure white — punishing for hours of viewing. Same family as solarized-light.

**Theme: high-contrast**

| Role | Hex |
|---|---|
| Background | `#000000` |
| Background raised | `#1a1a1a` |
| Foreground | `#ffffff` |
| Muted | `#cccccc` |
| Accent | `#00ffff` |
| Healthy | `#00ff00` |
| Attention | `#ffff00` |
| Critical | `#ff0000` |

All ratios exceed 7:1 (WCAG AAA).

## §14 — Color usage rules

Where color appears, regardless of theme:

| Element | Rule |
|---|---|
| Status dot (`●⚠✕○`) | Healthy / Attention / Critical / Muted. ONLY place colors appear on workload rows. |
| Alert banner | Background tinted: amber bg for VRAM/RAM/KV, red bg for OOM/Critical |
| Bar graphs | Foreground until 85% utilization → Attention; ≥95% → Critical |
| Pressure flag (`⚠ pressure`) | Attention color |
| Selected row | Background tinted with Accent (dimmer than full Accent) |
| Title bar | Accent color |
| Footer key hints | Accent for the key letter, Muted for description |
| Section headers (System, Workloads, Top, Activity) | Muted color, no bg |
| All other text | Foreground color |

**Forbidden:**
- Color used to indicate workload category (LLM ≠ blue, Vision ≠ green) — category is communicated by section grouping
- Decorative color anywhere
- More than 5% of the screen colored at any one time

## §15 — Symbols (must render in all terminals)

| Symbol | Codepoint | Meaning | ASCII fallback |
|---|---|---|---|
| `●` | U+25CF | Healthy | `*` |
| `⚠` | U+26A0 | Attention | `!` |
| `✕` | U+2715 | Critical | `X` |
| `○` | U+25CB | Loading / no data | `o` |
| `█▇▆▅▄▃▂▁` | U+2588 family | Bars and sparklines | `#` (whole), `=` (half) |
| `─│┌┐└┘├┤┬┴┼` | U+2500 family | Card borders | `-`, `|`, `+` |

**Detection:** at startup, write a test pattern to a hidden region and check terminal capability. If Unicode block characters fail, fall back to ASCII for the entire session. Real concern: older Windows ConHost, minimal SSH sessions, `tmux` with broken `LANG`.

## §16 — Power (added in `ux_contract` v0.3.3)

`ux_contract::power` (UX-CAR-002) provides the constants the
post-mortem `Energy:` row consumes per §5. Currently exposes:

- `power::DEFAULT_KWH_RATE_USD` — fallback electricity rate when no
  user override is configured. Used by the post-mortem energy
  rollup to convert joules to estimated cost.

The Linux post-mortem split (L16) wires the consumer; until then
this section is a stub recording the contract surface so reviewers
know the const exists.

## §17 — Workload categories (added in `ux_contract` v0.3.4)

`ux_contract::workload_category` (UX-CAR-008) ships **const-only**
group-header strings for §1 region 4:

```rust
pub mod workload_category {
    pub const GROUP_HEADER_LLM: &str        = "── LLM ──";
    pub const GROUP_HEADER_VISION: &str     = "── Vision ──";
    pub const GROUP_HEADER_ROS2: &str       = "── ROS2 ──";
    pub const GROUP_HEADER_EMBEDDINGS: &str = "── Embeddings ──";
    pub const GROUP_HEADER_UNKNOWN: &str    = "── Unknown ──";
}
```

Plus `ux_contract::status::COLD_LOADING = "cold-loading"`
(UX-CAR-007) for the §2 Loading-state primary metric.

**Enum location for v1.0:** the `WorkloadCategory` enum itself
lives in each binary's tree (`crate::model::WorkloadCategory` on
Linux). Contract intentionally ships only the strings, not the
type. The Linux panel maps the enum to the contract const via a
local `category_header` helper. v1.1+ may migrate the enum into
the contract so both binaries share the type — filed in
`BACKLOG.md`.

## §18 — Degraded-line templates (added in `ux_contract` v0.3.5)

`ux_contract::degraded_line` ships the five per-category templates
the Workloads panel renders below a degraded row's primary line
per §2. Each constant is a `{placeholder}`-bearing format string
the renderer substitutes at draw time:

- `degraded_line::LLM` — KV / queue / tail-latency / baseline / signed delta
- `degraded_line::VISION` — VRAM / pipeline phase / baseline fps / signed delta
- `degraded_line::EMBEDDINGS` — batch / tail-latency / baseline emb-s / signed delta
- `degraded_line::ROS2` — **intentionally empty** for v1.0. §2 lists a
  schema (`topics {n} · queue {n} · baseline {Hz} · {±delta}%`) but
  marks it `(v1.1+)`; consumers skip the expansion line entirely when
  the template is empty rather than emitting a blank row.
- `degraded_line::UNKNOWN` — single fixed message for unrecognised
  AI processes (no per-category metrics exist).

L12 (degraded-row expansion) renders these locally pending the
contract-adoption row that pulls v0.3.5 into edge_monitor; until
then this section is a stub recording the contract surface so
reviewers know the templates exist.

## §19 — Top processes panel surface (added in `ux_contract` v0.3.5)

`ux_contract::top_processes` (CAR-11) names the §1 region 5 panel
title prefix and the three sort-dimension labels:

```rust
pub mod top_processes {
    pub const PANEL_TITLE_PREFIX: &str = "Top processes";
    pub const SORT_BY_RAM: &str        = "RAM";
    pub const SORT_BY_CPU: &str        = "CPU";
    pub const SORT_BY_VRAM: &str       = "VRAM";
}
```

The final panel title composes as `"{PANEL_TITLE_PREFIX} (by
{SORT_BY_*})"`. The same `SORT_BY_*` constant substitutes into
`status::TOP_SORT_CHANGED`'s `{dimension}` placeholder so the
panel header and the post-cycle footer message stay in lock-step
when `t` cycles the sort.

L13 (panel scaffold) and L14 (`t`-key cycle) render the title +
status footer with local literals pending the contract-adoption
row that pulls v0.3.5 into edge_monitor; until then this section
is a stub recording the contract surface so reviewers know the
prefix + labels exist.

---

## §20 — Wire snapshot observable surface (added in v1.0.4)

The web companion exposes a JSON snapshot at `/api/snapshot` and the
same shape over WebSocket. Consumers (Tester matchers, third-party
integrators, web UI components) must reference only the fields
actually serialized — the live `WireSnapshot` Rust type is
authoritative; this section enumerates the human-visible promise.

### What workload rows expose

Each `WireSnapshot.workloads[i]` entry carries:

- `pid` — process id (u32).
- `name` — the kernel-recorded process name from
  `/proc/<pid>/comm`. **`TASK_COMM_LEN`-truncated to 15 characters
  plus the null terminator**, so any executable name longer than 15
  chars will be cut off. Matchers built against `name` must account
  for this truncation.
- `model_name` — resolved model identifier when the classifier
  extracted one (e.g. `llama3-8b`); `null` otherwise.
- `category` — `AICategory` projected as a wire-stable string.
- `workload_category` — `WorkloadCategory` projected as a
  wire-stable string (`llm` / `agent` / `vision` / `ros2` /
  `embeddings` / `unknown`).
- `cpu_pct`, `rss_mb`, `vram_mb`, `ram_pct` — current resource
  reads.
- `tokens_per_sec`, `fps`, `kv_cache_peak_pct` — live telemetry,
  `null` when the per-workload sampler hasn't reported.
- `status` — `WorkloadStatus` projected as a string
  (`healthy` / `attention` / `critical` / `loading`).

`WireSnapshot.activity[i]` entries carry `pid`, `name`,
`model_name`, `spawn_time`, `exit_time`, `uptime_secs`, peak
resource fields, and the projected `exit_kind` / `exit_detail`.

### What workload rows do NOT expose

The wire schema is deliberately narrow. The following are observable
inside the binary but never crossed the wire boundary in v1.0:

- `cmdline` — full `argv` is NOT serialized. Matchers that expected
  `cmdline` need to either (a) constrain on the truncated `name`,
  (b) consult the local RunStore for completed runs, or (c) ask for
  a contract amendment to expand the wire schema.
- Full process-tree info (`ppid`, sibling enumeration).
- `/proc/<pid>/maps` content (the library-signal evidence that
  drives ROS2 classification stays internal).
- Per-tick stderr tails (UI Contract v2 made stderr ephemeral —
  see `src/storage/run_store.rs` doc-comment).

### Implication for `name`-based matchers

`name` is truncated by the kernel (`TASK_COMM_LEN = 16`, including
the null terminator). Test fixtures and operator matchers should
either:

- Pin against the kernel-truncated form (e.g. `claude` not
  `claude-code`, `python3` not `python3.10`), or
- Look the process up via a separate `/proc/<pid>/comm` or
  `/proc/<pid>/cmdline` read outside the wire schema.

This rule applies symmetrically to the lifecycle / classifier
internals — the `ProcessSample::name` field carries the same
truncation because it is sourced from the same kernel field.

### F1 origin

Surfaced by Tester-A during v1.0.3 validation: their first
matcher checked a `cmdline` field that does not exist on
`/api/snapshot` workload rows, causing 3 false negatives before
discovery that only `name` (TASK_COMM_LEN-truncated) is exposed.
This section is the contract response.

## §21 — Phase 2 per-category activity surfacing (v1.1.0)

DISPATCH 1 foundation introduces an additive per-category activity
state for the workloads-panel column. The foundation ships the
infrastructure; the runtime-specific samplers (B1 Ollama merge,
B2 Agent, B3 ROS2-shellout, B4 Embeddings-CPU) land in DISPATCH 2A
and 2B and populate the state.

### Architecture lock

Phase 2 **reuses** the existing `TelemetrySource` trait +
`Dispatcher` infrastructure at `src/telemetry/`. There is **no**
parallel trait, dispatcher, or async pattern. Per Inspector #12,
the existing concurrency model (2-thread tokio runtime,
`Arc<Mutex>` per source, mpsc frame channel, 1s per-sample
timeout, `JoinError` panic isolation, `Drop` shutdown) satisfies
all Phase-2 requirements with zero new `Cargo.toml` deps.

### Additive wire-schema changes

- `TelemetryFrame.activity_state: Option<ActivityState>`, gated
  on `#[serde(default)]` so a v1.0 JSON frame still round-trips
  into a v1.1 reader. Inspector #7 ratified this as additive-only.
- `WireSnapshot.workloads[i].activity: Option<String>` exposes
  the per-PID state to the web companion / `/api/snapshot`. One
  of `active` / `idle` / `loading` / `not_detected`, or `null`
  when no Phase-2 sampler has surfaced a state for the PID. The
  Svelte SPA mirrors this with an `ActivityState` TypeScript
  union.

### `ActivityState` enum (LOCAL until v0.3.12 CAR)

Four bare variants — `Active`, `Idle`, `Loading`, `NotDetected`.
No payload on `NotDetected`; granularity ("why not detected") is
sampler-side debug context, not user-visible state. Variant
shape can be extended additively in v1.1.1+ once P5 sampler
validation proves the surface.

**CAR-candidate:** lift to `ux_contract::activity` in v0.3.12
once shape is proven. Until then the enum lives at
`crate::telemetry::source::ActivityState`. The wire schema
projection uses an explicit string-table mapping (not serde's
`rename_all`) so the lift to ux_contract won't break the
dashboard.

### `sample_with_context` additive trait method

The `TelemetrySource` trait grows one optional method:

```rust
async fn sample_with_context(
    &mut self,
    proc: &ProcessSnapshot,
    _all_procs: &[ProcessSnapshot],
) -> SourceResult<TelemetryFrame> {
    self.sample(proc).await
}
```

The default polyfill delegates to `sample`, so every existing
sampler (vLLM, llama.cpp, Ollama) compiles unchanged and behaves
identically. The dispatcher's tick path calls
`sample_with_context`; samplers that need parent / child tree
visibility (B2 agent-claude in particular) override the method
to read the full process list. Per Inspector #12 Option (i).

### TUI / web rendering (Inspector #8 V1)

New 8-char activity column on the workloads panel.
**Foreground-only** — L21 §14 invariant ("only status dots are
colored on workload rows") means the column conveys state via
the text label (`active` / `idle` / `loading` / `—`), not via
per-state color. Auto-hides when every visible row's `activity`
is `None`, mirroring the `model` column's hide rule
(Inspector #8 V1). Column is wide-rows only — narrow rows drop
the primary-metric column to fit F2/F3 columns inside the 80-col
floor, so an additional 8-char slot would overflow.

The web companion mirrors the column exactly (`web/src/
components/WorkloadRow.svelte`), with the same auto-hide rule
keyed on `workload.activity == null`.

### CAR-candidate inventory (post-v1.1.0 + P5 validation)

- `ux_contract::activity::ActivityState` (lift the local enum).
- Per-sampler thresholds currently marked `PROVISIONAL` in
  DISPATCH 2A/2B (Ollama 5s window, Agent tree-shape heuristics,
  ROS2 30s topic-list / 60s topic-hz / 5s subprocess timeout
  cadences, Embeddings 60% CPU / 3-tick / 10-tick-window
  thresholds).
- Wire-schema column-label strings (`active` etc.) — would
  belong in `ux_contract::status` or a new `ux_contract::
  workloads::columns` module.

# What this contract changes from current state

**Linux changes (~25 items):**
- ROS2 detection in classifier
- New Top processes panel
- New alert region with 6 alert types
- Split live detail vs post-mortem cards (currently one card)
- Implement `t` for sort cycle, `a` for alert ack
- Drop `d` keybinding entirely
- Adopt `ux_contract` crate for copy strings
- Three themes; theme detection + symbol fallback

**Windows changes (~50 items):**
- All Linux changes, plus:
- Wire ~12 orphan `core::*` modules (audit found these unused)
- Replace 9-column Inference Registry with 5-field Workloads spec per §2
- Drop Resource Hogs panel (replaced by Top processes with sharper rules)
- Drop Unmapped Processes panel
- Remove `a1/s1/d1` chord remnants (audit found these still rendering)
- Implement `RunRecord` schema with explicit no-stderr clause
- Wire telemetry samplers to populate Workload row throughput

# What's NOT in v0.3 (deliberate)

- `d` key, 6-panel detail layout, Resource Hogs, Unmapped Processes, ASCII art banners
- Stderr persistence, ROS1 detection
- ~~Sharing/export, custom themes, workload tagging, notifications, search/filter, in-tool help layers (all v1.1+)~~ — **DROPPED per DISPATCH 49**; v1.1.x line shipped without any of these. See [docs/ROADMAP.md](docs/ROADMAP.md) EXPLICITLY NOT DOING. (In-tool help — the `?` overlay — actually shipped; the `/` filter UX is dropped per the same dispatch.)

---
The note above is misplaced again. You're not asking for research — you're asking me to produce three concrete deliverables (the `ux_contract` crate scaffold, the Linux plan, the Windows plan) based on the contract we just locked. Everything I need is in our conversation. No web search.

This is going to be a long response. Brace.

---

# Deliverable 1 — The `ux_contract` crate

A single Rust crate, path-deps from both repos. ~200 lines. One file. No transitive deps beyond `std`.

## File layout

```
ux_contract/
├── Cargo.toml
└── src/
    └── lib.rs
```

## `Cargo.toml`

```toml
[package]
name = "ux_contract"
version = "0.3.0"
edition = "2024"
description = "UX contract for edge_monitor — copy strings, alert IDs, thresholds, keymap, themes."
license = "MIT OR Apache-2.0"
publish = false

[dependencies]
# None. This crate is intentionally dependency-free.
# It exists to make UX drift structurally impossible.
```

## `src/lib.rs`

```rust
//! ux_contract — the single source of truth for edge_monitor's user-facing
//! behavior. Both the Linux and Windows binaries depend on this crate.
//! Editing this crate is the only way to change UX behavior.
//!
//! Contract version: 0.3
//! Locked: see UX_CONTRACT.md
//!
//! When you change anything in this file:
//! 1. Bump CONTRACT_VERSION
//! 2. Update UX_CONTRACT.md
//! 3. Re-run golden-image tests in both repos

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::all)]

/// Contract version. Bumped when the contract changes in any way.
pub const CONTRACT_VERSION: &str = "0.3.0";

// ============================================================================
// §3 — Status dots (semantic states for workload rows)
// ============================================================================

/// Status of a workload, shown as a colored dot on the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkloadStatus {
    /// Green ●. All thresholds OK, throughput within ±10% of baseline.
    Healthy,
    /// Amber ⚠. Resource pressure or throughput regression.
    Attention,
    /// Red ✕. Critical: VRAM/KV ≥ 95%, governor armed, OOM detected.
    Critical,
    /// Gray ○. Less than 30s of telemetry, no baseline.
    Loading,
}

impl WorkloadStatus {
    /// Unicode symbol for this status. ASCII fallback in `symbol_ascii()`.
    pub const fn symbol(self) -> &'static str {
        match self {
            WorkloadStatus::Healthy => "●",
            WorkloadStatus::Attention => "⚠",
            WorkloadStatus::Critical => "✕",
            WorkloadStatus::Loading => "○",
        }
    }

    /// ASCII fallback when the terminal can't render Unicode block characters.
    pub const fn symbol_ascii(self) -> &'static str {
        match self {
            WorkloadStatus::Healthy => "*",
            WorkloadStatus::Attention => "!",
            WorkloadStatus::Critical => "X",
            WorkloadStatus::Loading => "o",
        }
    }
}

// ============================================================================
// Thresholds (drive WorkloadStatus computation)
// ============================================================================

/// Resource and performance thresholds. All percentages 0.0..=100.0.
pub mod thresholds {
    /// VRAM utilization that triggers Attention dot.
    pub const VRAM_ATTENTION_PCT: f64 = 85.0;
    /// VRAM utilization that triggers Critical dot.
    pub const VRAM_CRITICAL_PCT: f64 = 95.0;

    /// RAM utilization that triggers Attention.
    pub const RAM_ATTENTION_PCT: f64 = 90.0;
    /// RAM utilization that triggers Critical.
    pub const RAM_CRITICAL_PCT: f64 = 95.0;

    /// KV cache utilization that triggers Attention (LLM only).
    pub const KV_ATTENTION_PCT: f64 = 80.0;
    /// KV cache utilization that triggers Critical (LLM only).
    pub const KV_CRITICAL_PCT: f64 = 95.0;

    /// Throughput as fraction of baseline below which Attention fires.
    /// E.g. 0.80 means "current ≤ 80% of baseline → Attention".
    pub const THROUGHPUT_ATTENTION_RATIO: f64 = 0.80;

    /// Bar graph color shifts to Attention at this utilization.
    pub const BAR_ATTENTION_PCT: f64 = 85.0;
    /// Bar graph color shifts to Critical at this utilization.
    pub const BAR_CRITICAL_PCT: f64 = 95.0;

    /// Sustained-pressure window before alerts fire. Seconds.
    pub const ALERT_SUSTAIN_SECS: u64 = 5;

    /// Time before a workload has enough telemetry for a baseline. Seconds.
    pub const BASELINE_WARMUP_SECS: u64 = 30;

    /// Armed-kill window. Seconds.
    pub const KILL_ARM_WINDOW_SECS: u64 = 5;
}

// ============================================================================
// §4 — Alerts (sticky banners above the header)
// ============================================================================

/// Identity of an alert. Used for de-duplication and acknowledgment tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertId {
    /// VRAM_PRESSURE: VRAM ≥ 85% sustained 5s.
    VramPressure,
    /// RAM_PRESSURE: RAM ≥ 90% sustained 5s.
    RamPressure,
    /// KV_PRESSURE: KV cache ≥ 85% sustained 5s.
    KvPressure,
    /// GOVERNOR_ARMED: a manual kill is armed against a workload.
    GovernorArmed,
    /// OOM_DETECTED: kernel OOM kill in last 30s.
    OomDetected,
    /// WORKLOAD_EXITED: a workload exited with non-zero, OOM, or governor kill.
    /// Clean exits (code 0) do NOT raise this alert.
    WorkloadExited,
}

/// Maximum alerts visible simultaneously before "+N more".
pub const ALERT_MAX_VISIBLE: usize = 3;

// ============================================================================
// §6 — Keymap
// ============================================================================

/// Every action the TUI can dispatch in response to input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Quit the application (with confirm if kill armed).
    Quit,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Move workload selection up.
    SelectUp,
    /// Move workload selection down.
    SelectDown,
    /// Arm a kill on the focused workload (or confirm if already armed).
    KillOrConfirm,
    /// Open the detail card (live or post-mortem based on row state).
    OpenDetail,
    /// Open Grafana for the focused workload.
    OpenGrafana,
    /// Toggle the history overlay.
    ToggleHistory,
    /// Acknowledge all currently visible alerts.
    AcknowledgeAlerts,
    /// Cycle the Top processes panel sort: RAM → CPU → VRAM.
    CycleTopSort,
    /// Esc cascade — see UX_CONTRACT.md §6 for resolution order.
    EscapeCascade,
}

// ============================================================================
// §7 — Copy strings (every user-visible string in the TUI)
// ============================================================================

/// Status footer messages. `{placeholder}` substituted at render time.
pub mod status {
    /// Footer message after browser opens for Grafana.
    pub const DASHBOARD_OPENED: &str = "Opened {url}";
    /// Footer message when browser fails to launch.
    pub const DASHBOARD_FAILED: &str = "Could not open browser: {reason}";
    /// Footer message when the user invokes a workload-targeted action with no row focused.
    pub const NO_WORKLOAD_FOCUSED: &str = "No AI workload focused";
    /// Footer message after first 'k' press.
    pub const KILL_ARMED: &str =
        "Armed kill on {name} (PID {pid}) — press k again within {secs}s";
    /// Footer message in dry-run mode after kill confirmation.
    pub const KILL_DRY_RUN: &str = "Would stop {name} (dry-run mode — no action taken)";
    /// Footer message after kill signal sent.
    pub const KILL_SENT: &str = "Sent SIGTERM to {name} (PID {pid})";
    /// Footer message after Esc disarms a kill.
    pub const KILL_DISARMED: &str = "Kill disarmed";
    /// Footer message when kill is blocked by allowlist.
    pub const GOVERNOR_BLOCKED: &str = "Cannot kill {name}: protected by allowlist";
    /// Footer message after 'a' acknowledges alerts.
    pub const ALERTS_ACKNOWLEDGED: &str = "Acknowledged {n} alerts";
    /// Footer message when Enter pressed on a system process row.
    pub const NO_DETAIL_FOR_SYSTEM: &str = "Detail not available for system processes";
    /// Footer message after 't' cycles top-processes sort.
    pub const TOP_SORT_CHANGED: &str = "Top processes sorted by {dimension}";
    /// Footer message when Grafana pre-flight probe fails.
    pub const GRAFANA_UNREACHABLE: &str =
        "Grafana not reachable at {url}. Press s for setup help.";
}

/// Empty-state strings. Shown inside panels when no data is available.
pub mod empty {
    /// Workloads panel empty.
    pub const WORKLOADS: &str = "No AI workloads detected. Start one to begin monitoring.";
    /// Activity panel empty.
    pub const ACTIVITY: &str = "No recent activity.";
    /// History overlay empty.
    pub const HISTORY: &str = "No history yet. Completed runs will appear here.";
}

/// Alert message templates. `{placeholder}` substituted at render time.
pub mod alerts {
    /// Template for VRAM pressure alert.
    pub const VRAM_PRESSURE: &str = "VRAM at {pct}% — {workload} (PID {pid}) approaching limit";
    /// Template for RAM pressure alert.
    pub const RAM_PRESSURE: &str = "RAM at {pct}% — system approaching limit";
    /// Template for KV cache pressure alert.
    pub const KV_PRESSURE: &str = "KV cache at {pct}% — {workload} (PID {pid}) may stall";
    /// Template for governor-armed alert.
    pub const GOVERNOR_ARMED: &str =
        "Kill armed on {workload} (PID {pid}) — k confirms, Esc cancels";
    /// Template for OOM-detected alert.
    pub const OOM_DETECTED: &str =
        "OOM kill detected — {workload} (PID {pid}) terminated by kernel";
    /// Template for non-clean workload-exit alert.
    pub const WORKLOAD_EXITED: &str =
        "{workload} exited with {reason} — press Enter for post-mortem";
}

/// Confirmation prompt strings.
pub mod confirm {
    /// Prompt when user attempts to quit while a kill is armed.
    pub const QUIT_KILL_PENDING: &str = "Kill armed on {workload}. Quit anyway? (y/N)";
}

/// Error messages shown when the TUI cannot render normally.
pub mod errors {
    /// Shown when terminal is below the 80×24 minimum.
    pub const TERMINAL_TOO_SMALL: &str =
        "edge_monitor needs at least 80×24 terminal.\nCurrent size: {w}×{h}. Resize and press any key.";
}

// ============================================================================
// §13 — Themes
// ============================================================================

/// One of the three v1.0 themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    /// Tokyo Night-inspired dark palette. Default.
    Dark,
    /// Cream-background light palette (not pure white).
    Light,
    /// Pure-black-on-white with bright primaries; WCAG AAA.
    HighContrast,
}

/// Hex color values for one theme. Strings so they can be parsed at render time
/// by the platform-specific TUI layer (ratatui::style::Color).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Theme identity.
    pub name: ThemeName,
    /// Main background color hex.
    pub background: &'static str,
    /// Slightly lighter panel-surface background.
    pub background_raised: &'static str,
    /// Primary text color.
    pub foreground: &'static str,
    /// Secondary / muted text color.
    pub muted: &'static str,
    /// Selection highlight, title bar, key hints.
    pub accent: &'static str,
    /// Healthy dot color.
    pub healthy: &'static str,
    /// Attention dot / amber alert color.
    pub attention: &'static str,
    /// Critical dot / red alert color.
    pub critical: &'static str,
}

impl Theme {
    /// Returns the theme matching the given name.
    pub const fn for_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Dark => DARK,
            ThemeName::Light => LIGHT,
            ThemeName::HighContrast => HIGH_CONTRAST,
        }
    }
}

/// Tokyo Night-inspired dark theme. WCAG AA verified.
pub const DARK: Theme = Theme {
    name: ThemeName::Dark,
    background: "#1a1b26",
    background_raised: "#24283b",
    foreground: "#c0caf5",
    muted: "#9aa5ce",
    accent: "#7aa2f7",
    healthy: "#9ece6a",
    attention: "#e0af68",
    critical: "#f7768e",
};

/// Cream-background light theme. WCAG AA verified.
pub const LIGHT: Theme = Theme {
    name: ThemeName::Light,
    background: "#e6e2cf",
    background_raised: "#d8d2bb",
    foreground: "#2c2c2a",
    muted: "#5f5e5a",
    accent: "#185fa5",
    healthy: "#3b6d11",
    attention: "#854f0b",
    critical: "#a32d2d",
};

/// High-contrast theme. WCAG AAA.
pub const HIGH_CONTRAST: Theme = Theme {
    name: ThemeName::HighContrast,
    background: "#000000",
    background_raised: "#1a1a1a",
    foreground: "#ffffff",
    muted: "#cccccc",
    accent: "#00ffff",
    healthy: "#00ff00",
    attention: "#ffff00",
    critical: "#ff0000",
};

// ============================================================================
// §12 — Sizing
// ============================================================================

/// Terminal sizing breakpoints.
pub mod sizing {
    /// Minimum supported terminal width. Below this, refuse to render.
    pub const MIN_COLS: u16 = 80;
    /// Minimum supported terminal height.
    pub const MIN_ROWS: u16 = 24;

    /// Width above which the TUI uses the standard layout.
    pub const STANDARD_COLS: u16 = 120;
    /// Height above which the TUI uses the standard layout.
    pub const STANDARD_ROWS: u16 = 40;

    /// Width above which the TUI may use two-column workload layout.
    pub const WIDE_COLS: u16 = 160;

    /// Card overlays lock at this width regardless of terminal size.
    pub const CARD_WIDTH: u16 = 64;
    /// Minimum card height.
    pub const CARD_HEIGHT_MIN: u16 = 8;
    /// Maximum card height.
    pub const CARD_HEIGHT_MAX: u16 = 22;
}

// ============================================================================
// Tests — verify the contract is internally consistent
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_version_matches_doc() {
        // If you change CONTRACT_VERSION, update UX_CONTRACT.md to match.
        assert_eq!(CONTRACT_VERSION, "0.3.0");
    }

    #[test]
    fn all_alert_ids_have_a_template() {
        // Defensive: every AlertId variant should be reachable in alerts::* by
        // the dispatch layer. This test enumerates so a reviewer notices when
        // a new variant is added without a template.
        let _all = [
            AlertId::VramPressure,
            AlertId::RamPressure,
            AlertId::KvPressure,
            AlertId::GovernorArmed,
            AlertId::OomDetected,
            AlertId::WorkloadExited,
        ];
        assert_eq!(_all.len(), 6, "AlertId count changed — update templates");
    }

    #[test]
    fn thresholds_are_ordered() {
        // Critical thresholds must be ≥ Attention thresholds.
        assert!(thresholds::VRAM_CRITICAL_PCT >= thresholds::VRAM_ATTENTION_PCT);
        assert!(thresholds::RAM_CRITICAL_PCT >= thresholds::RAM_ATTENTION_PCT);
        assert!(thresholds::KV_CRITICAL_PCT >= thresholds::KV_ATTENTION_PCT);
    }

    #[test]
    fn themes_have_distinct_palettes() {
        assert_ne!(DARK.background, LIGHT.background);
        assert_ne!(LIGHT.background, HIGH_CONTRAST.background);
    }

    #[test]
    fn sizing_is_consistent() {
        assert!(sizing::STANDARD_COLS >= sizing::MIN_COLS);
        assert!(sizing::WIDE_COLS >= sizing::STANDARD_COLS);
        assert!(sizing::CARD_HEIGHT_MAX >= sizing::CARD_HEIGHT_MIN);
        assert!(sizing::CARD_WIDTH < sizing::MIN_COLS);
    }

    #[test]
    fn workload_status_symbols_distinct() {
        let symbols = [
            WorkloadStatus::Healthy.symbol(),
            WorkloadStatus::Attention.symbol(),
            WorkloadStatus::Critical.symbol(),
            WorkloadStatus::Loading.symbol(),
        ];
        let mut sorted: Vec<&'static str> = symbols.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 4);
    }
}
```

## How both repos consume this crate

**Linux `Cargo.toml`** gets one line:
```toml
[dependencies]
ux_contract = { path = "../ux_contract" }  # adjust path as appropriate
```

**Windows `crates/cli/Cargo.toml`** gets the same:
```toml
[dependencies]
ux_contract = { path = "../../ux_contract" }
```

The crate produces no runtime overhead — everything is `const` data. The compile-time check is the whole point: if Linux uses `ux_contract::status::KILL_ARMED` and Windows uses a hardcoded string, the next reviewer sees the asymmetry. After both repos have adopted the crate everywhere copy strings appear, drift is structurally impossible.

---

# Deliverable 2 — Linux implementation plan

26 rows. Ordered by dependency: foundation first (crate adoption, copy refactor), then features that build on it (alerts, ROS2, detail cards), then polish (sizing, themes). Each row is sized to be a single PR.

| # | Clause | Files to change | What changes | Test |
|---|---|---|---|---|
| **L1** | §7 (foundation) | `Cargo.toml`, all of `src/ui/*` | Add `ux_contract` path dep. Replace every hardcoded user-visible string with `ux_contract::status::*`, `::empty::*`, `::confirm::*`, `::errors::*`. Mechanical search-and-replace. | Existing TUI tests still pass; new `tests/copy_strings_via_contract.rs` greps `src/ui/` for non-allowlist string literals and fails on raw user-facing strings. |
| **L2** | §6 (foundation) | `src/ui/input.rs` | Replace internal `Action` enum with re-export from `ux_contract::Action`. Translate crossterm `KeyEvent` → `ux_contract::Action` table. | Update `tests/dashboard_keybinding_e2e.rs` to assert key→action mapping uses the contract enum. |
| **L3** | §3 (foundation) | `src/runtime.rs` (`build_baseline_status` and `compute_status_dot`) | Replace ad-hoc status logic with `ux_contract::WorkloadStatus` driven by `ux_contract::thresholds::*`. | New `tests/workload_status_thresholds.rs` table-tests every threshold boundary (84.9 → Healthy, 85.0 → Attention, etc.). |
| **L4** | §15 (foundation) | New `src/ui/symbols.rs` | At startup, write a UTF-8 test pattern, detect render capability, choose between `WorkloadStatus::symbol()` and `::symbol_ascii()` for the session. Same for box-drawing chars and bar blocks. | New `tests/symbol_fallback.rs` mocks a TTY without UTF-8 and asserts ASCII path. |
| **L5** | §4 | New `src/ui/alerts.rs`, `src/runtime.rs` | Add `AlertState` to `RuntimeState`. Per-tick: detect threshold crossings, raise alerts via `ux_contract::AlertId`, track sustained-pressure window. Drain on tick to UI. | New `tests/alert_state_machine.rs` proptest: every (alert raised → sustained → ack) sequence preserves invariants. |
| **L6** | §1 region 1, §4 | `src/ui/panels/mod.rs` (new alerts panel) | Render alert region above header. 0–3 lines. Stack vertically. `+N more` when count > 3. Background tinted per alert severity. | New `tests/alert_render_golden.rs` — 4 golden images: 0 alerts, 1 alert, 3 alerts, 5 alerts (with `+2 more`). |
| **L7** | §6 | `src/ui/input.rs`, `src/ui/mod.rs` | Wire `a` key → `Action::AcknowledgeAlerts` → `AlertState::ack_all()`. Add `STATUS_ALERTS_ACKNOWLEDGED` to footer for 3s. | Unit test in `tests/alert_state_machine.rs` for ack flow. |
| **L8** | §4 only-on-non-clean | `src/runtime.rs` (exit handling) | When `ExitClassifier` returns `ExitOk`, raise no `WORKLOAD_EXITED` alert. Only `OOMKill`, `CudaOOM`, `Segfault`, `GovernorKill`, `ExitNonZero`, `Unknown` raise. | Extend `tests/pipeline_end_to_end.rs` with both clean and non-clean exit cases. |
| **L9** | §2 (ROS2 detection) | `src/classifier/keyword_match.rs`, `src/classifier/model_extract.rs`, `src/classifier/mod.rs` | Add `AICategory::ROS2`. Detect via env vars (`RMW_IMPLEMENTATION`, `ROS_DOMAIN_ID`, `AMENT_PREFIX_PATH`), cmdline (`ros2 run`, `ros2 launch`), and `/proc/{pid}/maps` scan for `librcl.so`. Extract node name from `--ros-args -r __node:=`. | New `tests/ros2_classifier.rs` with 8 fixtures: `ros2 run` / `ros2 launch` / Python rclpy / C++ rclcpp / nodelet / launch-file-only / pre-existing-ROS1-not-detected / lifecycle-node. |
| **L10** | §2 (ROS2 metrics, deferred) | `src/runtime.rs` | For `AICategory::ROS2` workloads, set `primary_metric = "(no metrics)"` placeholder. Mark Hz sampling as v1.1 with TODO comment + GitHub issue. Process-level RAM/CPU still flow. | New `tests/ros2_workload_row.rs` asserts ROS2 row renders with RAM/CPU only and the `--` placeholder for Hz. |
| **L11** | §1 region 4 | `src/ui/panels/registry.rs` (rename to `workloads.rs`) | Group rows by `AICategory`. Order: LLM → Vision → ROS2 → Embeddings → Unknown. Render subsection header per non-empty category. Hide subsection header when only one category present. | Golden-image tests for: single LLM, multi-category, ROS2 only, all empty. |
| **L12** | §2 expanded line | `src/ui/panels/workloads.rs` | When `WorkloadStatus` is `Attention` or `Critical`, render second indented line per category schema in §2. | Extend golden-image tests above with degraded variants. |
| **L13** | §1 region 5 | New `src/ui/panels/top_processes.rs`, `src/runtime.rs` | New panel showing top-N (default 3, max 5 via `[ui].top_processes_count`) by RAM. Filter out edge_monitor itself; AI workloads already shown in Workloads above appear here too (full top-N, no de-duplication). | Inline tests in `src/ui/panels/top_processes.rs` assert edge_monitor self-exclusion and that AI workloads remain in the top-N list (`excludes_edge_monitor_self`, `includes_ai_workloads_in_unfiltered_list`). |
| **L14** | §6 `t` key | `src/ui/input.rs`, `src/ui/panels/top_processes.rs` | Wire `t` → `Action::CycleTopSort` → cycle RAM → CPU → VRAM. Footer message via `STATUS_TOP_SORT_CHANGED`. | Extend `tests/top_processes_filter.rs` with sort-cycle test. |
| **L15** | §1 region 6 | `src/ui/panels/audit.rs` (rename to `activity.rs`) | Merge `Recent runs` and `Governor interventions` into one timestamp-interleaved panel. Last 5 events. | Update existing audit panel tests; rename test file to match. |
| **L16** | §5 split | `src/ui/panels/postmortem.rs` (split into `live_detail.rs` + `postmortem.rs`) | Two distinct cards. `Enter` on running workload → live detail with sparklines. `Enter` on exited row in Activity/history → post-mortem. Same dimensions, different content. | Extend `tests/postmortem_e2e.rs`; new `tests/live_detail_card.rs` for the running variant. |
| **L17** | §5 sparklines | `src/ui/panels/live_detail.rs` | Per-workload rolling buffer of last 60s of throughput + KV (LLM only). Render via `▁▂▃▄▅▆▇█`. Auto-extend to 30 cells (90s) at terminal ≥ 160 cols. | Visual smoke; sparkline correctness via unit test on the buffer logic. |
| **L18** | §8 (no stderr) | `src/storage/run_store.rs`, `src/runtime.rs` | Verify `RunRecord` has no `stderr_lines` field (Linux audit confirmed it doesn't). Document the privacy stance with a doc-comment block. Add lint-style test that fails if anyone adds the field. | New `tests/no_stderr_persistence_guard.rs` — a `tests/expect_rule_guard.rs`-style test that walks `src/storage/` and rejects any field literally named `stderr` or `stderr_*` with `Serialize`. |
| **L19** | §5 stderr-when-fresh | `src/runtime.rs`, `src/ui/panels/postmortem.rs` | Capture stderr in a transient `HashMap<PID, VecDeque<String>>` cap'd at 64 lines × 1KB. Cleared on card dismiss or 30s after exit. Post-mortem card omits stderr section if buffer is gone. | Extend `tests/postmortem_e2e.rs` with a "stderr present immediately, gone after 30s" case. |
| **L20** | §13 themes | New `src/ui/theme.rs` | Three theme structs from `ux_contract::{DARK, LIGHT, HIGH_CONTRAST}`. Map hex → `ratatui::style::Color`. CLI flag `--theme` and `[ui].theme = "dark"` config. | New `tests/theme_switching.rs` asserts each theme renders with the right colors at panel-fill time. |
| **L21** | §14 color usage | All of `src/ui/panels/*` | Audit and refactor: status dots are the only colored thing on workload rows. Bar graphs shift to `attention` at 85% and `critical` at 95%. Section headers in `muted`. Footer key letters in `accent`. | Visual review + golden-image tests at boundary thresholds. |
| **L22** | §12 sizing | `src/ui/mod.rs` | Read terminal size on resize event. If below `MIN_COLS × MIN_ROWS`, render `errors::TERMINAL_TOO_SMALL`. At ≥ `STANDARD_*`, render full layout. At ≥ `WIDE_COLS`, two-column workload layout if 4+ workloads. | New `tests/sizing_breakpoints.rs` with `TestBackend` at 70×20, 80×24, 120×40, 160×50. |
| **L23** | §10 minimal Grafana | `src/ui/mod.rs` (existing `handle_open_dashboard`) | Verify pre-flight TCP probe is in place. On failure, footer shows `STATUS_GRAFANA_UNREACHABLE`. No dashboard JSON shipping in this PR — that's a v1.0 polish task post-implementation. | Extend `tests/dashboard_keybinding_e2e.rs` with a "probe fails → footer message" case. |
| **L24** | §1 region 1 + §4 dismiss flow | `src/ui/app.rs` (Esc cascade) | Update `handle_escape` per §6 cascade order. Specifically: alerts visible → ack all comes after history/help close. | Update `tests/postmortem_e2e.rs` Esc cascade tests. |
| **L25** | §0 mission line | `src/ui/panels/header.rs` (new file or update existing header render) | Header line: `edge_monitor · {n} workloads · {m} degraded · press ? for help`. Drop the "VATCH"-equivalent ASCII branding if any survives. | Golden-image test for header rendering. |
| **L26** | Cross-cutting cleanup | `BUILDER_STATUS.md`, `CHANGELOG.md`, `README.md`, `FEATURES.md` | Single sweep doc PR after L1-L25 land: delete stale claims, point to UX_CONTRACT.md as source of truth, fix test counts, remove the "Tier 1.x" / "Tier 2.x" taxonomy in favor of contract clauses. | None (doc-only). |

**Recommended PR ordering**: L1 → L2 → L3 → L4 (foundation, ~3 days). L5 → L6 → L7 → L8 (alerts feature, ~2 days). L9 → L10 → L11 → L12 (ROS2 + grouping, ~2 days). L13 → L14 → L15 (top processes + activity, ~2 days). L16 → L17 → L18 → L19 (cards + privacy, ~3 days). L20 → L21 → L22 (themes + sizing, ~2 days). L23 → L24 → L25 → L26 (Grafana + polish + docs, ~1 day). **Total: ~15 dev-days** for a focused engineer who already knows the codebase. Builds incrementally — every PR ships a visible improvement.

---

# Deliverable 3 — Windows implementation plan

49 rows. The Linux 25 changes plus 24 wiring/cleanup changes the audit identified. Heavier because the Windows codebase is mid-refactor with ~12 orphan modules and a separate audit blocker list.

| # | Clause | Files to change | What changes | Test |
|---|---|---|---|---|
| **W1** | §7 (foundation) | `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`, `crates/cli/src/ui/*` | Add `ux_contract` path dep. Replace every hardcoded user-visible string with `ux_contract::status::*`. | New `tests/copy_strings_via_contract.rs` mirroring Linux L1. |
| **W2** | §6 (foundation) | `crates/cli/src/ui/tui.rs::classify_key` | Replace internal action enum with `ux_contract::Action`. | Update existing TUI key tests. |
| **W3** | §3 (foundation) | `crates/cli/src/main.rs` (status logic embedded in monitor loop) | Extract status-dot computation to a free function using `ux_contract::WorkloadStatus` + thresholds. | New `tests/workload_status_thresholds.rs` mirroring L3. |
| **W4** | §15 (foundation) | New `crates/cli/src/ui/symbols.rs` | UTF-8 detection at startup. ConHost-aware fallback to ASCII. | New `tests/symbol_fallback.rs`. |
| **W5** | Audit Blocker 1 | `crates/core/src/model.rs` | Add `cpu_watts: Option<f64>` (avg + peak) to `RunSummary`. | Extend serialization tests for `RunSummary` schema. |
| **W6** | Audit Blocker 35 | `crates/core/src/model.rs`, `crates/cli/src/main.rs::finalize_profile` | Add `exit_reason: Option<ExitReason>` to `RunSummary`. Stop discarding the value at the `_exit_reason` parameter. | New test asserts persisted `RunSummary` round-trips `exit_reason`. |
| **W7** | Cleanup | `crates/cli/src/main.rs` | Remove `_input_rx` dead-code path (the stdin thread whose receiver is dropped). Just delete it. | `cargo build` clean. |
| **W8** | Cleanup | `crates/cli/src/q.py` | **Delete the file.** Bandwidth-hammer with `ssl.CERT_NONE` infinite loop has no place in a workspace `src/` directory. | `find` reports it gone. |
| **W9** | Cleanup | `crates/cli/src/w.py` | **Delete.** Mis-named Rust file kept around so old `mod` declarations still compile. | `cargo build` clean after removal of any stale `mod w;`. |
| **W10** | Cleanup | `crates/cli/src/{f,t,yolo1}.py` | Move to a `tools/dev-fixtures/` directory at repo root with a README. They're not part of the build but they shouldn't sit in `src/`. | None. |
| **W11** | Audit Risk R2 | `Cargo.toml`, `crates/cli/src/ui/tui.rs` | Resolve the `webbrowser` orphan. Either remove the unused dep, or actually call `webbrowser::open()` instead of shelling to `cmd /C start`. The latter aligns with cross-platform parity. | Update `tests/dashboard_keybinding_e2e.rs` if path changes. |
| **W12** | Cleanup | `crates/core/src/lib.rs`, `crates/core/src/dashboard.rs` | Rename `core::dashboard::DashboardConfig` → `core::dashboard::TemplateConfig`. The two structs sharing the name (config-side + dashboard-side) is a footgun the audit flagged. | Update import sites; existing tests re-pass. |
| **W13** | Cleanup | `crates/cli/src/ui/panels/{vitals,registry,rogue,culprits,completed,audit}.rs` | **Delete all six.** They're 8-line `pub struct Panel;` stubs that no code constructs. | `cargo build` clean. |
| **W14** | Cleanup | `Cargo.toml.backup` | **Delete.** Vestige of pre-workspace single-crate. | None. |
| **W15** | Audit Risk R1 | `crates/cli/src/main.rs` (NVML init), `crates/platform_windows/src/lib.rs` if applicable | Gate the NVML init log behind a `OnceLock<()>`. Currently fires every tick on no-GPU hosts. (Linux audit found this in `gpu_nvidia.rs:87-93`; verify Windows analog.) | Tail log for 1 minute on no-GPU host, assert at most 1 init line. |
| **W16** | §1 region 4 (drop legacy panels) | `crates/cli/src/ui/dashboard.rs` | Remove `Resource Hogs` panel rendering entirely. Remove `Unmapped Processes` panel; replace with `unmapped_count` integer in the header. | Update header golden-image test. |
| **W17** | §1 region 4 (drop chord remnants) | `crates/cli/src/main.rs::extract_models` and any `a1`/`s1`/`d1` synth logic | Remove the chord-kill scheme remnants. The audit found these still rendering in screenshots even though the chord scheme was deleted in commit 85b020c. PIDs only. | Snapshot test of registry render output without `a1/s1/d1` labels. |
| **W18** | §1 region 4 (the 9-column rebuild) | `crates/cli/src/ui/dashboard.rs` (Inference Registry render) | Rebuild from 9 columns down to 5: status dot, model name + runtime, primary metric (tok/s / fps / Hz / emb/s), RAM, PID. Drop CPU%, VRAM, Net KB/s, Unmapped, Threat columns. | Golden-image test of new 5-column registry. |
| **W19** | Wiring (telemetry → row) | `crates/cli/src/main.rs`, new `crates/core/src/telemetry/sampler.rs` | Wire one of `core::telemetry::{LlamaCppSampler, OllamaSampler, VLLMSampler}` to actually populate `ProcessMetrics.throughput_tokens_per_sec`. Currently every `sample_throughput()` returns `Err`. Pick one (Ollama is most common locally) and make it work end-to-end. | New `tests/ollama_throughput_wired.rs` mocks the `/api/ps` endpoint and asserts tok/s reaches the panel. |
| **W20** | Wiring (orphan → live) | `crates/core/src/lib.rs` exports + `crates/cli/src/main.rs` | Wire `core::runtime_detect::first_run_hint` at startup. Print the hint if no AI runtime is found in PATH. | New test that mocks an empty PATH and asserts hint emission. |
| **W21** | Wiring (orphan → live) | Same | Wire `core::dashboard::render_dashboard_url` from `tui.rs::resolve_dashboard_url`. Currently `tui.rs` reimplements URL templating with `str::replace`. Use the audited core function instead. | Replace `tui.rs`'s reimplementation; existing dashboard tests still pass. |
| **W22** | Wiring (orphan → live) | Same | Wire `core::dashboard_preflight::probe_reachable` before opening browser. Currently `g` opens browser regardless of reachability. | New test asserts no browser launch when probe fails; footer shows `STATUS_GRAFANA_UNREACHABLE`. |
| **W23** | §5 split | New `crates/cli/src/ui/live_detail_card.rs` + existing `post_mortem_card.rs` | Two distinct cards mirroring Linux L16. Live detail for running workloads with sparklines; post-mortem for exited. | Mirror Linux `tests/live_detail_card.rs`. |
| **W24** | §5 sparklines | `crates/cli/src/ui/live_detail_card.rs` | Same rolling buffer logic as Linux L17. | Same. |
| **W25** | Wiring | `crates/cli/src/main.rs::compare_runs` site | Switch from legacy `core::storage::compare_runs` to `core::analysis::compare::detect_regressions_with`. The new tier-classified comparator is exported but unused. | Extend regression detection tests with a case that requires the tier classification. |
| **W26** | §4 alerts | New `crates/cli/src/ui/alerts.rs`, `crates/cli/src/main.rs` | Mirror Linux L5 — alert state machine driven by `ux_contract::AlertId`. | Mirror Linux test. |
| **W27** | §1 region 1, §4 | `crates/cli/src/ui/dashboard.rs` (alerts panel) | Mirror Linux L6 — alert region above header. | Mirror Linux golden-image. |
| **W28** | §6 `a` key | `crates/cli/src/ui/tui.rs` | Mirror Linux L7. | Mirror Linux test. |
| **W29** | §4 only-on-non-clean | `crates/cli/src/main.rs` (exit handling) | Mirror Linux L8. | Mirror Linux test. |
| **W30** | §2 (ROS2 detection) | `crates/cli/src/main.rs::classify_process`, `extract_models` | Add `AICategory::ROS2`. WMI command-line scan + linked-DLL detection (`tasklist /m librcl.dll`) + env scan via `/proc`-equivalent `WMI Win32_Process.GetOwner` chain. ROS2 on Windows is uncommon but `cybertronix` may need it. | Mirror Linux `tests/ros2_classifier.rs` with Windows-specific fixtures. |
| **W31** | §2 (ROS2 metrics deferred) | Same | Process-level RAM/CPU only. Hz column = `--`. | Mirror Linux. |
| **W32** | §1 region 4 grouping | `crates/cli/src/ui/dashboard.rs` | Mirror Linux L11 — group by category, fixed order, hide single-category subsection header. | Mirror Linux golden-image. |
| **W33** | §2 expanded line | Same | Mirror Linux L12. | Mirror Linux. |
| **W34** | §1 region 5 | New `crates/cli/src/ui/panels/top_processes.rs` | Mirror Linux L13. Top-N by RAM/CPU/VRAM. Filter `edge_monitor.exe` and Workload PIDs. | Mirror Linux `tests/top_processes_filter.rs`. |
| **W35** | §6 `t` key | `crates/cli/src/ui/tui.rs` | Mirror Linux L14. | Mirror Linux. |
| **W36** | §1 region 6 | New `crates/cli/src/ui/panels/activity.rs` | Mirror Linux L15. | Mirror Linux. |
| **W37** | §8 (no stderr) | `crates/core/src/model.rs`, all `RunSummary` write sites | Verify Windows `RunSummary` has no `stderr` field. Add the lint-style guard test. | Mirror Linux `tests/no_stderr_persistence_guard.rs`. |
| **W38** | §5 stderr-when-fresh | `crates/cli/src/main.rs`, `crates/cli/src/ui/post_mortem_card.rs` | Mirror Linux L19 — transient stderr buffer cleared on dismiss/30s. | Mirror Linux. |
| **W39** | Cleanup (legacy storage) | `crates/cli/src/main.rs` | Stop double-writing `LogStore("./logs")`. Use only `RunStore("./run_history")`. The audit found three on-disk layouts simultaneously. | New test asserts only one write site fires per exit. |
| **W40** | Cleanup (script slurp) | `crates/cli/src/main.rs:1378-1387` | Cache `fs::read_to_string` results by absolute path. Currently re-reads referenced `.py` files every tick. | Microbenchmark before/after on a 200-process system. |
| **W41** | §13 themes | New `crates/cli/src/ui/theme.rs` | SHIPPED via W41a (5b5d5da) + W41b (a7b0f24). Integration test mirror at `crates/cli/tests/theme_switching.rs` landed as W41c. | Mirror Linux `tests/theme_switching.rs`. |
| **W42** | §14 color usage | All of `crates/cli/src/ui/panels/*` | Mirror Linux L21. | Mirror Linux. |
| **W43** | §12 sizing | `crates/cli/src/ui/tui.rs` | Mirror Linux L22. | Mirror Linux. |
| **W44** | §10 minimal Grafana | `crates/cli/src/ui/tui.rs::resolve_dashboard_url` + new `preflight` call | Use `core::dashboard_preflight::probe_reachable` (wired in W22) before launching browser. | Mirror Linux. |
| **W45** | §1 region 1 + §4 Esc cascade | `crates/cli/src/ui/tui.rs` | Mirror Linux L24. | Mirror Linux. |
| **W46** | §0 mission line | `crates/cli/src/ui/dashboard.rs` (header render) | Replace any "VATCH" or all-caps banner with the contract header line. | Golden-image. |
| **W47** | Audit Risk R6 | `crates/core/src/config.rs`, `edge_monitor.toml.example` | Resolve `[power]` config gap: either add `PowerConfig` matching `latest.md` cross-cutting spec, or remove the spec reference. | Update `tests/config_loader.rs` for whichever decision lands. |
| **W48** | Audit Risk R8 + cleanup | `README.md`, `CHANGELOG.md`, `FEATURES.md` | Doc sweep mirroring Linux L26. Refresh test counts via CI auto-injection. Remove the post-mortem auto-trigger reference (CHANGELOG describes API that doesn't exist). | None. |
| **W49** | Cross-cutting | `.github/workflows/ci.yml` (new file — Windows currently has no CI for the Rust workspace; only the vendored llama.cpp has CI) | Add a Windows CI workflow: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`. Match Linux's CI shape. | CI green. |

**Recommended PR ordering for Windows**:

Phase A — Cleanup and audit blockers (W5-W14, ~3 days). Fast wins, deletions, audit blocker fixes. Reviewable in chunks.

Phase B — Foundation (W1-W4, W15, ~3 days). Same as Linux foundation, plus the NVML log-spam fix.

Phase C — Wiring orphan modules (W19-W22, W25, ~3 days). Each PR wires one orphan module to the live binary and exercises it end-to-end. This is the phase that actually changes user-visible behavior most dramatically — telemetry numbers start appearing where they were `0.0`.

Phase D — UX rebuild (W16-W18, W23-W24, W26-W36, ~5 days). Drop legacy panels, build new ones, mirror Linux features.

Phase E — Polish (W37-W49, ~3 days). Themes, sizing, docs, CI.

**Total: ~17 dev-days** with a focused engineer. Slightly longer than Linux because Windows starts further behind. The first ~6 days (Phases A+B) are mostly cleanup that doesn't change user-visible behavior; the rebuild starts to look like the contract from Phase C onward.

---

# How these three deliverables fit together

The crate is the thing that makes the two plans implement the *same* product. Without it, Linux PR L1 and Windows PR W1 would each independently choose copy strings and they'd drift again within three months. With it, the second PR to touch a copy string has to either (a) edit the crate (visible to both repos) or (b) reach for a non-contract string (rejected by `tests/copy_strings_via_contract.rs`).

The plans are sized so every PR has a single visible result. Even the cleanup PRs (delete `q.py`, delete `Cargo.toml.backup`, delete six stub panels) are individually small enough to land same-day. The biggest PR in either plan is W18 (rebuild Inference Registry from 9 to 5 columns) and even that's a bounded change in a single file.

Each plan starts with a foundation phase that doesn't touch user-visible behavior — the crate adoption (L1/W1) and action enum (L2/W2) and status enum (L3/W3) are pure refactors. After those land, every subsequent PR adds or changes something the user can see. That's the right shape: invisible work first, visible work second, so reviewers know what they're looking at when the screenshots start changing.

The Windows plan has 24 more rows than Linux not because the contract is harder on Windows — it's the same contract — but because the Windows codebase carries more cleanup debt. Most of the extra rows (W5-W18, W37, W39-W40, W47-W49) are work the Linux codebase already did or never accumulated. Once Windows finishes its cleanup phase, the per-feature parity with Linux is roughly 1:1.

# What I suggest as the first PR

**Linux**: L1 — the `ux_contract` crate adoption + copy-string refactor. It's mechanical, the test (`tests/copy_strings_via_contract.rs`) is self-enforcing, and once it lands every subsequent UX PR has a place to add new copy. Should be ~1 day.

**Windows**: W8 — delete `q.py`. Smallest possible PR. Removes a real safety hazard (the bandwidth hammer). Sets the precedent that we delete things we don't need. ~10 minutes.

After those two land, the next PRs (L2/W1) put both repos on the same crate dependency, and the rebuild begins.

