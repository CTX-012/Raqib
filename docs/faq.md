# FAQ

### Why not just use `htop` / `nvtop`?

Neither knows what model is loaded inside each process. `htop` shows
"python"; `nvtop` shows 4 GB VRAM used by PID 1234. `edge_monitor`
tells you it's `yolov8n` and how long it has been running.

### Why not `earlyoom` / `systemd-oomd`?

They can kill runaway memory hogs, but they're model-blind and have no
allowlist for "the robot's actual perception loop." They'd happily kill
your production stack in the same scenario where `edge_monitor`
preserves it.

### Will it run on a Raspberry Pi?

Linux-first means yes-ish. Pis without a discrete NVIDIA GPU will have
an empty `GpuSnapshot` — classifier + process-level monitoring still
works, just no VRAM attribution.

### Does it work on Jetson Orin?

That's the hero platform. NVML on Tegra has quirks around per-process
VRAM that we fall back gracefully on (logging a warning and using the
whole-device number). tegrastats integration is Phase 2.

### Will it kill my SSH session?

No — `sshd` is in the default allowlist. If you add a shell to the
allowlist too (default includes `bash`, `zsh`, `sh`), your interactive
sessions are safe from automated policy. Manual kill requires an
explicit override confirm for allowlisted names.

### Can I dry-run forever?

Yes — that's the default. Leave `policy.enforce = false` in your
config and the governor will log "would send SIGTERM to X" every tick
without ever signalling anything. Useful for tuning allowlist + model
detection before going live.

### How do I see the audit log?

Two options:

- In-memory: the TUI Audit panel shows the last `audit_history` entries.
- Persistent: set `runtime.audit_log_path` to a writable file; every
  decision is appended as one JSONL line.

### What counts against the rate limit?

Only real kills — decisions that actually send SIGTERM. Dry-run
"would-have-killed" decisions do *not* consume the budget; otherwise a
long dry-run session would exhaust the limiter and block you from ever
enforcing later.

### How do I extract the model Ollama is serving?

Ollama doesn't put the live model name in the process cmdline.
Currently we detect the `ollama` process and the `OLLAMA_MODELS`
directory. Polling `http://localhost:11434/api/ps` for the currently
loaded model is on the Phase 2 backlog.

### What if my model file lives in a non-standard location?

The classifier picks up any file ending in `.gguf`, `.safetensors`,
`.onnx`, `.engine`, `.plan`, or `.tflite` passed to `--model` /
`-m` / `--model-path` (or `=value` forms). If your launcher uses a
different flag name, add it to `MODEL_FLAGS` in
[`src/classifier/model_extract.rs`](../src/classifier/model_extract.rs).

### How do I contribute?

Read [VISION.md](../VISION.md) first — we say no to a lot of scope. If
your feature passes the four guardrails there, open an issue before a
PR so we can agree on acceptance criteria. Every PR must include tests
and pass `cargo clippy --all-targets -- -D warnings`.
