# edge_monitor — Build Handoff

> This is the operational build plan. Phase breakdown, module-by-module
> scope, acceptance criteria, test requirements. Read this before every
> Claude Code session. Do not skip ahead. Do not parallelize modules.
>
> For the *why* (audience, wedge, positioning), read [VISION.md](VISION.md).
> For day-to-day conventions, read [CLAUDE.md](CLAUDE.md).

---

## Phase map

| Phase | Goal                                                      | Rough duration |
|-------|-----------------------------------------------------------|----------------|
| 0     | v1 Linux build — 8 modules                                | 6–8 focused weeks |
| 1     | v1 launch — demo, README, Show HN                         | 1–2 weeks |
| 2     | v1.1 features — tegrastats, thermal, ROS2 awareness, Prometheus | 6–10 weeks |
| 3     | v2 expansion — Intel NPU, AMD, OOM post-mortem            | demand-driven |
| 4     | Windows revival (maybe, only if community demand)         | n/a or deferred |

Durations are "focused hours" at ~10 hrs/week. Calendar time is typically 2×.

## Phase 0 — v1 Linux build — ✅ complete

The 8 modules, in strict order. Each module's tests must be green before
the next starts.

- ✅ Module 1 — Classifier (pure logic, no hardware)
- ✅ Module 2 — Platform layer (`/proc` + sysinfo)
- ✅ Module 3 — NVML GPU backend
- ✅ Module 4 — Lifecycle + run summaries (+ resource accumulation)
- ✅ Module 5 — Governor (dry-run first) (+ 3-kills-per-60s rate limit)
- ✅ Module 6 — Manual kill wiring
- ✅ Module 7 — ratatui TUI
- ✅ Module 8 — main.rs wiring + CLI + config + signal handling

Phase 0 acceptance gates:

- All tests green (`cargo test`)
- `cargo clippy --all-targets -- -D warnings` clean
- No `unwrap()` or `expect()` outside tests
- Release binary <15 MB (currently ~2.6 MB)
- `--dry-run` overrides config and is the default
- Persistent JSONL audit log for governor decisions
- Persistent JSONL run-summary log for completed processes

## Phase 1 — v1 launch

Pre-launch checklist:

- [x] All 8 modules' tests green
- [x] `cargo clippy -- -D warnings` clean
- [ ] `cargo audit` clean (no known vulnerabilities)
- [x] Binary size <15 MB release build
- [x] README.md with demo GIF above the fold (GIF pending capture on Orin)
- [x] `edge_monitor.toml.example` with commented config
- [x] `--help` covers all flags (clap-generated)
- [x] CHANGELOG.md started
- [x] LICENSE-MIT + LICENSE-APACHE
- [x] GitHub Actions CI: build, test, clippy, audit on every push
- [x] `.deb` release artifact via `cargo-deb` (metadata configured)
- [ ] `cargo install edge_monitor` works (verify name on crates.io)

Launch sequence (week 1):

- Day 1: tag v0.1.0, publish release with binaries + .deb
- Day 2: README polish, demo GIF record on Jetson Orin
- Day 3: Show HN post ("edge_monitor — a model-aware resource governor
  for edge AI")
- Day 4: r/ROS, r/robotics, r/rust posts (staggered, not same day)
- Day 5: ROS Discourse, NVIDIA Jetson forums
- Days 6–30: respond to every issue within 48 hours, every PR within 72

## Phase 2 — v1.1 features (post-launch, demand-driven)

Priorities ordered by pre-emptive guess at demand; reorder based on
real issues filed:

1. **tegrastats integration** — Jetson users will ask immediately
2. **Thermal + throttle detection** — paired with tegrastats
3. **ROS2 node name detection** — parsing `__node:=` from cmdline
4. **Prometheus exporter** — lets people pipe to Grafana
5. **OOM post-mortem** — parse `dmesg` / `journalctl -k` for recent OOM
6. **Model inventory pre-flight** — "will your models fit on this box?"
7. **Config hot-reload** on SIGHUP
8. **Ollama model-name extraction** — hit `http://localhost:11434/api/ps`
   to name the model actively being served

Each becomes its own 1–2 week mini-phase with its own acceptance criteria.

## Phase 3 — v2 expansion

- Intel NPU support (verify `intel_vpu` driver maturity first)
- AMD ROCm support
- Hailo support (self-dogfood via NOVA)
- Coral TPU, RK3588 NPU (niche, only if requested)
- Web UI / remote view (only if requested loudly)
- cgroup v2-based enforcement (lets kernel do the killing, safer)
- rosbag time correlation

## Phase 4 — Windows revival

**Only if:** a community maintainer volunteers to own it. Otherwise the
legacy Windows prototype stays archived with a note pointing to Linux.

## Testing strategy (applies to all phases)

- **Unit tests** for every pure-logic function
- **Integration tests** in `tests/` for cross-module behavior
- **Property tests** (via `proptest`) for classifier and governor
  decision logic — the places correctness matters most
- **Manual hardware tests** on Jetson Orin before every release tag
- **No feature merged without tests.**

## Release cadence

- v0.1.x (Phase 0 → 1): initial launch
- v0.2 every 6 weeks during Phase 2 if scope permits
- v1.0 declared when: 6 months post-launch, 3 consecutive releases with
  no critical bugs, at least one external contributor
- Semver strict after v1.0

## What a contributor sees

When someone clones the repo, they see:

```
edge_monitor/
├── README.md         ← demo GIF, install, one-page pitch
├── VISION.md         ← the why
├── HANDOFF.md        ← this file
├── CLAUDE.md         ← conventions, current scope
├── CHANGELOG.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── Cargo.toml
├── edge_monitor.toml.example
├── src/
├── tests/
├── .github/workflows/ci.yml
└── docs/
    ├── architecture.md
    ├── configuration.md
    └── faq.md
```

## Meta: using this doc

- Update `HANDOFF.md` when a module's scope changes
- Update `VISION.md` when audience or wedge changes
- Update `CLAUDE.md` when coding conventions change
- If you find yourself in chat re-scoping something, the answer is either
  "check this doc" or "this doc needs updating" — not "let me re-explain"
