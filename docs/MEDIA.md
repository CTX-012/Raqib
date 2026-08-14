# raqib — media assets

This file is the source-of-truth for every image, GIF, and video in the raqib
repo. It exists for two reasons:

1. **Honesty guardrails.** The claim table below states exactly what each
   asset shows and — importantly — what it does NOT claim. No asset is
   permitted to imply a feature raqib doesn't actually have. If a future
   contributor adds a caption or a callout that contradicts the "does NOT
   claim" column, that's a bug.
2. **Reproducibility.** Every asset has a regen command. If a screenshot
   goes stale (UI drift, threshold changes, wire-schema evolution), the
   command to rebuild it is one paste away.

All assets are dark-theme only — the TUI is dark, the web dashboard is dark,
so the media is dark. The docs site's `index.html` is separate; that file
has its own dark palette.

---

## Assets

Under `docs/media/` (all committed):

| File | Size | Dims | Source |
|---|---|---|---|
| `tui-workloads.png` | 66 KB | 1754 × 698 | Live TUI capture 2026-08-14 15:30:34, `raqib` running against the real workload mix on the RTX 3060 dev box |
| `web-dashboard.png` | 49 KB | 1616 × 469 | Live web capture 2026-08-14 15:32:06, `http://127.0.0.1:7070/` |
| `activity-log.png` | 71 KB | 1764 × 560 | Live TUI capture 2026-08-14 15:52:07, activity feed panel |
| `../demo.gif` | 373 KB | 720 × ~325 | 6.0 s, 12 fps loop from a live monitoring pan (Recording 2026-08-14 16:02:13, trimmed t=4→10 s) |

The GIF loops silently; the palette was optimized with `palettegen`/
`paletteuse` (Bayer dither) so the file stays tiny without ANSI-colour
banding.

---

## Claim table — what each asset can be captioned with

**Rule of thumb:** a caption may name only what is visibly happening in the
asset in front of the reader. Do not import claims from another asset,
another session, or the storyboard's aspirational scenes.

### `tui-workloads.png` — TUI, live workload mix

| Claims allowed | Claims NOT allowed |
|---|---|
| "raqib's terminal UI showing the live workload mix on one dev box" | "raqib killed X" (this is a static frame; no kill visible) |
| "vitals, workloads, top processes, activity — all in one screen" | "the governor is armed" (auto_actuate state is not visibly proven in a still) |
| "AI category classification (LLM / Agent / Vision / ROS 2 as the row shows)" | Specific tokens/sec numbers as raqib's benchmark — the number is a live sample of one workload, not a benchmark claim |
| Whatever specific workload names appear in the still | Any workload not visibly named in the still (no importing from another session) |

### `web-dashboard.png` — Web dashboard, live

| Claims allowed | Claims NOT allowed |
|---|---|
| "the same live data in the browser at `localhost:7070`" | "the web can arm the governor" — it cannot; seven tripwire tests pin this boundary |
| "read the state, tune thresholds, persist to your TOML" | Persisted-formatting claims that aren't visible (correct in the code, but this still doesn't show it) |
| Whatever panel content is visibly rendered | Feature claims for panels not visible in the crop |

### `activity-log.png` — Activity feed panel

| Claims allowed | Claims NOT allowed |
|---|---|
| Whatever activity lines are visibly on screen | "raqib killed X automatically" unless a `SIGTERM ... source=Automated` line is visibly present in the still |
| "raqib's activity feed panel — kill audit trail lives here" | Claims about frequency / rate that require the full audit log, not this crop |

### `demo.gif` — monitoring pan loop (6 s)

| Claims allowed | Claims NOT allowed |
|---|---|
| "raqib's live TUI — the workload mix on one GPU box, one screen" | "the governor is firing" (this GIF is a monitoring pan; no kill happens in it) |
| "workloads sorted by category, live vitals, live activity" | "raqib killed the runaway workload" (the auto-kill sequence is a separate asset — currently held for Phase 3) |
| Loop implies continuous live behavior | Any numeric claim that the loop doesn't visibly sustain across its full duration |

---

## Regen commands

All commands assume you're in the repo root.

### Fresh stills

Any of the three PNGs can be replaced with a newer screenshot. Preserve the
`docs/media/<name>.png` path so the README + `index.html` links keep working.
Recommended shape:

- 1600-1900 px wide, dark theme, no transparency.
- Crop tight to the panel(s) the still is meant to show — don't include the
  whole desktop unless the desktop context matters.
- No sensitive data on screen (branch names, tokens, real user names). raqib
  itself never displays tokens; check the shell prompt behind it.

```bash
# Example: replace tui-workloads.png with a new capture
cp ~/Pictures/Screenshots/"Screenshot from <date>.png" docs/media/tui-workloads.png
```

### Rebuild the demo GIF

```bash
# 1) pick a source clip — a live TUI or dashboard recording
SRC="$HOME/Videos/Screencasts/<your-recording>.mp4"
START=4          # seconds — where the loop begins
LEN=6            # seconds — target 5-8s for a README GIF

# 2) two-pass palette for a small, banding-free GIF
PALETTE=/tmp/raqib-palette.png
ffmpeg -y -ss $START -t $LEN -i "$SRC" \
  -vf "fps=12,scale=720:-1:flags=lanczos,palettegen=max_colors=192:stats_mode=diff" \
  "$PALETTE"
ffmpeg -y -ss $START -t $LEN -i "$SRC" -i "$PALETTE" \
  -filter_complex "fps=12,scale=720:-1:flags=lanczos [x]; [x][1:v] paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
  -loop 0 docs/demo.gif

# 3) sanity — under 5 MB, plays clean
identify docs/demo.gif | head -3
du -h docs/demo.gif
```

Tunables if the GIF needs to be smaller: drop `fps=12` to `fps=10`, shrink
`scale=720:-1` to `scale=640:-1`, or lower `max_colors=192` to `128`. Loop
seam: pick a `START` where the terminal state is visually stable and take
`LEN` seconds during which no scroll or major state change happens — the
last frame should look like the first.

### Test the render locally

Open the docs site index in a browser: the README (`README.md`) and the
docs site (`docs/index.html`) both reference these assets by relative path
from the repo root — no build step, no bundler.

---

## Aspirational assets — NOT here yet

The following are described in the storyboard files but are NOT in this
repo. Do not link them from README or `docs/` until they land:

- **The auto-kill hero video** — a clean take of `raqib` autonomously
  killing an ollama runaway with no keypress on camera. The existing
  screen recordings include manual `k`-kill takes; those are not usable
  for the "raqib acts on its own" claim (a keypress contradicts it). A
  dedicated reshoot per the storyboard's `RESHOOT` block is required
  before this asset can be produced. Blocks Phase 3 of the video build.
- **Motion-graphics scenes** — title card, four-gates safety diagram,
  callout overlays, end card. Described in `docs/VIDEO_PLAN.md`; will be
  built with a Remotion-style pipeline once the plan is ratified.
- **Voice-over track** — VO engine choice, script, and per-scene audio
  live in `docs/VIDEO_PLAN.md`. Not synthesized until the plan is
  approved.

---

## Attribution

Every asset is captured from the operator's own dev box, on the current
`main` branch of this repo. No stock footage, no AI-generated frames, no
third-party workloads shown under a raqib caption. If any of that ever
becomes part of an asset, mark it clearly in this file's claim table
before shipping.
