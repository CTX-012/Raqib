# PENDING — things waiting on the human

## ✅ SAFE-TEST CONFIG — ready to apply — 2026-07-30

**READ-ONLY dispatch.** Nothing applied. Nothing armed. No code touched. No config
written. This block is the operator's ready-to-paste artefact + the checks that must
pass BEFORE `auto_actuate = true`.

Every key below was confirmed against the shipped schema (`src/config.rs`,
`src/governor/policy.rs`, `src/thresholds.rs`) and every process name against the
LIVE host (`/proc/<pid>/comm`, `/api/snapshot`, `nvidia-smi`).

---

### 🚨 §0 — FOUR DEFECTS in the 2026-07-29 draft config. Do NOT paste that block.

The recommended config in the safety-investigation below (§"Recommended safe-test
config") is **broken in four ways**. Each one was verified:

| # | Defect | Consequence | Proof |
|---|---|---|---|
| **D1** | `vram_critical_pct = 15.0` with no `vram_attention_pct` override | **edge_monitor REFUSES TO START.** `check_pair` rejects `critical < attention`; attention defaults to 85.0 → `vram_critical_pct (15.0) must be ≥ vram_attention_pct (85.0)` | [`thresholds.rs:133-138,189-201`](../../src/thresholds.rs#L133-L201), default [`ux_contract/src/lib.rs:134`](../../../ux_contract/src/lib.rs#L134) |
| **D2** | Allowlist entry `"controller_server"` | **NEVER MATCHES.** Policy matches `/proc/<pid>/comm`, which the kernel truncates to 15 chars. The real name is `controller_serv`. A 17-char entry can never equal a 15-char comm | [`linux_proc.rs:119,203-207`](../../src/platform/linux_proc.rs#L203-L207); live `comm` = `controller_serv` |
| **D3** | Disarm via `kill $(pgrep -f "target/release/edge_monitor")` | **KILLS THE WRONG PROCESS.** Tested live: that pattern returned PID 126199 (a transient `rustc` build job) and did **NOT** match the running monitor (PID 123212, whose `comm` AND `argv[0]` are both `edge`). Disarm would appear to succeed while the killer stayed up | verified live via `pgrep -f` + `/proc/123212/comm` |
| **D4** | Draft allowlist omits 5 classes of real workload, and specifying `allowlist` at all **silently drops the built-in shell/init defaults** (`sshd`,`bash`,`zsh`,`sh`,`systemd`,`init`,`kworker`,`kthreadd`) — `#[serde(default)]` is per-FIELD, so a present `allowlist` replaces the default set wholesale | Gazebo, rviz2, the ROS2 daemon, VS Code and the shells all end up unlisted | [`config.rs:512-514`](../../src/config.rs#L512-L514) + [`policy.rs:37-46`](../../src/governor/policy.rs#L37-L46) |

---

### §1 — THE ALLOWLIST: every real workload on this host, enumerated

Names are **exact `/proc/<pid>/comm` strings** — kernel-truncated at 15 chars.
`policy.evaluate` does `whitelist_names.contains(name)`: **exact match, no globbing,
no substring, no regex** ([`policy.rs:70-72`](../../src/governor/policy.rs#L70-L72)).
A name that is one character off is *not protected*.

PIDs below are indicative only — the ROS2 stack was restarted mid-investigation
(PIDs moved 119752→129358 range). **Names are the stable identifier; PIDs are not.**

**ROS2 / Nav2 / SLAM stack — 15 names** (all `AICategory::Inference` on the wire):

| comm (exact) | e.g. PID | Real binary | Truncated? |
|---|---|---|---|
| `ros2` | 129358, 129882, 131029, 137803 | `ros2 launch nova …` (4 launchers + `topic hz`) | no |
| `robot_state_pub` | 129514 | `robot_state_publisher` | ✂ yes |
| `parameter_bridg` | 129519 | `ros_gz_bridge/parameter_bridge` | ✂ yes |
| `range_converter` | 129521 | `nova/range_converter` | exactly 15 |
| `async_slam_tool` | 129969 | `async_slam_toolbox_node` | ✂ yes |
| `ekf_node` | 131084 | `robot_localization/ekf_node` | no |
| `controller_serv` | 131086 | `nav2_controller/controller_server` | ✂ **yes — draft had this wrong** |
| `smoother_server` | 131090 | `nav2_smoother/smoother_server` | exactly 15 |
| `planner_server` | 131092 | `nav2_planner/planner_server` | no |
| `behavior_server` | 131094 | `nav2_behaviors/behavior_server` | exactly 15 |
| `bt_navigator` | 131096 | `nav2_bt_navigator/bt_navigator` | no |
| `waypoint_follow` | 131107 | `nav2_waypoint_follower/waypoint_follower` | ✂ yes |
| `velocity_smooth` | 131120 | `nav2_velocity_smoother/velocity_smoother` | ✂ yes |
| `lifecycle_manag` | 131124 | `nav2_lifecycle_manager/lifecycle_manager` | ✂ yes |
| `rviz2` | 129525 | `rviz2` — **GPU/OpenGL client** | no |

**Agents:** `claude` — 6 live processes (9607, 9729, 9904, 35263, 124206, +1).
The draft said "1 agent"; there are **six**.

**🚩 FLAGGED — real workloads hiding behind GENERIC names (§1a below):**

| comm | PID | What it actually is |
|---|---|---|
| `python3` | 129419 | **the ROS2 daemon** — `ros2cli.daemon.daemonize` |
| `ruby` | 129512, 129614, 129624 | **Ignition Gazebo** — launcher + `ign gazebo server` + `ign gazebo gui` (GPU client) |
| `sh` | 129511 | the `ign gazebo` launcher wrapper |
| `MainThread` | 5006, 5186, 5198, 5209, 9505, 27164, 36297 | **VS Code Server** node processes |
| `code-6a44c352bd` | ×2 | VS Code Server supervisor |

**Dev/host processes worth protecting:** `rustc` (×16 during a build), `cargo`,
`esbuild`, `node`, `terminator`, `Xorg`, `gnome-shell`, `chrome` (×18), `firefox`,
`docker`, plus the monitor itself (`edge` / `edge_monitor`).

#### §1a — 🚩 FLAG: `python3` and `ruby` CANNOT be cleanly protected by name

The dispatch asked me to flag any real workload that name/pattern matching can't
cleanly protect. **Two qualify, and one of them changes the target spec:**

- **`python3`** carries the ROS2 daemon. Allowlisting `python3` protects it — but it
  also blanket-protects *every* Python process on the host, including any future
  Python AI workload you might actually want the governor to manage. It is blunt,
  but it errs safe. **Accept the bluntness for this test.**
- **`ruby`** carries all three Gazebo processes. Same trade: allowlisting `ruby`
  protects the simulator and any other Ruby process. Also errs safe.
- **⛔ THE CONSEQUENCE FOR THE TARGET: the disposable MUST NOT be a bare `python3`.**
  Evaluation order is allowlist → blocklist → category
  ([`policy.rs:70-77`](../../src/governor/policy.rs#L70-L77)), so `"python3"` on
  BOTH lists means the allowlist silently wins. You would get the worst of both:
  a target that can never be killed (test proves nothing), and — if you "fixed" it
  by removing `python3` from the allowlist — **the ROS2 daemon becomes a kill
  candidate under the same name.** §3 gives a distinctly-named target that
  sidesteps this entirely.

---

### §2 — THE CONFIG BLOCK (exact keys, confirmed against the schema)

**⚠️ Schema correction to the dispatch:** the dispatch asked for one `[governor]`
block containing `allowlist`, `thermal_red_c`, `rate_limit_max_kills`,
`kill_sustain_secs`, `default_ai_action` and `auto_actuate`. **That is not the
schema.** Those keys live in **three different sections**. Putting them all under
`[governor]` fails silently — `Config` is `#[serde(default)]` with no
`deny_unknown_fields` ([`config.rs:27-28`](../../src/config.rs#L27-L28)), so the
misplaced keys are **ignored without error** and you would run on defaults while
believing you were protected. Actual homes:

| Key | Section | Cite |
|---|---|---|
| `auto_actuate`, `kill_sustain_secs` | `[governor]` | [`config.rs:145-173`](../../src/config.rs#L145-L173) |
| `allowlist`, `blocklist`, `default_ai_action`, `sigterm_grace_secs`, `rate_limit_max_kills`, `rate_limit_window_secs` | `[policy]` | [`config.rs:512-528`](../../src/config.rs#L512-L528) |
| `thermal_red_c`, `thermal_amber_c`, `vram_critical_pct`, `vram_attention_pct`, `ram_critical_pct`, `ram_attention_pct`, `alert_sustain_secs` | `[thresholds]` | [`config.rs:264-275`](../../src/config.rs#L264-L275) |
| `prometheus_bind` | `[telemetry]` | [`config.rs:328-330`](../../src/config.rs#L328-L330) |

**WRITE TO:** `/home/faiz/edge_monitor-l14/edge_monitor.toml` — first entry in the
discovery chain and the CWD of the running instance
([`onboarding.rs:63-70`](../../src/onboarding.rs#L63-L70)). It currently holds only
`[web] allow_no_auth = true`; **keep that block or the web server refuses to start**
([`config.rs:validate_web_auth`](../../src/config.rs#L727)) — and the web API is the
verification surface in §4.

```toml
# ═══════════════════════════════════════════════════════════════════
# SAFE-TEST CONFIG — governor live-fire, disposable target only.
# Written 2026-07-30. REVERT AFTER THE TEST (see §5).
# ═══════════════════════════════════════════════════════════════════

[web]
allow_no_auth = true          # KEEP — /api/settings is the verification surface

[governor]
# ⛔ THE ARMING SWITCH. Stays false until every check in §4 passes.
auto_actuate      = false
# LONG reaction window: 30 s of sustained breach before SIGTERM fires.
# Must be >= thresholds.alert_sustain_secs (5) or config load FAILS.
kill_sustain_secs = 30

[policy]
# ── THE PRIMARY STRUCTURAL GUARANTEE ──────────────────────────────
# "Allow" means policy.evaluate can return Kill for ONE reason only:
# a blocklist hit. Every other process on the host — AI-classified or
# not — returns Allow -> KillAction::Whitelisted -> structurally
# unreachable by the actuation loop (runtime.rs:2130-2134).
# DO NOT set this to "Kill" for this test.
default_ai_action = "Allow"

# ── THE ONLY KILLABLE NAME ON THE HOST ────────────────────────────
blocklist = ["vram_canary"]

# ── BELT-AND-BRACES: survives even if default_ai_action is flipped ─
# NOTE: this list REPLACES the built-in defaults, so the shells and
# init are re-listed explicitly. Exact /proc/<pid>/comm, 15-char max.
allowlist = [
    # -- shells + init (re-added: a present allowlist drops the defaults)
    "systemd", "init", "sshd", "bash", "zsh", "sh",
    "kworker", "kthreadd",
    # -- agents (6 live)
    "claude",
    # -- ROS2 / Nav2 / SLAM stack (15)
    "ros2", "robot_state_pub", "parameter_bridg", "range_converter",
    "async_slam_tool", "ekf_node", "controller_serv", "smoother_server",
    "planner_server", "behavior_server", "bt_navigator", "waypoint_follow",
    "velocity_smooth", "lifecycle_manag", "rviz2",
    # -- generic names carrying REAL workloads (see §1a FLAG)
    "python3",            # <- the ROS2 daemon (ros2cli.daemon)
    "ruby",               # <- ign gazebo server / gui / launcher
    # -- IDE + build toolchain
    "MainThread", "code-6a44c352bd", "node", "esbuild",
    "rustc", "cargo", "terminator",
    # -- desktop / GPU clients (hold real VRAM; see §2a)
    "Xorg", "gnome-shell", "chrome", "firefox",
    # -- containers
    "docker", "containerd",
    # -- the monitor itself
    "edge", "edge_monitor", "raqib",
]

sigterm_grace_secs     = 5    # SIGTERM -> SIGKILL delay (min 1)
rate_limit_max_kills   = 1    # ONE kill per window. One shot only.
rate_limit_window_secs = 60

[thresholds]
# ── THERMAL: raised to disable the system-wide mass-kill trigger ───
# Host CPU Package is at 93.0 C RIGHT NOW (confirmed live this
# session). Default thermal_red_c = 95.0 is ~2 C away. A thermal
# breach is HOST-WIDE: it makes EVERY AI-classified PID a candidate
# on the same tick (threshold_breach.rs:241-266, executor.rs:229).
# 120.0 puts it out of reach of any load spike. Must be > amber.
thermal_amber_c    = 85.0
thermal_red_c      = 120.0

# ── VRAM: tuned so the canary breaches and nothing else does ───────
# 15% of 12288 MB = 1843 MB. The canary holds ~2.5-3.0 GB (~23%).
# vram_attention_pct MUST be <= vram_critical_pct or startup FAILS
# (this is defect D1). Both must be in 0.0..=100.0.
vram_attention_pct = 12.0
vram_critical_pct  = 15.0

# ── RAM: untouched. No real workload is near 95% of 31960 MB;
#    the largest is claude at ~1.17%.
ram_attention_pct  = 90.0
ram_critical_pct   = 95.0

alert_sustain_secs = 5

[telemetry]
# Enables the decision counters used by the §4 dry-run gate.
# Default is empty = exporter DISABLED. Required for verification.
prometheus_bind = "127.0.0.1:9472"
```

#### §2a — Two accepted side-effects of the threshold tuning

1. **`vram_attention_pct = 12.0` will raise VRAM-pressure ALERTS** for any GPU client
   over ~1475 MB — Gazebo GUI, rviz2, Xorg, chrome. That is **alert noise, not
   danger**: they are allowlisted AND `default_ai_action = "Allow"`, so they can
   never reach `PolicyAction::Kill`. Expect the alerts panel to light up.
2. **`thermal_red_c = 120.0` disables the thermal RED alarm for the duration.** You
   lose the real 95 °C warning while the test runs. The host is at 93 °C. **Do not
   leave this in place after the test** — §5 reverts it.

---

### §3 — THE DISPOSABLE TARGET

**Requirements it satisfies:** distinctive 11-char `comm` (no collision with any of
the 445 live processes, and critically **not** `python3`); holds ~2.5 GB VRAM
(≈20-24% of 12288 MB, comfortably over the 15% cutoff); registers as a **CUDA
compute process** so NVML attributes the memory per-PID; holds steady well past the
30 s sustain window; loses nothing when killed.

**Why the name trick matters:** `comm` comes from the exec'd filename, so a **symlink**
to the interpreter renames the process while `sys.prefix` still resolves through the
symlink to the real stdlib. **I verified this live** — symlinked, launched, read
`/proc/<pid>/comm` → `vram_canary` (11 chars, untruncated), `sys.prefix` intact,
`torch.cuda.is_available()` → `True` on torch 2.5.1+cu124.

**Why per-PID VRAM works:** the sampler merges BOTH `running_graphics_processes()`
and `running_compute_processes()`, and torch's CUDA context lands in the compute list
([`gpu_nvidia.rs:166-215`](../../src/platform/gpu_nvidia.rs#L166-L215)). Note that
`nvidia-smi --query-compute-apps` is currently **empty** on this host — no compute
process is running yet. That is expected, but it means **per-PID VRAM attribution is
UNPROVEN on this host until the canary appears in `/api/snapshot` with a non-null
`vram_mb`.** §4 step 6 makes that an explicit gate: if `vram_mb` stays `null`, the
canary can never breach and the test would silently prove nothing.

```bash
# ── 1. create the distinctly-named interpreter (one time) ──────────
ln -sf "$(command -v python3)" /tmp/vram_canary

# ── 2. launch the canary: grabs ~2.5 GB VRAM and holds it ──────────
/tmp/vram_canary -c "
import torch, time, os
torch.cuda.init()
# 2.5 GiB of float32 on the device, kept referenced so it is never freed
hog = torch.empty(int(2.5*1024**3//4), dtype=torch.float32, device='cuda')
torch.cuda.synchronize()
print('canary pid', os.getpid(),
      'holding', torch.cuda.memory_allocated()//1024**2, 'MiB', flush=True)
while True:            # hold steady, ~0% CPU
    time.sleep(5)
" &
echo $! > /tmp/vram_canary.pid

# ── 3. confirm identity + attribution BEFORE relying on it ─────────
cat /proc/$(cat /tmp/vram_canary.pid)/comm     # MUST print exactly: vram_canary
nvidia-smi --query-compute-apps=pid,used_memory --format=csv   # MUST list that PID
```

**Cleanup (it is disposable — kill it any time):**
`kill $(cat /tmp/vram_canary.pid) 2>/dev/null; rm -f /tmp/vram_canary /tmp/vram_canary.pid`

---

### §4 — PRE-ARM VERIFICATION CHECKLIST (all 7 must pass; `auto_actuate` stays `false` throughout)

The whole point: prove the governor would target **only** `vram_canary` while the
killer is still structurally disabled by Gate 1
([`runtime.rs:2111`](../../src/runtime.rs#L2111)).

**1 — Build, then launch capturing the PID.** Do not skip the PID capture; §0/D3 is
exactly what happens without it.
```bash
cd /home/faiz/edge_monitor-l14 && cargo build --release
./target/release/edge_monitor > /tmp/raqib_test.log 2>&1 &
echo $! > /tmp/raqib_test.pid
```

**2 — Prove the config was actually READ.** The currently-running instance reports
`"config_path": null` — it loaded **no config file at all**. Editing a TOML the
binary never reads is the quietest possible failure mode.
```bash
curl -s http://127.0.0.1:7070/api/settings | python3 -m json.tool
```
✅ PASS requires ALL of: `thermal_red_c: 120.0`, `vram_critical_pct: 15.0`,
`vram_attention_pct: 12.0`, `kill_sustain_secs: 30`,
`auto_actuate_readonly: false`, `default_ai_action_readonly: "Allow"`.
❌ If any value still shows the default (95.0 / 85.0 / 10) → the config was NOT
loaded. **STOP.** Fix the path before going further.

**3 — Confirm the killer is still disabled.** `auto_actuate_readonly` must be
`false`. Everything below runs with the actuation loop structurally off.

**4 — Allowlist coverage sweep — use `ps`, NOT `/api/snapshot`.** The draft's
one-liner read `/api/snapshot`, which only lists the 21 AI-classified workloads and
therefore **misses `rviz2`, `ruby`/Gazebo and the `python3` ROS2 daemon entirely**.
Sweep every process on the host instead:
```bash
python3 - <<'EOF'
import subprocess
allow = {"systemd","init","sshd","bash","zsh","sh","kworker","kthreadd","claude",
 "ros2","robot_state_pub","parameter_bridg","range_converter","async_slam_tool",
 "ekf_node","controller_serv","smoother_server","planner_server","behavior_server",
 "bt_navigator","waypoint_follow","velocity_smooth","lifecycle_manag","rviz2",
 "python3","ruby","MainThread","code-6a44c352bd","node","esbuild","rustc","cargo",
 "terminator","Xorg","gnome-shell","chrome","firefox","docker","containerd",
 "edge","edge_monitor","raqib"}
live = subprocess.run(["ps","-eo","comm="],capture_output=True,text=True).stdout.split()
missing = sorted(set(live) - allow)
print("UNLISTED (%d distinct):" % len(missing))
for m in missing: print("  ", m)
print("\nvram_canary in allowlist?", "vram_canary" in allow, "<- MUST be False")
EOF
```
✅ PASS: `vram_canary` is **not** in the allowlist, and you have eyeballed the
UNLISTED names and confirmed none is a workload you care about. Unlisted is *not*
dangerous while `default_ai_action = "Allow"` — this step is to protect you if the
setting is ever flipped.

**5 — Baseline the decision counters** (exporter from `[telemetry]`):
```bash
curl -s http://127.0.0.1:9472/metrics | grep governor_kills_total
```
✅ PASS: `reason="whitelisted"` is large and climbing (~one per process per tick);
`reason="sigterm"` is **absent or flat**. A non-zero, *climbing* `sigterm` here —
before the canary exists — means something real is already a kill candidate. **STOP.**

**6 — Launch the canary (§3) and prove VRAM attribution works.**
```bash
curl -s http://127.0.0.1:7070/api/snapshot \
 | python3 -c "import json,sys; [print(w['pid'],w['name'],w['vram_mb']) for w in json.load(sys.stdin)['workloads'] if 'canary' in w['name']]"
```
✅ PASS: a row appears with `vram_mb` ≈ 2500-3000 (**not `null`**).
❌ `null` → NVML is not attributing per-process VRAM on this host. The canary can
never breach; the test proves nothing. **STOP** and fix attribution first.

**7 — THE DECISIVE GATE: count how many PIDs are kill candidates.** Sample the
counter twice, 20 s apart, while the canary is breaching:
```bash
A=$(curl -s http://127.0.0.1:9472/metrics | grep 'governor_kills_total{reason="sigterm"}' | awk '{print $2}')
sleep 20
B=$(curl -s http://127.0.0.1:9472/metrics | grep 'governor_kills_total{reason="sigterm"}' | awk '{print $2}')
echo "delta=$(( ${B:-0} - ${A:-0} ))  over ~20 ticks (1 Hz)"
```
✅ **PASS: delta ≈ 20** — i.e. **exactly ONE** `SignalTermSent` decision per tick.
That one decision is the canary.
❌ **delta ≈ 40 (or any multiple)** → **TWO OR MORE PIDs are kill candidates.**
**ABORT. DO NOT ARM.** Something besides the canary is reaching `PolicyAction::Kill`.
❌ **delta = 0** → the canary is not breaching (check step 6, and that 30 s sustain
has elapsed). Arming would prove nothing.

Also confirm the canary is **still alive** at this point — that is Gate 1 doing its
job with `auto_actuate = false`.

**ONLY IF ALL SEVEN PASS:** set `auto_actuate = true` in `[governor]`, then **restart**
(§5 — config is not hot-reloaded). Re-check `/api/settings` shows
`auto_actuate_readonly: true`, and watch `/tmp/raqib_test.log` for
`auto_actuate: firing SIGTERM`.

---

### §5 — ABORT + DISARM

**Config is NOT hot-reloaded** — no inotify, no SIGHUP path. Editing the TOML does
nothing until restart. The only instant stop is killing the process.

**🔴 INSTANT ABORT (use this the moment anything looks wrong):**
```bash
kill $(cat /tmp/raqib_test.pid)
```
This stops the governor immediately. Because `execute_after_grace` runs **in-process**
([`executor.rs:481-488`](../../src/governor/executor.rs#L481-L488)), killing the
monitor during the 5 s SIGTERM grace **also cancels the pending SIGKILL escalation**.

**If the PID file is lost** — do **NOT** use `pgrep -f "target/release/edge_monitor"`
(defect D3: it matches `rustc` build jobs and misses the real process). Use one of:
```bash
fuser -k -n tcp 7070          # kills whatever holds the web port — most reliable
pgrep -x edge_monitor         # exact comm match
pgrep -x edge                 # the currently-running instance's comm is 'edge'
ss -lntp | grep 7070          # read the PID out directly
```

**Confirm the killer is OFF:**
```bash
curl -s -m 2 http://127.0.0.1:7070/api/settings   # connection refused == dead
pgrep -x edge_monitor; pgrep -x edge; echo "exit=$? (1 == nothing running)"
```

**Full disarm (do this after the test — do not leave the tuned thresholds in place):**
1. `kill $(cat /tmp/raqib_test.pid)`
2. Edit `edge_monitor.toml`: `auto_actuate = false`; revert `thermal_red_c = 95.0`;
   revert `vram_attention_pct = 85.0` / `vram_critical_pct = 95.0`; empty the
   `blocklist`. **Re-arming the 95 °C thermal alarm is the important one** — the host
   idles at 93 °C.
3. Kill the canary: `kill $(cat /tmp/vram_canary.pid); rm -f /tmp/vram_canary /tmp/vram_canary.pid`
4. Restart and confirm: `/api/settings` shows `auto_actuate_readonly: false`,
   `thermal_red_c: 95.0`, `vram_critical_pct: 95.0`.

**Two notes on things that will NOT disarm it:**
- The web UI **cannot** flip `auto_actuate` — it is schema-firewalled out of
  `/api/settings` POST ([`runtime.rs:707,724`](../../src/runtime.rs#L707)). The wire
  can neither arm nor disarm the killer. Console only.
- The `GovernorArmed` alert is **unrelated** to `auto_actuate` — it tracks the TUI's
  manual kill-confirm card ([`runtime.rs:508-517,752-763`](../../src/runtime.rs#L508-L517))
  and is always Idle in headless mode. **Do not read it as an arming indicator.**

---

### §6 — Current host state (measured this session, for the record)

- **21 AI-classified workloads**, every one `AICategory::Inference`: 6 × `claude`,
  15 × ROS2/Nav2. Plus Gazebo (×3 `ruby`), `rviz2`, and the ROS2 daemon (`python3`)
  which the wire does not list as workloads.
- **CPU Package: 93.0 °C** (amber). Default `thermal_red_c` is 95.0 — **2 °C of
  headroom**. This is the single biggest reason the draft config was unsafe.
- GPU: RTX 3060, **12288 MB** total, 1087-2198 MB in use (graphics only —
  `--query-compute-apps` is empty). Every workload reports `vram_mb: null`.
- RAM: 31960 MB total, 33.7% used. Largest workload ~1.17%. Nowhere near 95%.
- Running monitor: PID 123212, `comm`/`argv[0]` = **`edge`**, exe
  `target/release/edge_monitor (deleted)` — an older build, rebuilt underneath it.
  Reports `auto_actuate_readonly: false` and `config_path: null`, i.e. **currently
  safe and running on built-in defaults.**

**HARD STOP #1 and #5 both intact.** No governor code read-modified, no config
written, no arming performed. This is READING the governor and WRITING a document —
both permitted. **The operator reviews all of the above before applying anything.**

---

## [RESOLVED 2026-07-30] Rename `edge_monitor` → `raqib` — CAR landed, decisions in, atomic commit shipped

* **CAR resolved**: `../ux_contract` is at v0.3.22 (past the promised v0.3.17 bump). The 3 strings — `errors::TERMINAL_TOO_SMALL`, `help::TITLE`, `mission::TEMPLATE` — now say "raqib"; the test at contract line 1421 asserts the new template verbatim. Consumer's path-dep picks it up automatically (constraint `>=0.3.14` is satisfied by 0.3.22).
* **STOP #3 (Prometheus prefix)**: **KEPT `edge_monitor_*`** per operator decision + inspector lean (option a). Doc-note added at the top of `src/telemetry/exporter.rs` calling the prefix an EXTERNAL CONTRACT, pinning the decision so a future contributor doesn't "clean it up" and break every downstream Grafana dashboard + alerting rule. Coordinated migration deferred.
* **Backward-compat**: raqib.toml first, then legacy `edge_monitor.toml` fallback for one release with a `tracing::warn!("loading LEGACY edge_monitor.toml (raqib rename) — run raqib init to migrate ...")`. Live-verified: an existing operator with `~/.config/edge_monitor/edge_monitor.toml` still boots cleanly on the raqib binary; the warn gives them a migration cue.
* **Governor gate held**: `git diff HEAD src/governor/` after the rename touched exactly 4 lines — all doc-comment string edits (`edge_monitor.toml` → `raqib.toml` at 3 sites). Zero logic. HARD STOP #1 boundary intact.
* **Landing**: single atomic commit on top of the header web-link. Binary `target/release/raqib` (11.2 MB). Library crate name stays `edge_monitor` so ~30 `use edge_monitor::…` imports across the tree don't churn.
* **Tests + gate**: 1256 → 1259 (+3 onboarding legacy-fallback tests). Browser gate unchanged at 269/0. clippy clean.
* **Original STOP block preserved below for the discovery-stage record — no active work item remains.**

### Original STOP block (archived) — 2026-07-30

**What I was asked to do:** rename the project's user-facing identity from `edge_monitor` to `raqib` — binary name, config file/paths, log paths, output strings, TUI+web parity. Dispatch is explicit: "cosmetic rename — NO governor logic change, NO behavior change, NO contract semantics change."

**Why I stopped BEFORE landing anything:**

Three user-facing strings that must change to complete the rename live in `../ux_contract`. HARD STOP #2 (contract change needed) fires cleanly. CLAUDE.md says stop and file a CAR — I stopped.

### The blocking strings (contract crate — I cannot edit them)

All in `/home/faiz/ux_contract/src/lib.rs`:

| Line | Constant | Current text |
|---|---|---|
| 561 | `errors::TERMINAL_TOO_SMALL` | `"edge_monitor needs at least 80×24 terminal.\nCurrent size: {w}×{h}. Resize and press any key."` |
| 574 | `help::TITLE` | `"edge_monitor — keyboard reference"` |
| 852 | `mission::TEMPLATE` | `"edge_monitor · {n} workloads · {m} degraded · press ? for help"` |
| 1421 | *(test asserting the exact mission template — moves with 852)* | mirror of 852 |

Additional doc-comment mentions of `edge_monitor` in `../ux_contract/src/lib.rs:483,486,503`, `activity.rs:1`, `host_vitals.rs:211`, `recommendation.rs:325`. Doc-only, but a consistent rename should sweep them too.

### 📋 Contract Amendment Request (CAR) — routed via Agent A

**Bump:** ux_contract 0.3.16 → 0.3.17 (breaking string content, additive shape). Tag AND push (per the no-orphan-tag rule).

**Rename these constants' CONTENTS (identifiers stay stable):**

```rust
// errors::TERMINAL_TOO_SMALL
"raqib needs at least 80×24 terminal.\nCurrent size: {w}×{h}. Resize and press any key."

// help::TITLE
"raqib — keyboard reference"

// mission::TEMPLATE
"raqib · {n} workloads · {m} degraded · press ? for help"
```

The **placeholders** (`{n}`, `{m}`, `{w}`, `{h}`) stay verbatim — those are contract semantics and my consumer-side substitution code depends on them.

**Test at line 1421** — the exact-string test:
```rust
assert_eq!(
    text,
    "raqib · 3 workloads · 1 degraded · press ? for help"  // was "edge_monitor · …"
);
```

**Doc-comment mentions of `edge_monitor`** — recommend leaving as-is (they document the consumer-side history, not user-facing text). Or replace with `raqib` for a full sweep. Judgment call for Agent A.

### 🛑 [STOP #3] Prometheus metric prefix — DECISION NEEDED

`src/telemetry/exporter.rs` emits ~14 Prometheus metrics prefixed `edge_monitor_*` (`edge_monitor_processes_total`, `edge_monitor_gpu_watts`, `edge_monitor_governor_kills_total`, `edge_monitor_tick_count_total`, …). These are the metric identifiers external scrapers (Prometheus, Grafana, alerting rules) read by NAME.

Renaming them from `edge_monitor_*` to `raqib_*` is a **breaking change for any external monitoring setup** — dashboards go blank, alerts stop firing until every downstream config is updated in lockstep. The dispatch HARD RULE 2 says "NO behavior change" and HARD RULE 4 says "no contract touch"; Prometheus metric names arguably are both external contract AND behavior.

**Materially different approaches — dispatch does not settle this:**

- **(a)** **KEEP `edge_monitor_*` prefix, document as external contract.** No external breakage; the metric names become an internal-vs-external asymmetry (binary is `raqib`, metrics still say `edge_monitor_`). Add a short block in the exporter's module doc pinning the decision so a future contributor doesn't "clean it up" accidentally.
- **(b)** **RENAME to `raqib_*`.** Clean sweep, but a breaking change for any external monitoring config (Grafana dashboards, alerting rules, `prometheus.yml` scrape configs). Requires a coordinated release note + probably a deprecation window emitting BOTH prefixes.
- **(c)** **Hybrid — emit both prefixes for one release**, then drop `edge_monitor_*` in the next minor. Doubles metric-emission cost per scrape (~2x line count in the /metrics response) but gives operators time to migrate.

**Inspector lean: (a).** Prometheus metric names ARE external contract; renaming without coordination is exactly the "breaking change" the CLAUDE.md HARD RULE 2 forbids. The internal-vs-external asymmetry is honest and documented. Operator picks.

### What I DID before stopping (reversible with `git reset` — nothing committed)

Nothing. Working tree clean. No files touched. I stopped at the discovery stage per CLAUDE.md HARD STOP protocol.

### What's safe to do meanwhile

Nothing on this rename until the CAR + STOP #3 decision route. Every other autonomous item is human/hardware-blocked per the 2026-07-16 EXIT summary above. No hot HARD STOPs to prevent OTHER work — the loop can hit EXIT again if there's nothing else.

### What I need from you

1. **Route the CAR** (or edit `../ux_contract` yourself, bump to 0.3.17, tag `v0.3.17`, push tag + commit). Then I can pick up the new symbols on next `cargo build`.
2. **Decide (a) / (b) / (c) for the Prometheus prefix.** My lean is (a) — keep as `edge_monitor_*` with a doc-note.
3. **Confirm the backward-compat plan for config discovery**: dispatch RECOMMENDS reading `raqib.toml` first + falling back to `edge_monitor.toml` for one version with a deprecation log-line. My lean matches: **yes to fallback**. Existing users (including you) don't have to re-init on upgrade. Confirm or override.

### Post-decision landing plan (once you route)

1. Cargo.toml — add `[[bin]] name = "raqib"` (keep package `name = "edge_monitor"` internally so the library crate compiles under the same identifier and lib consumers don't have to churn; the binary target is what users see). Verify `cargo build --release` produces `target/release/raqib`.
2. `src/onboarding.rs` — update discovery paths (`./raqib.toml`, `~/.config/raqib/raqib.toml`, `/etc/raqib/raqib.toml`), init target path, error message, DEFAULT_CONFIG_TOML header comment. **Add fallback:** if none of the new paths hit, try the equivalent `edge_monitor.toml` paths + emit `tracing::warn!("config found at legacy edge_monitor.toml — run `raqib init` to migrate")`.
3. `src/config.rs` — `~/.local/share/edge_monitor` → `~/.local/share/raqib`, `~/.cache/edge_monitor/fingerprints.json` → `~/.cache/raqib/fingerprints.json`. Consider fallback reads at the old paths too.
4. `src/main.rs` — clap `name = "edge_monitor"` → `"raqib"`, doc-comment references to the config file, `use edge_monitor::…` stays (internal lib import — no user impact).
5. `src/ui/panels/help.rs` — the two `edge_monitor exec`/`edge_monitor.toml` references in help text → `raqib`.
6. `src/exec_wrapper.rs` — 6 `eprintln!("edge_monitor: …")` lines → `raqib:`.
7. `src/history/cli.rs` — 4 `edge_monitor history`/`edge_monitor exec` references in output → `raqib`.
8. `src/compare.rs`, `src/storage/run_store.rs` — same, user-facing error text.
9. `src/web/handlers.rs` — `<title>edge_monitor — frontend not built</title>` + `<h1>` → `raqib`.
10. `src/telemetry/dispatcher.rs:80` — `.thread_name("edge_monitor-telemetry")` — thread name shows in `htop`/`ps`. Rename to `raqib-telemetry` (user-observable via process inspection).
11. `src/ui/panels/top_processes.rs` — the self-exclusion tests reference `"edge_monitor"` as the process name; after the binary rename, the runtime's self-exclusion needs to match the new binary name. Update tests + the self-exclusion filter (`proc_full(42, "edge_monitor", …)` → `"raqib"`).
12. Prometheus prefix — apply (a) / (b) / (c) per your decision.
13. Update tests that assert the exact mission string (currently ~4 sites: header.rs unit tests, ux_contract's own test at 1421 — moves with the contract).
14. Full gate: `cargo test --workspace --release` (target 1256), `cargo clippy --workspace --all-targets -- -D warnings`, `npm --prefix web run test:browser` (target 269).
15. Update BOARD.md + JOURNAL.md with the rename landing.
16. Governor diff verification: `git diff HEAD src/governor/` should touch ZERO logic — only the string comments at `policy.rs:57-58,116` mentioning `edge_monitor.toml` → `raqib.toml`. Confirmed at CLAUDE.md HARD STOP #1 gate.

**Estimated LoC (post-CAR):** ~30–50 line edits across ~15 files. Atomic single commit. No breakage of gates.

---

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
