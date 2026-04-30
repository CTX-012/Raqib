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
# Build release binary (~2.6 MB stripped)
cargo build --release

# Headless smoke test — two ticks, dry-run default, logs to stderr
./target/release/edge_monitor --no-ui --ticks 2

# Interactive TUI
./target/release/edge_monitor

# Point at a custom config
./target/release/edge_monitor --config ./edge_monitor.toml
```

See [`edge_monitor.toml.example`](edge_monitor.toml.example) for a
commented config file.

### Opening the dashboard from the TUI (`g` keybinding)

Pressing `g` on a focused AI workload row asks the OS to open the
configured dashboard URL (`[dashboard].url_template` in the config,
or the `EDGE_MONITOR_GRAFANA_URL` env var, or a `localhost:3000`
fallback) in your default browser. On Linux this shells out to
`xdg-open`; on a vanilla WSL install neither `xdg-open` nor a browser
association exists by default, so `g` will surface
`Could not open browser — URL: <url>` in the status footer. Two
fixes:

```bash
# Option A — install wslu, which provides wslview as an xdg-open
# shim that hands the URL to your Windows-side default browser.
sudo apt install wslu
sudo update-alternatives --install /usr/bin/xdg-open xdg-open \
  "$(command -v wslview)" 100

# Option B — set $BROWSER to any opener you have on PATH; xdg-open
# (when present) honours it, and you can copy the URL from the
# status footer in the meantime.
export BROWSER='/mnt/c/Windows/explorer.exe'
```

### CLI flags

| Flag                  | Effect                                                       |
| --------------------- | ------------------------------------------------------------ |
| `--config <PATH>`     | Load TOML config (default: `./edge_monitor.toml` if present) |
| `--dry-run`           | Force dry-run regardless of config — no kill signals sent    |
| `--no-ui`             | Run headless; log to stderr only                             |
| `--ticks <N>`         | Exit after N ticks (`0` = run until killed). Useful in CI    |
| `--log-level <LEVEL>` | `trace` / `debug` / `info` / `warn` / `error`                |
| `--log-format <FMT>`  | `human` (default K=V text) or `json` (one JSON object per line, `jq`-pipeable) |

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
- Governor: allowlist, dry-run default, SIGTERM→grace→SIGKILL, audit
  log, rate limit
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
