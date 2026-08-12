<div align="center">

# raqib

**The watcher for your GPU box.**

One screen for every workload competing for your GPU — ROS 2 nodes, LLM servers, agents — and a governor that can evict the one starving the rest.

![status](https://img.shields.io/badge/status-beta-orange)
![platform](https://img.shields.io/badge/platform-Linux-blue)
![license](https://img.shields.io/badge/license-TBD-lightgrey)

<!-- DEMO GIF HERE — the single most important thing in this README.
     10-second loop of the TUI + a glance of the web kiosk. Record with
     asciinema+agg or a screen recorder -> docs/demo.gif -->

*demo GIF goes here*

**[Full documentation](https://ctx-012.github.io/Raqib/)**

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

```bash
# prerequisites: build-essential, pkg-config, libssl-dev, git,
#                Rust 1.88+ (install via rustup — distro packages are usually too old)
git clone https://github.com/CTX-012/Raqib.git raqib
cd raqib

# build the raqib binary (web dashboard assets are pre-built + committed)
cargo build --release
sudo install -m755 target/release/raqib /usr/local/bin/raqib

# first run
raqib init      # writes a safe-by-default config to ~/.config/raqib/raqib.toml
raqib           # TUI + web dashboard on http://localhost:7070
```

The web dashboard's built bundle (`web/dist/`) is committed to the repo, so
`cargo build --release` works standalone — no Node.js required for end users.
Contributors who modify anything under `web/src/` need Node.js 20+ and must
regenerate the bundle with `npm --prefix web ci && npm --prefix web run build`
before committing; CI enforces that the committed `web/dist/` matches a fresh
build.

Monitoring is on; the governor is off. Open `http://localhost:7070` for the web view.

**Common commands:**

```bash
raqib --no-ui        # web only (use for background/service runs)
raqib --no-web       # TUI only
raqib config check   # validate config + print the loaded policy
raqib --help
```

---

## Safety in one line

raqib can kill processes, but **out of the box it kills nothing** — it observes. The governor only acts after you deliberately set `auto_actuate = true` *and* `default_ai_action = "Kill"` in your config, and the web API can never arm it. Before arming, run `raqib config check` to see exactly what will and won't be killed. See the [governor guide](https://ctx-012.github.io/Raqib/governor).

---

## Documentation

Full guides, config reference, examples, and troubleshooting live on the **[documentation site](https://ctx-012.github.io/Raqib/)**:

- **[Getting started](https://ctx-012.github.io/Raqib/getting-started)** — install, first run, the five views
- **[Configuration](https://ctx-012.github.io/Raqib/configuration)** — every setting, explained
- **[The governor](https://ctx-012.github.io/Raqib/governor)** — how the kill switch works, safely
- **[Experiments](https://ctx-012.github.io/Raqib/experiments)** — hands-on walkthroughs (incl. a safe kill demo)
- **[Integrations](https://ctx-012.github.io/Raqib/integrations)** — ollama, vLLM, llama.cpp, ROS 2, Gazebo
- **[Troubleshooting](https://ctx-012.github.io/Raqib/troubleshooting)**

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

*TBD — add before publishing (MIT / Apache-2.0 dual recommended).*
