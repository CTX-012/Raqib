# edge_monitor — Vision

> This document holds the long-term thinking. It exists so the "why" never
> gets re-litigated every three weeks. If a decision here changes, update
> this file before changing code. If the code drifts from this doc, one of
> them is wrong — figure out which.

---

## What this tool is

A **model-aware resource monitor and governor for edge AI workloads** running
on Linux. It sees the model, not just the Python process — and kills the
offender before an 8 GB Jetson OOM-crashes the whole robot stack.

## What it is NOT

- Not a training dashboard (W&B owns that)
- Not a CUDA kernel profiler (nsys owns that)
- Not a fleet/cluster tool (k8s + Prometheus own that)
- Not a desktop task manager (htop/btop own that)
- Not a Windows tool — the Windows prototype was a learning artifact

## The audience

**Edge robotics and on-device AI developers.** Specifically:

- Robotics developers running ROS2 + perception + local LLM on Jetson, NUC, Pi
- On-device AI developers deploying inference to edge hardware
- Researchers running multi-model benchmarks on a single shared GPU

What they have in common:

- Linux (Ubuntu, JetPack, Debian, Arch)
- Shared, constrained hardware (8–32 GB RAM, one GPU/NPU)
- Multiple AI workloads fighting for resources
- Zero tolerance for silent OOM crashes that kill the whole stack
- Often headless — must work over SSH

## The wedge (one sentence)

*On a shared edge box running ROS + YOLO + a local LLM, this monitor sees
which model each Python process is running and kills the offender before
the kernel OOM-killer takes the whole robot stack down.*

## Why this wedge is defensible

Existing tools fail at exactly this intersection:

| Tool                       | What it misses                                    |
|----------------------------|---------------------------------------------------|
| `htop` / `btop`            | Shows "python" — no idea what model is loaded     |
| `nvtop` / `nvidia-smi`     | Shows VRAM per PID — no idea which model, no governor |
| `jtop`                     | Jetson-specific, no model awareness, no governor  |
| `py-spy`                   | Profiles Python — doesn't govern resources        |
| W&B / TensorBoard          | Training metrics — not a runtime monitor          |
| Prometheus + Grafana       | Requires setup, infra, no edge focus              |
| `systemd-oomd` / earlyoom  | Generic memory killer, zero model awareness       |

Nobody combines **model identification + resource attribution + safe
governor + edge-first**. That's the gap.

## Success criteria

### Minimum (what "v1 shipped" means)

- Runs clean on `apt install`-equivalent on Ubuntu 22.04+ x86_64
- Runs clean on JetPack 6 on Jetson Orin
- Correctly identifies common AI frameworks (Ultralytics, llama.cpp, Ollama,
  HuggingFace transformers, ONNX Runtime)
- Governor never kills an allowlisted process
- Governor never kills anything in dry-run mode
- Single `killall -9 edge_monitor` cleanly shuts down, no orphans
- README with a demo GIF that tells the story in 15 seconds

### Stretch (6–12 months post-launch)

- 1000+ GitHub stars
- Adopted by at least one named robotics project (visible in their CI or docs)
- At least one external contributor with merged non-trivial PR
- Packaged in at least one distro repo (Ubuntu PPA, AUR, or Nix)

### Long reach (18+ months)

- 5000+ GitHub stars
- Cited in at least one robotics paper or blog post as the monitoring tool
- De facto standard for Jetson-based edge AI resource monitoring
- Sustainable maintenance rhythm (releases every 6–8 weeks)

## Non-goals (explicit)

These will be asked for. Saying no early is the point.

- **Windows support after Linux ships** — unless a maintainer steps up to own
  it, the Windows prototype stays archived. Splitting focus kills projects.
- **GUI / web dashboard as core feature** — terminal is the feature for
  headless robots. A Prometheus exporter is the bridge if people want
  Grafana; we don't build the Grafana replacement.
- **Training metrics, hyperparameter tuning, model comparison** — out of
  scope. Point people to W&B / MLflow / Aim.
- **Cloud / multi-host monitoring** — edge-first means single-host. Fleet
  tooling is a different product.
- **Kernel-level instrumentation (eBPF) in v1** — too much complexity for
  v1, breaks on older kernels. Possibly v3+.
- **Supporting every NPU on day one** — NVIDIA first, then Intel + AMD +
  Hailo based on real user demand.

## Competitive positioning

Say in the README: *"If htop had a baby with a safety-conscious version of
earlyoom that could read Python scripts and recognize AI models, on a Jetson
— this is that baby."*

Core differentiators, in priority order:

1. **Model-awareness**: names the model, not the process
2. **Safe governor**: allowlist-first, dry-run default, graceful termination
3. **Edge-first**: Jetson Orin as a hero platform, not an afterthought
4. **Zero config to start**: sensible defaults, config only when you need it
5. **TUI that respects SSH**: works on a 80x24 terminal over a flaky link

## Guardrails against scope creep

Before adding any feature, it must pass all four:

1. **Does it serve the edge-robotics wedge?** If it's for cloud, training,
   or desktop — no.
2. **Does it work on a Jetson Orin?** If Jetson-unsafe, no.
3. **Does it increase the binary size by more than 10%?** If yes, it must
   earn it. Optional features gated behind Cargo features.
4. **Can it be explained in one sentence in the README?** If not, it's too
   complex for v1.

If a feature request fails any of these, the answer is "not now" with a
link to this document.

## What breaks this plan

Known risks, ordered by likelihood:

1. **Maintainer bandwidth.** Realistic coding window is evenings/weekends.
   Plan accounts for this. Mitigation: tight scope, slow cadence, honest
   README.
2. **NVML semantics on Jetson.** Per-process VRAM attribution on Tegra is
   not as clean as on desktop NVIDIA. Mitigation: tegrastats fallback in v1.1.
3. **Competitor ships first.** NVIDIA or a well-funded robotics startup
   ships something similar. Mitigation: ship v1 in 8 weeks, not 8 months.
4. **Unsafe governor kills someone's robot.** Reputational ruin. Mitigation:
   dry-run default, allowlist-first, SIGTERM-then-SIGKILL, visible audit
   log, loud warnings in README.

## Revisiting this doc

Review this document at these checkpoints:

- **Before v1 launch** — does the wedge still fit what we built?
- **30 days post-launch** — do real users validate the audience model?
- **Every 6 months after** — is the positioning still defensible?

If the wedge changes, rewrite this doc *first*, then change the code.
