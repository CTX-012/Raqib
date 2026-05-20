# edge_monitor

**Model-aware resource monitor and governor for edge AI workloads.**

On a shared edge box running ROS + YOLO + a local LLM, `edge_monitor`
sees which model each Python process is running and kills the offender
before the kernel OOM-killer takes the whole robot stack down.

Target platforms: Ubuntu 22.04+ x86_64 and NVIDIA Jetson (JetPack 6).
Windows is out of scope.

> **Status — 2026-04-24:** Phase 0 complete. 150 tests pass on WSL
> Ubuntu. Pre-launch (Phase 1) work in progress: demo recording on
> Jetson Orin, CI, `.deb` packaging. No stable release yet; API may
> still shift before `v0.1.0` is tagged.

## Why not `htop` / `nvtop` / `jtop`

| Tool                      | What it misses                                   |
| ------------------------- | ------------------------------------------------ |
| `htop` / `btop`           | Shows `python` — no idea what model is loaded   |
| `nvtop` / `nvidia-smi`    | VRAM per PID, but no model name, no governor    |
| `jtop`                    | Jetson-specific, no model awareness, no governor |
| `systemd-oomd` / earlyoom | Generic memory killer, zero model awareness      |

Nobody combines **model identification + resource attribution + safe
governor + edge-first**. That's the gap `edge_monitor` fills.

## Quick start

```bash
# Build the Svelte web companion first so the Rust binary can
# embed the compiled assets. Skip this if you only want the TUI
# (the binary still ships a "frontend not built" placeholder page
# in that case).
cd web
npm install
npm run build
cd ..

# Build release binary (~2.6 MB stripped)
cargo build --release

# Headless smoke test — two ticks, logs to stderr
./target/release/edge_monitor --no-ui --ticks 2

# Interactive TUI + web dashboard on http://<host>:7070
# (defaults to 0.0.0.0 bind — see "Web UI security" below)
./target/release/edge_monitor

# Disable the web companion (TUI-only)
./target/release/edge_monitor --no-web

# Override the web port
./target/release/edge_monitor --port 8080

# Restrict web to localhost-only (recommended on untrusted networks)
./target/release/edge_monitor --bind 127.0.0.1

# Point at a custom config
./target/release/edge_monitor --config ./edge_monitor.toml
```

The web UI is **read-only** for v1.0 — kill confirmation, theme
selection, and navigation stay TUI-only. The TUI is the
authoritative control surface; the web dashboard is for at-a-glance
monitoring from a browser tab.

### Web UI security

**v1.0 has no authentication on the web companion.** The default
bind address is `0.0.0.0:7070`, which means the dashboard is
reachable from any host on the same LAN. The design assumes a
**trusted LAN** — workstation, lab network, robot dev fleet on a
private subnet. Do not expose the binary directly to the
internet.

If the host runs on an untrusted network (shared coworking, hotel
Wi-Fi, cloud VM with a public IP), restrict the listener to
localhost only:

```bash
edge_monitor --bind 127.0.0.1
```

A future release will add auth (token / mTLS) so the wider bind
is safe by default; until then, treat the open listener like any
other unauthenticated dashboard (Grafana on `:3000`, Prometheus
on `:9090`, etc.) and put a reverse proxy in front of it if you
need network access with auth.

See [`edge_monitor.toml.example`](edge_monitor.toml.example) for a
commented config file.

### CLI flags

| Flag                  | Effect                                                       |
| --------------------- | ------------------------------------------------------------ |
| `--config <PATH>`     | Load TOML config (default: `./edge_monitor.toml` if present) |
| `--no-ui`             | Run headless; log to stderr only                             |
| `--ticks <N>`         | Exit after N ticks (`0` = run until killed). Useful in CI    |
| `--log-level <LEVEL>` | `trace` / `debug` / `info` / `warn` / `error`                |
| `--log-format <FMT>`  | `human` (default K=V text) or `json` (one JSON object per line, `jq`-pipeable) |
| `--log-stderr`        | Force tracing to stderr while running the TUI (default: write to log file) |
| `--theme <NAME>`      | `dark` (default), `light`, or `high-contrast` (UX_CONTRACT.md §13) |
| `--no-web`            | Disable the embedded web companion                           |
| `--port <N>`          | Web companion listen port (default `7070`)                   |
| `--bind <IP>`         | Web companion listen address (default `0.0.0.0`, LAN-accessible — see "Web UI security") |

Logs default to `~/.cache/edge_monitor/edge_monitor.log` when running
the dashboard so tracing output cannot bleed into the alternate-screen
TUI. `--no-ui` and the subcommands keep using stderr; pass
`--log-stderr` to opt back into stderr while the TUI is active.

## History

Every AI process exit is appended to a typed run store under
`~/.local/share/edge_monitor`. Inspect it with the `history`
subcommand:

```bash
# Summary table — one row per known model.
edge_monitor history

# Recent runs of one specific model.
edge_monitor history phi3-mini

# Bigger window, JSON output for scripting.
edge_monitor history phi3-mini --limit 50 --json
```

Sample output:

```
$ edge_monitor history phi3-mini
TIMESTAMP             UPTIME  EXIT  CPU%avg  RSSpk_MB  VRAMpk_MB  TPSavg
2026-04-28 09:12:04   00:03:42  ok      48.1      1812       4096    36.7
2026-04-28 08:14:53   00:00:11  ok      63.2      1804       4096    34.9
2026-04-27 22:58:11   00:08:01  ok      45.0      1820       4096    37.4
```

The store path, retention cap, and regression-detector thresholds are
configurable — see [docs/configuration.md](docs/configuration.md)
sections `[storage]` and `[regression]`.

## Safety defaults

1. **Dry-run is the default.** No kill signals are sent unless
   `policy.enforce = true` in config.
2. **Allowlist is honored first.** Allowlisted processes are never
   killed by automated policy.
3. **SIGTERM before SIGKILL.** Configurable grace period (default 5s,
   minimum 1s).
4. **Rate limit.** At most 3 automated kills per 60s window.
5. **Every kill is audited** — PID, name, model, reason, timestamp,
   policy snapshot.
6. **Manual kill respects allowlist** with an explicit override
   confirm in the TUI.

## Architecture (one tick)

```
Platform → Classifier → Lifecycle → Governor → UI
 (/proc,    (annotate    (track    (decide    (render)
  sysinfo,   with model   spawns    SIGTERM/
  NVML)      + category)  & exits)  SIGKILL,
                                    audit)
```

Default tick rate: 1 Hz. TUI renders at 10 Hz with cached data between
samples. The tick loop uses `event::poll` / `recv_timeout` — never
`std::thread::sleep`.

## Scope

### In scope (v1, Phase 0 — complete)

- `/proc` + `sysinfo` process sampling, CPU%, RSS, global network
- NVML for GPU utilization, VRAM, per-process VRAM (graceful fallback
  when no NVIDIA GPU)
- Classifier: cmdline / env / script-sniff model identification;
  Inference / Training / ModelDownload / Framework categories
- Governor: allowlist, kill_confirm card (CAR-17), SIGTERM→grace
  →SIGKILL, audit log, rate limit
- Manual kill by serial ID (allowlist-respecting)
- Process run summary on termination
- ratatui TUI

### Out of scope (deferred to Phase 2+)

tegrastats, thermal zones, ROS2 node detection, Prometheus exporter,
OOM post-mortem, Intel NPU, AMD ROCm, Hailo, web UI, Windows support,
cgroup enforcement, rosbag correlation.

See [HANDOFF.md](HANDOFF.md) for the phase map.

## Development

```bash
cargo test                                   # 150 tests
cargo clippy --all-targets -- -D warnings    # must be clean
cargo build --release
```

See [src/CLAUDE.md](src/CLAUDE.md) for coding conventions and safety
rules that must not be violated when extending the code.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Contributions are accepted under the same dual license.
