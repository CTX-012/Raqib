<div align="center">

# raqib

**The watcher for your GPU box.**

One screen for every workload competing for your GPU — ROS 2 nodes, LLM servers, agents — and a governor that can evict the one starving the rest.

![status](https://img.shields.io/badge/status-beta-orange)
![platform](https://img.shields.io/badge/platform-Linux-blue)
![rust](https://img.shields.io/badge/rust-1.88%2B-orange)
![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)

<video src="https://github.com/CTX-012/Raqib/raw/main/docs/raqib-demo.mp4" controls muted loop playsinline poster="https://raw.githubusercontent.com/CTX-012/Raqib/main/docs/media/tui-workloads.png" width="720">
Your browser can't play the embedded video. <a href="docs/raqib-demo.mp4">Download the 64-second narrated demo (3.2 MB, MP4)</a>.
</video>

![raqib demo — live TUI monitoring the workload mix on one GPU box](docs/demo.gif)

**Also:** [silent + captions MP4](docs/raqib-demo-silent.mp4) · [SRT captions](docs/raqib-demo.srt) · [6s hero GIF above](docs/demo.gif)

**[Full documentation](https://ctx-012.github.io/Raqib/)** · [Screenshots](#screenshots) · [Media guardrails](docs/MEDIA.md) · [Video plan](docs/VIDEO_PLAN.md)

</div>

---

## Why raqib?

You run more than one thing on one GPU — a ROS 2 stack, an LLM server, a couple of agents. They fight over the same VRAM. One balloons, and everything else grinds, or the box OOMs and takes your work down with it.

Tools like `nvtop`, `btop`, and `ollama ps` show you *that* it happened — all read-only, five terminals deep. **raqib is one screen for all of it, and it can act.**

- **One pane of glass** for every GPU/VRAM/CPU/thermal workload — ROS 2, ollama, vLLM, llama.cpp, agents.
- **Honest metrics** — unmeasurable VRAM shows an em dash, never a fake 0.
- **Live LLM health** — reachability + tokens/sec for ollama, vLLM, llama.cpp.
- **The governor** — can terminate a workload starving the others. **Off by default, opt-in.**
- **Built for a shared robotics + AI box.**

---

## Quick start

Fresh Ubuntu 22.04 or 24.04 box → running raqib in about five minutes. Every
line below is a real command; nothing hand-waved.

### 1. System packages (apt)

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config libssl-dev \
  git curl ca-certificates
```

Why each one:

- **build-essential** — C compiler + linker for the native bits of some crates.
- **pkg-config** — how Cargo discovers system libraries.
- **libssl-dev** — a few transitive deps still expect OpenSSL headers at build time.
  raqib itself uses rustls (no OpenSSL at runtime), but the dev headers keep
  the build clean on a first-time box.
- **git / curl / ca-certificates** — for cloning the repo and installing Rust.

### 2. Rust toolchain (rustup — distro packages are too old)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version   # must be 1.88 or newer
```

raqib uses `edition = "2024"` and let-chains — needs **Rust 1.88+**. Ubuntu's
apt-packaged rustc lags by 6–18 months; use rustup.

### 3. (Optional) NVIDIA GPU metrics

Skip this on an integrated-GPU or non-NVIDIA box — raqib still works, you just
lose NVML VRAM / thermal / power.

```bash
nvidia-smi   # if this prints your GPU, NVML is already present — done.
```

If `nvidia-smi` is missing:

```bash
sudo ubuntu-drivers install         # picks the recommended driver
sudo reboot                         # required after a fresh driver install
nvidia-smi                          # verify after the reboot
```

### 4. Clone + build raqib

```bash
git clone https://github.com/CTX-012/Raqib.git raqib
cd raqib
cargo build --release               # ~2-4 min on a modern box, first build
sudo install -m755 target/release/raqib /usr/local/bin/raqib
raqib --version                     # confirms the binary is on PATH
```

The web dashboard's built bundle (`web/dist/`) is committed, so
`cargo build --release` works standalone — **no Node.js required for end users.**

### 5. First run

```bash
raqib init                          # writes a safe-by-default config to ~/.config/raqib/raqib.toml
raqib                               # TUI + web dashboard on http://127.0.0.1:7070
```

Monitoring is on; the governor is off. Press <kbd>?</kbd> for keybindings,
<kbd>q</kbd> to quit. Open `http://127.0.0.1:7070` in a browser for the web
view.

### Common commands

```bash
raqib --no-ui        # web only (use for background / service runs)
raqib --no-web       # TUI only
raqib config check   # validate config + print the loaded policy (pre-arm gate)
raqib --help         # all flags
```

### Contributor extras (only if you're editing web/src/)

End users don't need Node.js. Contributors modifying anything under `web/src/`
do — Node 20+ and npm — to regenerate the committed bundle. CI enforces that
`web/dist/` matches a fresh build:

```bash
# Node 20 via NodeSource (Ubuntu):
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
npm --prefix web ci
npm --prefix web run build          # regenerates web/dist/
npm --prefix web run test:browser   # 221 render assertions
```

### Update raqib

```bash
cd raqib
git pull
cargo build --release
sudo install -m755 target/release/raqib /usr/local/bin/raqib
raqib --version
```

### Uninstall

```bash
sudo rm /usr/local/bin/raqib
rm -rf ~/.config/raqib               # optional — removes your config too
# clone directory + ~/.cargo can be removed too if you're not using Rust elsewhere
```

---

## Safety in one line

raqib can kill processes, but **out of the box it kills nothing** — it observes. The governor only acts after you deliberately flip both `auto_actuate = true` *and* `default_ai_action = "Kill"` in the config file (the web API can't arm it — seven tripwire tests pin the boundary). The auto-kill path itself is **proven live** (RAM canary + 19-cell signal × threshold matrix all green). Before arming, run `raqib config check` to see exactly what will and won't be killed. See the [governor guide](https://ctx-012.github.io/Raqib/#governor) for the four gates and the pre-arm checklist.

The web dashboard is also **secure-by-default** — `--bind 127.0.0.1` (localhost-only) unless you explicitly opt into `--bind 0.0.0.0` (LAN); doing that without an `auth_token` fires a loud startup WARN.

---

## Documentation

Full guides, config reference, examples, and troubleshooting live on the **[documentation site](https://ctx-012.github.io/Raqib/)** — one page, anchor-linked:

- **[Installation](https://ctx-012.github.io/Raqib/#install)** and **[first run](https://ctx-012.github.io/Raqib/#first-run)**
- **[The TUI](https://ctx-012.github.io/Raqib/#tui)** + **[keybindings](https://ctx-012.github.io/Raqib/#tui-keys)**
- **[The web dashboard](https://ctx-012.github.io/Raqib/#web-modes)** (5 modes) + the **[Settings panel](https://ctx-012.github.io/Raqib/#web-settings)**
- **[Configuration](https://ctx-012.github.io/Raqib/#config)** — every key with the real defaults
- **[The governor](https://ctx-012.github.io/Raqib/#governor)** — the four gates, the two kill paths, the pre-arm checklist
- Hands-on: **[see an LLM appear](https://ctx-012.github.io/Raqib/#exp-llm)** · **[safe kill demo (canary)](https://ctx-012.github.io/Raqib/#exp-kill)** · **[reclaim VRAM from ollama](https://ctx-012.github.io/Raqib/#exp-ollama-kill)**
- **[REST API](https://ctx-012.github.io/Raqib/#api)** · **[Integrations](https://ctx-012.github.io/Raqib/#integrations)** · **[Troubleshooting](https://ctx-012.github.io/Raqib/#troubleshooting)** · **[FAQ](https://ctx-012.github.io/Raqib/#faq)**

---

## Screenshots

<a name="screenshots"></a>

Live captures from the dev box on the current `main` branch. Every image is
governed by [`docs/MEDIA.md`](docs/MEDIA.md), which tracks exactly what each
one can and cannot be captioned with.

| | |
|---|---|
| ![TUI showing the live workload mix](docs/media/tui-workloads.png) | The **TUI** — vitals, workloads sorted by category (LLM / Agent / Vision / ROS 2), top processes, activity feed. One screen for everything competing for the GPU. |
| ![Web dashboard at localhost:7070](docs/media/web-dashboard.png) | The **web dashboard** at `localhost:7070` — the same live data in the browser. Read state, tune thresholds, persist to your TOML. |
| ![Activity feed panel showing recent events](docs/media/activity-log.png) | The **activity feed** — kill audit trail, workload starts/stops, and threshold events land here. |

The demo GIF at the top of this README is a 6-second loop of the same TUI
running against the real workload mix — no kill happens in it. The 64-second
[narrated demo](docs/raqib-demo.mp4) walks the whole arc; scene 5 (the auto-kill
moment) currently uses an animated still + audit-log reconstruction, per the
fallback in [`docs/VIDEO_PLAN.md` §9](docs/VIDEO_PLAN.md) — the real footage
lands once the reshoot does.

---

## About the name

**raqib** (Arabic) — *"the watchful one, who observes and guards."* A tool that watches over your workloads so you don't have to.

---

## Contributing

Issues and PRs welcome — especially first-run friction reports.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm --prefix web run test:browser
```

## License

Dual-licensed under either of:

- [MIT license](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

at your option. Contributions are accepted under the same terms.
