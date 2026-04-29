# edge_monitor — Design, Experience & Gaps Handoff

> **Status (2026-04-29).** This document is *forward-looking design intent*,
> filed at the project owner's request as a sibling to
> [VISION.md](VISION.md) (audience) and [HANDOFF.md](HANDOFF.md)
> (engineering). It is **not yet adopted into [latest.md](latest.md)**,
> which remains the authoritative implementation roadmap. Items listed
> here that conflict with `latest.md` (e.g. theme system, energy
> accounting framing as v1, post-mortem cards, vocabulary changes) need
> to be reconciled into `latest.md` before any code chases them — per
> the parallel-builder protocol in [CLAUDE.md](CLAUDE.md), code that
> drifts ahead of the spec gets reverted at audit time.
>
> **How to use this doc.** Pull individual sections into `latest.md` as
> they are accepted; cite this doc as the source. Do not treat it as a
> single 10–12-day commitment — that estimate (Part 17) is for the full
> program after spec acceptance, not for ad-hoc cherry-picking.
>
> **Encoding note.** The original paste contained UTF-8/Latin-1 mojibake
> from clipboard round-tripping. Obvious cases (em-dashes, middle dots,
> degree signs, progress-bar full blocks, curly quotes) were normalised
> in transcription. Any remaining `â`-style artefact is one I was not
> confident enough to fix; flag it on review.

> **Purpose.** This document consolidates everything discussed about UX, visual design, color, themes, audience, and missing features into a single handoff. It is meant to be read by whoever picks up the design and product polish work next — whether that's another agent, a designer, or a future version of yourself returning to the project after a gap.
>
> **Companion documents:**
> - `IMPLEMENTATION_LINUX.md` / `IMPLEMENTATION_WINDOWS.md` — engineering specs for features
> - `TEST.md` — adversarial test specification
> - `verify-edge-monitor.sh` / `Verify-EdgeMonitor.ps1` — audit scripts
>
> Those documents tell you *what to build* and *how to verify*. This document tells you *how it should feel* and *what's missing from the experience*.

---

## Part 1 — The audience question (this drives everything else)

Earlier in the project we listed four audiences: beginner, hobbyist, dev, researcher. **Trying to serve all four with one UI was making the product muddled.** This handoff makes a deliberate choice:

**Primary audience: ML engineers and developers iterating on models.**
The tool's features (regression detection, audit log, history, governor, JSONL persistence) are dev concerns. Devs are the segment that writes blog posts and tweets screenshots. They're the segment with the strongest word-of-mouth.

**Secondary: solo developers running local LLMs (hobbyists/enthusiasts).**
They benefit from the simplified default screen, the clear empty states, and the post-mortem cards. They aren't the primary buyer but they're a healthy adoption flywheel.

**Tertiary: researchers running structured experiments.**
They live in Grafana and `--json` pipelines. The TUI matters less to them than the Prometheus exporter, history JSONL, and comparison commands. They're well-served by output formats, not pretty defaults.

**Out of scope: complete beginners with zero command-line comfort.**
edge_monitor is a CLI tool. A user who can't `cargo install` is not a user we optimize for.

**What this means in practice.**
- The default TUI is built for the dev segment. Plain English, sane defaults, no jargon overload.
- Progressive disclosure lets hobbyists stay shallow and researchers go deep without forking the product.
- "Beginner" features (tutorials, hand-holding, zero-config behaviors) serve the dev's first 60 seconds, not a separate persona.

**The pitch is now:**
> See what your AI workloads are actually doing. Live tokens/sec, energy per inference, and a permanent record of every run. Catches regressions before you do.

Notice the governor (the kill-runaway feature) is no longer the headline. It's a safety net that lives in settings, not the marquee. The headline is **AI-aware history with regression detection**.

---

## Part 2 — Visual design principles

These principles are non-negotiable. Every UI decision in the project should trace back to one of them. If you can't articulate why a visual choice supports a principle, the choice is decoration and should be cut.

### Principle 1 — Most of the screen is neutral. Color is the exception.

Look at the tools in the same family that work: htop, lazygit, k9s, btop. The default screen is mostly grey-and-white text on a dark or light background. Color appears at *moments* — a red bar when memory is critical, green when a build passes, amber when something needs attention.

When most of the screen is colored, color stops carrying information.

**Apply by:** starting every component as a single neutral foreground color. Only add color when answering "what does this color tell the user that the surrounding text doesn't?" If you can't answer, remove the color.

### Principle 2 — Color carries meaning, never decoration.

Three semantic colors, applied consistently across the entire product:
- **Green** — healthy. "Working well, no action needed."
- **Amber** — attention. "Degraded but not failed; user should glance."
- **Red** — critical. "Something went wrong; user must act."

Plus one **accent** color used for the product's identity (title bar, selected row, key hints). That's it.

**Forbidden patterns:**
- "Purple for LLM, blue for vision, green for embeddings." Categorization is the workload's name's job, not color's.
- Section-specific colors. Headers use the foreground color; borders use a muted grey. Boundaries are visual via spacing, not chromatic.
- Picking a new color whenever a new metric appears.

### Principle 3 — Plain English in the default view, jargon behind keys.

A beginner running their first local LLM should understand the default screen without a glossary. Technical terms exist behind explicit user actions (pressing `d` for details, opening `--json` output, reading the config docs).

**Translation table:**
| Avoid | Use |
|---|---|
| "RSS" | "RAM" |
| "VRAM" | "GPU memory" |
| "tok/s" | "tokens/sec" |
| "fps" | "frames/sec" |
| "NVML uninitialized" | "No NVIDIA GPU detected" |
| "INTERFERENCE LEVEL" | (delete entirely) |
| "Registry" | "AI Workloads" or just "Running" |
| "Rogues" | "Unrecognized" or remove from default view |
| "Culprits" | "Top Memory Users" or remove from default view |
| "Audit (governor decisions)" | "Recent Actions" — only in detail mode |
| "exit_code: 137" | "Killed by system (out of memory)" |
| "Permission denied (EACCES)" | "Need elevated privileges to read this process" |

### Principle 4 — Respect muscle memory.

Users come from htop, vim, less, k9s, lazygit. Match their conventions:
- `q` quits. Always.
- `?` opens help. Always.
- `/` searches. Always.
- Arrow keys + `j/k` navigate. Always.
- `Esc` cancels. Always.
- `Enter` activates / drills in.

**No clever schemes.** The Windows version's `a1`, `s2`, `d3` selection scheme was novel — and novel is bad for a tool. Users had to learn it. Standard arrow keys + Enter work for everyone with zero learning.

### Principle 5 — Information density scales with intent.

Default view is sparse. As the user explicitly opts in to more detail (presses keys, opens panels), the tool reveals more. The reverse — showing everything by default — overwhelms.

Three levels:
1. **Simple** (default) — what's running, basic system stats, status dots.
2. **Details** (press `d`) — full metrics, history visible, audit panel.
3. **Power** (CLI flags, config, Prometheus, JSON) — never visible in TUI; lives in the data layer.

### Principle 6 — Empty states teach the product.

When there's no data, the screen is not blank — it's a teaching moment.

| State | Old behavior | New behavior |
|---|---|---|
| No AI workloads running | Empty panel | "No AI workloads detected yet. Try `ollama run llama3` in another terminal — we'll detect it automatically." |
| No GPU | "GPU: not available (NVML uninitialized)" | "No GPU detected. Some features (VRAM, GPU power, fps) need a GPU. Everything else still works." |
| Network unreachable for sampler | Silent retry loop | "Can't reach vLLM at :8000. Some metrics may be missing." |
| First-time launch with no config | (proceeds with defaults silently) | "Running with defaults. Press `?` to learn the basics, or see ~/edge_monitor.toml.example for config." |

### Principle 7 — Whitespace is the most underused tool.

The current TUIs (Linux and especially Windows) cram every panel against every other panel. Borders touching, columns crammed, no breathing room. Compare to k9s or lazygit which use empty space deliberately.

**Apply by:**
- Padding inside panels (1-2 spaces minimum on every side)
- Gaps between sections (1 blank line minimum)
- Right-aligned numbers, left-aligned labels — never both centered
- Decimal alignment in numeric columns (`38.4` and `127.0` line up at the dot)

### Principle 8 — Weight and symbols carry emphasis cheaply.

Bold the primary metric on a row. Dim the secondary info. Use symbols (✓ ✗ ⚠ ●) instead of words when context makes meaning obvious.

This works in monochrome terminals. Color is one tool; weight is another. Use both.

### Principle 9 — No ASCII art.

The Windows version's "VATCH" block-letter banner is the kind of thing that makes a serious user close the tool. It signals personal-project amateurism. Even htop doesn't do this.

The first screen should look professional, not like a teenager's first GitHub repo. The product name in plain text, in the title bar, is enough.

### Principle 10 — Pre-flight checks before opening external things.

If pressing a key opens a browser, terminal, file, or external program, verify the destination first. A keystroke that opens a 404 page or a "command not found" terminal feels broken even if the surrounding tool works perfectly. Single most underrated polish item.

---

## Part 3 — The color palette

A working palette, with hex codes, drawn from the Tokyo Night family. Six colors, each with a defined role.

```
Foreground   #c0caf5   default text                  → 80% of screen
Muted        #565f89   secondary info, borders       → labels, units
Accent       #7aa2f7   product identity              → title, selection
Healthy      #9ece6a   "everything's fine"           → green status dot
Attention    #e0af68   "something needs a glance"    → amber dot, regression warn
Critical     #f7768e   "something's wrong"           → red dot, OOM kill, crash
```

**Why these specific colors:**

1. **Slightly desaturated.** Pure red `#ff0000` is panic-inducing; soft red `#f7768e` is firm but not stressful. Users sit in front of monitoring tools for hours; saturation fatigues the eye.

2. **Matched luminance.** When colors have wildly different brightnesses, the brightest one steals attention regardless of meaning. Matched luminance lets *meaning* drive attention.

3. **WCAG AA contrast on dark backgrounds.** Light grey on near-black passes accessibility. About 8% of men have some color vision deficiency, and at least one user will have low vision.

4. **Harmonize.** Drawn from the same palette family, they don't clash when they appear together. Compare to picking primaries (red/yellow/green/blue) which feel like a kindergarten poster.

**Backgrounds (don't forget these):**
```
BG default   #1a1b26   main background
BG raised    #24283b   panel surfaces, slightly lighter than default
BG selection #364a82   selected row highlight (dimmer than accent)
```

---

## Part 4 — Themes

Multiple themes are not about preference. They're about respecting the environment the user is already in. A power user with a tuned Solarized terminal wants apps to fit; if your tool ships only with One Dark, your tool feels out of place.

### Ship 3 themes for v1.0

**`dark` (default).** The Tokyo Night-ish palette above. Calm, professional, fits the "monitoring tool for serious work" register.

**`light`.** Same semantics, inverted — dark text on cream background, slightly desaturated colors so they don't burn on bright displays. Critical for users with bright office environments.

**`high-contrast`.** Pure black/white/yellow/red. No subtlety. Designed for low-vision users and bright environments where the dark theme washes out.

### Add `--theme=ansi` for terminal-respect

Power users with tuned terminals (Solarized, Gruvbox, Catppuccin in their terminal config) opt in. Use ratatui's `Color::Reset` and ANSI 16-color names. The tool inherits the user's terminal palette.

### Add config-driven themes for v1.1

```toml
[theme]
preset = "dark"   # or "light", "high-contrast", "ansi", "custom"

# When preset = "custom":
foreground   = "#c0caf5"
muted        = "#565f89"
accent       = "#7aa2f7"
healthy      = "#9ece6a"
attention    = "#e0af68"
critical     = "#f7768e"
```

Skip 12 themes. Ship 3 well-curated ones plus the ANSI escape hatch.

### Avoid these theme registers

- **Neon / synthwave.** Energetic, distracting. Wrong for monitoring.
- **Retro 80s.** Playful. Wrong for production.
- **Highly saturated primaries.** Tiring to look at for hours.

The tool should feel like something you'd run on a production server, even if your "production server" is your gaming PC.

---

## Part 5 — Layout principles

### The default screen, redesigned

```
  edge_monitor  ·  monitoring 2 AI workloads  ·  press ? for help

  Running

    phi3 (Ollama)            38.4 tokens/sec     4.2 GB        ●
    YOLOv8 (Ultralytics)     47.0 frames/sec     1.8 GB        ●

  System

    CPU      █████████████████  37%
    Memory   █████████████████  56%   (12.4 / 32 GB)
    GPU      █████████████████  68%   71°C   142 W

                                         d details · g graph · q quit
```

What this gets right vs. the current implementation:

- **No outer ASCII art banner.** Just one line of plain text identifying the tool.
- **No box-drawing characters as decoration.** Sections are separated by whitespace and a section label. Visual hierarchy from typography, not borders.
- **Numbers right-aligned.** Labels left-aligned. Decimals align in columns.
- **Status dots (`●`) on each row.** Single symbol carries health status. Green/amber/red. No color on the rest of the row.
- **Partial-block characters (`▎▍▌▋▊▉█`) for bars.** Higher resolution than plain `█`. Reads more "designed."
- **Accent color used in three places only.** Tool name in title, selected row, footer key hints. Nowhere else.
- **80% of the screen is neutral.** Color shows up only where it carries information.

A beginner reads this instantly. A dev sees what they need. A researcher presses `g` and goes to Grafana.

### Layout rules

**Right-align numbers, left-align labels.** Mixed alignment is illegible.

**Decimal-align numeric columns.** When you have `38.4`, `127.0`, `4.2` in a column, line them up at the decimal point. Eye scans instantly.

**Use weight (bold/dim), not color, for hierarchy.** Bold the primary metric. Dim the secondary. Works in monochrome.

**Borders are noisy.** Default to no borders. Use whitespace and section labels. Reach for borders only when content groups need explicit visual separation.

**Symbols replace words where context permits.** ✓ ✗ ⚠ ● ○ are clear. Prefer them to "(success)", "(failed)", "(warning)" labels.

**Be careful with emoji.** They break in some terminals (older ConHost on Windows, minimal SSH sessions, tmux sometimes). Stick to Unicode geometric shapes (●○■□▲△) which render consistently. Avoid emoji on the default screen; reserve for help text.

### Progressive disclosure structure

```
Simple TUI (default — beginner & dev first-glance)
    ↓ press 'd' for details
Detail TUI (full metrics, history visible, audit panel)
    ↓ press 'h' on a row
History overlay (per-workload past runs)
    ↓ press 'g' or Enter on a row
Browser → Grafana / built-in dashboard
    ↓ click on a panel
Grafana drill-down → Prometheus query
```

Each level escalates with one gesture. Nobody is forced to climb; everyone can stop at the level that suits them. The README should call this out explicitly:

> edge_monitor meets you where you are. New to local LLMs? The first screen tells you what's running in plain English. Want detail? One keypress. Want graphs? Press `g` and your browser opens. Want raw data? It's already in Prometheus format.

---

## Part 6 — Keybindings

A complete map. Defaults must follow conventions; novelty kills adoption.

### Global keys (work everywhere)

| Key | Action |
|---|---|
| `q` | Quit |
| `?` | Help overlay |
| `/` | Search / filter |
| `Esc` | Cancel current action / close overlay |
| `↑ ↓` or `j k` | Navigate selection |
| `← →` or `h l` | Switch panels (where applicable) |
| `Tab` | Cycle focus across panels |
| `Enter` | Drill into selected item |

### View modes

| Key | Action |
|---|---|
| `d` | Toggle details mode (simple ↔ full) |
| `g` | Open graph dashboard in browser (Grafana or built-in) |
| `h` | History overlay for selected workload |

### Workload actions (when a row is selected)

| Key | Action |
|---|---|
| `Enter` | Open detail view for this workload |
| `t` | Tag this workload |
| `o` | Open project directory in file manager |
| `e` | Open audit log entry in `$EDITOR` |

### Power actions (require explicit confirm)

| Key | Action |
|---|---|
| `k` | Arm kill (press twice to confirm) |
| `K` | Kill all matching the current filter (extra confirm) |
| `r` | Reset / clear local data (extra confirm) |

### What to avoid

- **`Ctrl+D` for any action.** Terminals send EOF on `Ctrl+D`. Most TUIs exit on it. Using it for anything else fights the terminal.
- **`Ctrl+C` for anything other than emergency exit.** Same reason.
- **Vim-mode-like multi-key sequences.** No `gg`, no `dd`, no `:q!`. Single keypresses only. We're not building Vim.
- **Letter combinations like `a1`, `s2`, `d3`.** Force the user to read selection IDs and remember mappings. Standard arrow keys do the same job with zero learning.

---

## Part 7 — The 16 feature gaps

Sixteen gaps identified across the conversation. Listed once, with priority. Every gap traces to a real user moment.

### v1.0 must-have (launch-blocking polish)

**Gap 1 — Onboarding and first-run.** Detect installed AI runtimes (Ollama, llama.cpp, Python+torch). Show "no workloads detected yet — try `ollama run llama3`" message. One-time tutorial walkthrough. ~1 day. **Disproportionate value: first-run quality determines whether the tool gets a second run.**

**Gap 5 — "What just happened?" post-mortem card.** When a workload exits, surface a brief card: what ran, how long, final metrics, why it stopped, last few stderr lines, baseline comparison. Stays on screen ~30 seconds or until dismissed. ~half day, most data already exists. **Huge for everyday users — this is the "oh that's helpful" moment.**

**Gap 8 — Energy accounting.** Aggregate power data over time, integrate to energy. Configurable cost-per-kWh. Reported as watts-per-token, joules-per-inference, kWh-per-day. ~1 day, math is simple. **Huge for differentiation — nothing else in the monitoring space publishes this. Tweet-worthy metrics.**

**Gap 12 (partial) — At least one killer demo GIF + one quickstart guide.** 30-second screencast: user runs Ollama, edge_monitor detects it, shows tokens/sec, run completes, post-mortem card pops up showing "18% slower than baseline." Plus a 5-minute quickstart for the primary audience. **Single most valuable artifact for adoption.**

**Gap 14 — Empty states and error states.** Audit every error path. Every empty state becomes a teaching moment. Every error message reviewed for jargon, made actionable, never blames the user. ~half day, tedious. **Large for first impressions and retention.**

### v1.1 should-have (next month of work)

**Gap 2 — Workload tagging.** User-supplied tags via `edge_monitor exec --tag "q4_test" --` or interactive `t` keypress. Tags become a queryable history dimension. ~half day. **Large for ML engineers doing iterative experimentation.**

**Gap 3 — Comparison and diff view.** `edge_monitor compare <run-id-A> <run-id-B>` puts two runs side by side: every metric, config, fingerprint. Already on the implementation roadmap as Tier 3.7 — elevate priority. ~half day if data exists. **Massive for the ML researcher segment.**

**Gap 4 — Notifications.** Optional desktop notifications, webhooks, email on important events: workload exited non-zero, OOM killed, regression detected, watch-flagged process completed. Configurable per event class. ~1-2 days. **Large for anyone running training or batch inference.**

**Gap 7 — Filtering and search.** `/` opens filter. Filter by name, category, status, tag. Persists across sessions optionally. ~1 day. **Essential once a user has more than a handful of workloads.**

**Gap 13 — In-tool help layers.** Three layers: contextual help on `?`, man page, troubleshooting section in README. ~1 day. **Large for retention. Users who get unstuck stay.**

**Gap 15 — Privacy and retention.** Configurable retention policy. `clear` command. Field redaction options. Documentation about exactly what's stored where. ~half day plus docs. **Matters a lot for enterprise / regulated users.**

### v2.0 (defer, validate need first)

**Gap 6 — Workload relationships.** Link Ollama server to its Python client. Process-relationship detection (network, IPC, parent-child). Display can fold runtime under workload. ~3-4 days, fiddly. **Medium value, improves clarity in common scenarios.**

**Gap 9 — Sharing and exporting.** `edge_monitor report --runs <ids>` produces portable HTML or markdown. Shareable via email/Slack/wiki. ~2 days. **Enables word-of-mouth — every shared report is an ad.**

**Gap 10 — Remote machines.** Decision: don't build a custom protocol. Document the Grafana approach. "Run edge_monitor headless on the server, expose Prometheus, view in Grafana from your laptop." Small effort (docs only). **Large value for the segment that needs it; rest don't care.**

**Gap 11 — Workload control vs observation only.** Real fork. Stay observation-only is simpler. Adding "start a workload" / "pause" / "resume" turns this into a task runner — different product. **Default decision: observation-only for v1, document `edge_monitor exec` as the lightweight version. Don't compete with task runners.**

**Gap 16 — Sampler health checks.** Detect when a known runtime is present but the sampler returns no data. Surface a warning. Version detection ("vLLM 0.5 detected; parser tested with 0.4"). Auto-update reminders. ~2-3 days. **Large for long-term viability.**

---

## Part 8 — The dashboard integration

You raised this directly: researchers love Grafana, give them one keypress to escalate.

### The keypress

`g` opens the graph dashboard. **Not** `Ctrl+D` (terminal collision). Not `Ctrl+G` (some terminals capture). Just `g`.

`Enter` on a workload row opens the dashboard *filtered to that workload*. Even better than the global keypress because it's contextual.

### What "open the dashboard" actually does

Three options, with tradeoffs:

**Option A — Open user's configured Grafana URL.** Simple. Requires user to set up Grafana. If the URL is wrong, browser opens a 404 and the feature feels broken.

**Option B — Built-in dashboard server on `localhost:9473`.** Zero setup. But you become a frontend team. Real cost.

**Option C — Hybrid.** First time `g` is pressed:
- If `[dashboard].url_template` is configured, open that URL with the right query params.
- If not, open a built-in landing page on `localhost:9473/setup` showing a one-paragraph "How to set up Grafana with edge_monitor" tutorial. Includes a copy-paste docker command and a button to download the dashboard JSON.

**Ship Option C.** Most generous to users without forcing you to be a dashboard product.

### Pre-flight check (never skip this)

Before opening the browser, verify the destination responds. If Grafana is down or returns 404, show inline TUI message:

> Grafana not reachable at <url>. Press `s` for setup help, or update `[dashboard].url_template` in config.

Single most underrated polish item. The difference between "polished" and "amateur."

### Pass meaningful query params

When the user has highlighted `phi3` and presses Enter, the URL should include `?var-model=phi3&from=now-1h&to=now`. Grafana opens already filtered to what the user was looking at. Without this, the feature is "open Grafana"; with it, the feature is "show me this workload's graphs."

### Don't lock to Grafana specifically

Some users have Datadog, some have custom dashboards, some use Prometheus's own UI. Make it templatable:

```toml
[dashboard]
url_template = "http://localhost:3000/d/edge-monitor?var-model={model}&from=now-{lookback}"
lookback = "1h"
```

### Bundle a sample dashboard

Ship `dashboards/grafana-overview.json` in the repo. Document the import flow in README. **Critical:** every panel in the JSON must reference a metric edge_monitor actually exports. Verify with `promtool check metrics` against the live `/metrics` output. The Windows audit caught dashboards-with-fake-metrics as a real bug pattern; don't repeat it.

### Cross-platform browser opening

Use the `webbrowser` Rust crate. Don't write three code paths for `xdg-open` / `start` / `open`.

---

## Part 9 — The "doorway, not destination" framing

This is the deeper UX win. edge_monitor is the *entry point* to a workflow that includes Grafana, the user's text editor, the file manager, jq pipelines, and existing monitoring systems. **It's not the whole experience.**

Tools that try to be the whole experience fight the user's existing workflow and lose.
Tools that act as a doorway connect to what the user already does and get adopted.

Apply this framing everywhere:

- TUI → press `g` → Grafana
- TUI → press `e` → opens audit log JSONL in `$EDITOR`
- TUI → press `o` on a workload → opens project directory in file manager
- CLI → `edge_monitor history --json | jq` for any pipeline workflow
- Prometheus output → any monitoring system the user already runs
- Run records on disk → any future tool that wants to read JSONL

Each is small individually. Together they make the tool feel like part of the user's environment instead of an island.

---

## Part 10 — What this changes about the product

Three honest realignments fall out of this design work:

### The governor stops being the main story

Auto-killing runaway processes is a safety feature, not a marketing feature. Bury it in "Settings → Auto-cleanup" rather than headlining it. Most users will never need it; those who do will find it. The audit log + governor combination is still a real differentiator for ops users — but it's a depth feature, not a hook.

### The differentiator becomes "AI-aware history"

Per-model run records, regression detection, energy accounting, comparison. **That's** the wedge against htop, nvtop, Glances, btop. None of them have any of this. The pitch:

> See what your AI workloads are actually doing. Live tokens/sec, energy per inference, and a permanent record of every run. Catches regressions before you do.

### The audience hierarchy clarifies

Primary: ML engineers iterating on models. Their feature priorities drive the roadmap. Their screenshots drive adoption. The other audiences come along for the ride — well-served by the same tool, but secondary in priority disputes.

### The tool is calmer than it currently looks

All the "REGISTRY," "CULPRITS," "VISION & AI INFERENCE REGISTRY," "INTERFERENCE LEVEL" language stops feeling like features and starts feeling like noise. Strip it. Replace with plain English. The redesign in Part 5 makes this concrete.

---

## Part 11 — Anti-patterns specific to this project

Things observed in past versions of edge_monitor that should not return:

1. **The VATCH ASCII banner on Windows.** Reads as personal-project amateurism. Replace with a single-line title.
2. **Different vocabulary on Linux vs Windows.** "Registry" vs "VISION & AI INFERENCE REGISTRY." Pick one set. Linux's terms (post-rewrite) are the source of truth.
3. **Selection scheme `a1`, `s2`, `d3`.** Replace with arrow keys + Enter.
4. **Color used for categorization.** The category is in the workload name. Color repeats nothing useful.
5. **All-caps section headers.** Reads like marketing copy in a tool. Use Title Case or sentence case.
6. **"Production-ready" / "100% complete" claims in the UI or docs.** Earned by users, not asserted by the tool.
7. **Borders around every panel.** Visual noise. Use whitespace.
8. **Crammed columns with no padding.** Add breathing room.
9. **Technical errors thrown as-is at users.** Wrap them with actionable language.
10. **"Tier 1 100%" while a Tier 1 feature is missing.** Status reports must be honest. Either move the feature out of Tier 1 or admit Tier 1 isn't done.

---

## Part 12 — Cross-platform consistency policy

Linux and Windows users have different habits, but edge_monitor users often run *both* (laptop + GPU server). Decisions:

- **Look identical across platforms.** Same TUI library (ratatui), same vocabulary, same keybindings, same colors. Users SSH-ing from a Mac to a Linux server should feel at home.
- **Use forward slashes everywhere in paths.** Linux users expect; Windows users tolerate.
- **Don't use platform terms where a neutral term exists.** Say "elevated privileges" not "sudo" or "administrator." Say "running in the background" not "service" or "daemon."
- **Shared core, thin platform shims.** The workspace structure (`core/` + `platform_linux/` + `platform_windows/` + `cli/`) keeps platform divergence localized.
- **Same install story shape.** `cargo install` works everywhere. Distribution channels (brew/scoop/winget) come post-launch.

The Windows agent's earlier work created a divergent product. The unification (U.1–U.5 in IMPLEMENTATION_WINDOWS.md) is a prerequisite for any of this design work landing on Windows.

---

## Part 13 — README rewrite framing

The current README is feature-list-shaped. Rewrite it to:

### Lead with the killer demo GIF

30-second screencast. Above the fold. Before any text. Shows:
1. User starts `ollama run phi3 "explain quicksort"`
2. edge_monitor detects it, displays tokens/sec live
3. Run completes, post-mortem card appears
4. Card shows "18% slower than your baseline of last 5 runs"

That's the artifact that converts visitors to users. Worth more than three feature paragraphs.

### Audience-specific quickstarts (3 of them)

Three short tutorials, each ~5 minutes, each assumes nothing from the others:

**"I run Ollama locally for chat."**
Install. Run. See your chats appear. Done.

**"I'm fine-tuning on my GPU."**
Install. Add `--tag "experiment-3"` to your training command. Compare runs across experiments. Done.

**"I'm running a model server in production."**
Install. Configure Prometheus. Import the Grafana dashboard. Set up alerts on regression events. Done.

### Honest feature list

After the demo and quickstarts, *then* the feature list. Honest about what works:

- ✓ Linux (Ubuntu 22.04+, RHEL 9, Debian 12 tested)
- ✓ Windows 11 (signed binary)
- 🟡 macOS (untested but should work)
- 🟡 Jetson (untested; tegrastats sampler shipped)
- ✗ Multi-GPU host (single-GPU tested; multi-GPU untested)

Not "production ready." Not "100% complete." Just what works and what's untested.

### Troubleshooting section

Discoverable, scannable. Top 10 issues with one-line fixes. Examples:
- "GPU shows as unavailable" — install nvidia-driver, ensure `nvidia-smi` works
- "tokens/sec shows 0" — check that vLLM is exposing /metrics
- "Permission denied on /proc/<pid>/environ" — expected for processes you don't own
- "Defender blocks the binary" — see signing instructions in INSTALL.md

---

## Part 14 — What ships in v1.0 (consolidated)

Pulling together everything in this document, the v1.0 launch list:

### Engineering (from IMPLEMENTATION_*.md)
- Tier 1.1 history viewer ✓
- Tier 1.2 telemetry samplers (vLLM, llama.cpp, Ollama) ✓
- Tier 1.3 regression detection ✓
- Tier 2.1 power & thermals ✓
- Tier 2.2 cold-start I/O ✓
- Tier 2.3 Prometheus exporter (in progress)
- G.1.11 / G.7 PID reuse safety fix (must land)
- Identity unification on Windows (U.1–U.5)

### Design (this document)
- Plain-English label rewrite
- Six-color palette implemented
- Three themes (dark, light, high-contrast)
- Default screen redesign per Part 5
- Section labels replace "Registry/Rogues/Culprits" vocabulary
- Status dots (●) for at-a-glance health
- Whitespace pass on all panels
- ASCII banner removed

### Experience (the gaps)
- Gap 1: First-run / onboarding
- Gap 5: Post-mortem card on workload exit
- Gap 8: Energy accounting (watts/token, joules/inference)
- Gap 12: Killer demo GIF + one quickstart
- Gap 14: Empty state + error state pass

### Integration
- `g` keypress opens Grafana / built-in setup page
- Sample Grafana dashboard JSON (verified against actual metrics)
- Pre-flight check before opening browser
- Templatable URL for non-Grafana users

### Documentation
- README rewrite per Part 13
- Three audience-specific quickstarts
- Troubleshooting section
- Honest "what works / what's untested" matrix
- Configuration reference for new keys (theme, dashboard, retention)

---

## Part 15 — What v1.1 adds

The polish wave that follows the launch:

- Gap 2: Workload tagging
- Gap 3: Comparison/diff view (`edge_monitor compare`)
- Gap 4: Notifications (desktop, webhook, email)
- Gap 7: Filtering and search
- Gap 13: In-tool help layers
- Gap 15: Privacy and retention controls
- Custom theme support via config
- Per-audience tutorials for the remaining segments
- Distribution channels (brew, scoop, winget)

---

## Part 16 — How to verify this design lands

Three checks before declaring the design work done:

### Check 1 — The 60-second test
Show the redesigned default TUI to someone who's never seen edge_monitor. Don't explain anything. Ask: "what does this tool do?" Their answer reveals whether the design communicates without help.

Pass: they can name 2-3 things the tool tracks.
Fail: they ask "what's a Registry?" or "why is this colored like that?"

### Check 2 — The screenshot test
Take the redesigned screenshot. Post it to a relevant subreddit or share with 5 ML engineers. Note the reaction.

Pass: someone says "oh, that looks nice" or "I'd install that."
Fail: silence, or "looks like btop with extra steps."

### Check 3 — The first-run test
Spin up a fresh VM. No prior edge_monitor exposure. Install. Run. Time how long until the user understands what the tool does and tries something.

Pass: <60 seconds to first useful interaction.
Fail: confusion, requires reading docs, gives up.

These tests are cheap and the only real measure of whether the design work succeeded.

---

## Part 17 — What to do next

Ordered task list for whoever picks this up:

1. **Audit the current TUI against Part 2 principles.** List every label, color, and layout decision. Mark each as keep / change / cut. Cheapest, fastest improvement. ~1 day.

2. **Implement the palette and three themes.** ratatui supports this directly. Add `--theme` flag. ~1 day.

3. **Apply the vocabulary rewrite.** Every "Registry" → "AI Workloads," every "RSS" → "RAM," every "NVML uninitialized" → "No GPU detected." ~half day.

4. **Build the redesigned default screen** per Part 5 layout. The simple-mode default. ~1 day.

5. **Implement Gap 5 (post-mortem card).** Most data already exists. Just present it. ~half day.

6. **Implement Gap 14 (empty/error state pass).** Audit every error path. ~half day.

7. **Implement Gap 1 (first-run).** Detect runtimes, show "try this" hint. ~1 day.

8. **Implement Gap 8 (energy accounting).** Math is simple, data exists. ~1 day.

9. **Implement `g` keybinding** with pre-flight Grafana check (Part 8). ~half day.

10. **Record the killer demo GIF** (Gap 12). ~1 day to script, record, edit.

11. **Rewrite the README** per Part 13. ~1 day.

12. **Run the three verification checks** in Part 16. As long as it takes.

Total: roughly 10-12 days of focused work to take edge_monitor from "engineering-complete" to "shippable polish." Not a lot for the upgrade in product quality.

---

## Closing note

The features were the easy part. Engineering an edge_monitor that monitors AI workloads with a governor and history is doable in a few weeks; we did it.

The hard part — the one that determines whether anyone actually uses what we built — is everything in this document. The plain-English labels. The calm color palette. The respect for user habits. The empty states that teach. The post-mortem card that makes the user think "oh, that's helpful." The killer demo GIF that converts the visitor to a user.

A tool with five polished features beats a tool with thirty half-finished ones, every time. The Pareto principle applies brutally to developer tools: 80% of adoption comes from 20% of the work, and that 20% is design and onboarding, not features.

The good news: edge_monitor's bones are strong. The features work. The code quality is real. Adding the UX layer on top is a matter of taste and discipline, not raw effort. You're closer to a great product than the feature list suggests.

Ship the polish, not just the engineering. Then run the three checks. Then tag v1.0.
