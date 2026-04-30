# edge_monitor — UI Design

> **What this is.** The single source of truth for what the TUI
> looks like and how it behaves visually. Replaces the visual sections
> of `DESIGN_HANDOFF.md` (which was forward-looking and partially
> aspirational). Implementation specs (`UI_CONTRACT.md`) reference
> this doc for layout and vocabulary.
>
> **What this is not.** Not a feature list. Not a roadmap. Not a
> rationale doc. Pure design — what the user sees, what each key
> does, in what color, with what string.
>
> **Scope.** Linux TUI only (the WSL build). Windows TUI mirrors
> this contract under cross-platform parity.

---

## Default screen — what the user sees on launch

```
┌─ System ───────────────────────────────────────────────────────────┐
│ load 2.7   cpu 16%   ram 8.2/16 GB   gpu –   62 procs   1 AI       │
└────────────────────────────────────────────────────────────────────┘
┌─ AI Workloads ─────────────────────────────────────────────────────┐
│ > ollama       PID 4242   1.2 GB RAM   38 tokens/sec               │
│   claude       PID 8801   180 MB RAM   idle                        │
└────────────────────────────────────────────────────────────────────┘
┌─ Recent runs ──────────────────────────────────────────────────────┐
│   phi3-mini    1m 5s     38.4 tok/s   matches baseline             │
│   llama3.1     2m 14s    24.1 tok/s   12% slower than baseline     │
└────────────────────────────────────────────────────────────────────┘

[k] arm kill  [Enter] details  [g] dashboard  [h] history  [v] more  [?] help  [q] quit
```

Three panels by default. Status footer at the bottom with the keys
that actually do something on this screen. No clutter.

### Panel: System (top, full width, 3 rows tall)

Single line of vitals, comma-spaced. No labels for things that are
obvious from the value (`8.2/16 GB` doesn't need to say "RAM" twice).

```
Border:    rounded, single-line, title " System "
Title:     left-justified in border-top
Content:   one line, plain text
Vitals:    load avg (1m), cpu pct, ram used/total, gpu pct or "–", proc count, AI count
```

**Vocabulary (locked):**
- `load 2.7` — not "load avg" or "loadavg"
- `cpu 16%` — not "CPU%" or "cpu_pct"
- `ram 8.2/16 GB` — not "RSS" or "memory" or "RAM:"
- `gpu –` when no GPU; `gpu 32%` when present (NVIDIA)
- `62 procs` — not "processes" or "PIDs"
- `1 AI` — short for "1 AI workload running"

### Panel: AI Workloads (middle, full width, expandable)

The list of currently-running AI processes. One row per process.
Selected row prefixed with `> `; others with `  ` (two spaces).

```
Border:    rounded, single-line, title " AI Workloads "
Selection: "> " on selected row, "  " on unselected
Columns:   name (10ch, truncate "…"), "PID" label, pid (6ch),
           ram in human bytes, throughput or status
Empty state: "No AI workloads detected yet — try `ollama run llama3 'hello'`"
```

**Vocabulary:**
- Process name comes from `/proc/<pid>/comm`, shortened
- `PID 4242` — bold "PID" label, dark gray
- `1.2 GB RAM` — value first, unit, then "RAM"
- Throughput: `38 tokens/sec` if known; `idle` if running but no
  recent token activity; `loading` if classifier sees it but no
  process metrics yet

### Panel: Recent runs (bottom, full width, 6 rows tall)

The five most-recent completed runs, newest at top. One row per run.

```
Border:    rounded, single-line, title " Recent runs "
Columns:   model (12ch), duration, throughput, baseline status
Empty state: "No completed runs yet"
```

**Vocabulary:**
- Model name as `display_name` (resolved by the classifier)
- Duration: `1m 5s` / `2h 14m` style, never seconds-only when > 60
- Throughput: `38.4 tok/s` (terse — full row is dense)
- Baseline: same color bands as post-mortem card (red/yellow/green/muted)

### Footer (last row, full width, 1 row tall)

Status hints, no border. Always visible. Updates contextually when
something is armed or focused.

```
Default:    [k] arm kill  [Enter] details  [g] dashboard  [h] history  [v] more  [?] help  [q] quit
With kill:  ARMED kill PID=4242 (ollama) — press k to confirm, Esc/5s to disarm — 5s
After kill: Sent SIGTERM to PID 4242 (ollama)                                  (auto-clear at 3s)
After dash: Opened http://localhost:3000/d/edge_monitor?model=ollama&pid=4242  (auto-clear at 3s)
```

**Color semantics:**
- Default: dark gray text, no background
- Armed banner: red background, white bold text
- Status messages (after-kill, after-dash, errors): default text, no background, auto-clear at 3 seconds

---

## Overlays — appear on top of the default screen

### Overlay: Armed-kill banner (top, row 0)

Replaces the topmost row of the screen with a red banner while a
kill is armed. Disappears when the arm is confirmed, dismissed, or
times out.

```
ARMED kill PID=4242 (ollama) — press k to confirm, Esc/5s to disarm — 5s
```

```
Position:  row 0, full width, height 1
Color:     bg red, fg white, bold
Window:    5 seconds, countdown updates every render tick
Strings:   verbatim:
  Normal:      "ARMED kill PID={pid} ({name}) — press k to confirm, Esc/5s to disarm — {n}s"
  Allowlisted: "ARMED kill PID={pid} ({name}) — ALLOWLISTED, press k to override — {n}s"
Countdown: rounds up (fresh arm reads "5s" not "4s")
Triggered: first `k` press on focused AI workload row
Cleared:   second `k` (fires kill), `Esc` (disarms), or 5s timeout
```

### Overlay: Post-mortem card (centered)

**Triggered by user pressing `Enter` on a focused row** in AI
Workloads panel. Shows the most recent run for that workload's
model. **Not auto-triggered** when a process exits.

```
                    ╭─ phi3-mini ──────────────────────────────╮
                    │                                          │
                    │  Duration:        1m 5s                  │
                    │  Avg CPU:         38.4%                  │
                    │  Peak RAM:        1.2 GB                 │
                    │  Peak GPU memory: 4.0 GB                 │
                    │  Throughput:      38.4 tokens/sec        │
                    │  Exited:          cleanly                │
                    │                                          │
                    │  12% slower than baseline                │
                    │                                          │
                    │  Last stderr lines:                      │
                    │  loading model weights...                │
                    │  warmup pass complete                    │
                    │  exiting cleanly                         │
                    │                                          │
                    │  [Esc] dismiss · [Enter] dismiss · 30s   │
                    ╰──────────────────────────────────────────╯
```

```
Position:  centered both axes
Width:     fixed 64 columns
Height:    computed from content, clamped [8, 22] rows
Border:    rounded, single-line
Title:     " {display_name} " (cyan, bold)
Padding:   1 col inside border, all four sides
Window:    30 seconds auto-dismiss

Field block (top to bottom, locked order):
  Duration:        {humanized_duration}     (always shown)
  Avg CPU:         {pct:.1}%                (always shown)
  Peak RAM:        {bytes_humanized}        (always shown)
  Peak GPU memory: {bytes_humanized}        (omit if 0)
  Throughput:      {tps:.1} tokens/sec      (omit if None)
  Exited:          {plain_english_reason}   (always shown)

Label column: 18 chars wide (longest is "Peak GPU memory:" + 2 padding)
Values:       left-aligned at column 19
Format:       1 decimal place where applicable

Baseline headline (single line below field block, blank line above):
  Critical (≥20% slower):   "{n:.0}% slower than baseline"  red bold
  Attention (≥10% slower):  "{n:.0}% slower than baseline"  yellow bold
  Healthy (≥10% faster):    "{n:.0}% faster than baseline"  green bold
  Matching (within ±10%):   "matches baseline"              dark gray
  Not available:            (omit headline entirely)

Stderr block (only when stderr_tail is non-empty):
  Blank line above
  Header:    "Last stderr lines:"   bold
  Up to 3 lines from stderr_tail (newest at bottom)
  Each line clipped to (CARD_WIDTH - 4) cols, "…" truncation marker

Footer (bottom, dark gray):
  "[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s"
```

**Triggers (only):**

- User presses `Enter` while focus is on AI Workloads panel and a
  row is selected → card shows that workload's most recent run
- User presses `Enter` while a card is already visible → dismisses
  the visible card

**Does NOT trigger on:**

- Process exit (auto-trigger removed in agent handoff #2)
- Any subcommand exit (`exec`, `compare`, `history`)
- Any timer or scheduler

### Overlay: History (centered)

Shown when user presses `h` on a focused row. Lists the last 20 runs
of that workload's model with timestamps and outcomes.

```
                    ╭─ History: phi3-mini ─────────────────╮
                    │                                       │
                    │  2026-04-30 14:22  1m 5s   38 tok/s ✓│
                    │  2026-04-30 13:18  2m 14s  24 tok/s ⚠│
                    │  2026-04-30 11:05  45s    OOM       │
                    │  ...                                  │
                    │                                       │
                    │  [Esc] close                          │
                    ╰───────────────────────────────────────╯
```

```
Position:  centered both axes
Width:     60% of screen, clamped [60, 100]
Height:    computed from content, clamped [10, 24]
Title:     " History: {model_name} "
Rows:      timestamp, duration, throughput, outcome marker
Outcome:   ✓ clean, ⚠ regression, ✗ crash, OOM, etc.
Empty:     "No runs recorded yet for {model}"
Dismiss:   Esc or h again
```

### Overlay: Help (centered)

Shown when user presses `?`. Lists every keybinding.

```
Position:  centered both axes
Width:     50 cols
Height:    computed
Content:   one line per binding, "{key}  {action}"
Dismiss:   Esc or ? again
```

---

## Keybindings (complete table)

| Key | When | Action |
|---|---|---|
| `q` | Always (Normal mode) | Quit |
| `Ctrl+C` | Always (any mode, including Filter) | Quit |
| `k` | Focused row + no kill armed | Arm kill on focused PID |
| `k` | Focused row + same PID armed | Confirm kill (fires SIGTERM) |
| `Enter` | Focused row + no card visible | Show post-mortem card for focused row |
| `Enter` | Card visible | Dismiss card |
| `Esc` | Card visible | Dismiss card |
| `Esc` | Kill armed | Disarm |
| `Esc` | History/help open | Close overlay |
| `Esc` | Nothing dismissable | Quit (same as `q`) |
| `g` | Focused row | Open dashboard URL in browser |
| `h` | Focused row | Open history overlay |
| `?` | Always | Toggle help |
| `d` | Always | Toggle dry-run / enforce mode |
| `v` | Always | Toggle detail mode (show extra panels) |
| `↑` `↓` | Always | Move selection within focused panel |
| `j` | Always | Move selection down (alongside `↓`) |
| `K` (Shift+k) | Always | Move selection up (alongside `↑`; uppercase to avoid collision with arm-kill `k`) |
| `Tab` | Always | Move focus to next panel |
| `Shift+Tab` | Always | Move focus to previous panel |
| `/` | Always | Filter mode (live filter on process names) |

**Cascading priority for `Esc`:** dismiss card → disarm kill →
close overlay → quit. First match wins; subsequent levels not
reached. **Note:** `q` is also accepted as a dismiss for the
history overlay specifically — pressing `q` while history is open
closes the overlay rather than quitting the app, on the rationale
that someone scanning a list intuitively reaches for `q` to back
out. All other overlays (help, post-mortem card) close on `Esc`
only; `q` quits from those.

---

## Detail mode (`v` toggles)

Off by default. Shows three additional panels below Recent runs:

```
┌─ Framework processes ──────────────────────────────────────────────┐
│   conda          PID 1234   240 MB RAM                             │
│   python         PID 5678   1.8 GB RAM                             │
└────────────────────────────────────────────────────────────────────┘
┌─ All processes (top by RAM) ───────────────────────────────────────┐
│   firefox        PID 9999   2.1 GB RAM                             │
│   ...                                                              │
└────────────────────────────────────────────────────────────────────┘
┌─ Recent governor actions ──────────────────────────────────────────┐
│   2026-04-30 14:30  killed PID 4242 (RAM > 12GB)                   │
└────────────────────────────────────────────────────────────────────┘
```

These are the panels that the original 6-panel design called
"Unmapped Processes," "Resource Hogs," and "Governor Interventions."
**Renamed** for plain English:

| Old name | New name |
|---|---|
| Unmapped Processes | Framework processes |
| Resource Hogs | All processes (top by RAM) |
| Governor Interventions | Recent governor actions |

---

## Color semantics (locked)

| Element | Color | Modifier |
|---|---|---|
| Panel borders | default fg | none |
| Panel titles | cyan | bold |
| Selected row marker `> ` | default fg | bold |
| Field labels (post-mortem, vitals) | default fg | bold |
| Field values | default fg | none |
| Armed banner bg | red | — |
| Armed banner fg | white | bold |
| Baseline: critical (≥20% slower) | red | bold |
| Baseline: attention (≥10% slower) | yellow | bold |
| Baseline: healthy (≥10% faster) | green | bold |
| Baseline: matching | dark gray | none |
| Footer / hints / muted | dark gray | none |
| Status: success (kill confirmed, etc.) | green | none |
| Status: error | red | none |

**No theme system.** No Tokyo Night palette. No `--theme` flag.
Use the terminal's default 16-color set; user's own terminal theme
applies. This is a deliberate simplification from `DESIGN_HANDOFF.md`'s
4-theme system that was never built.

**No partial-block bar characters** (`▎▍▌▋▊▉`). No Unicode meters.
Plain text values only. This is a deliberate simplification from
the original design.

---

## Vocabulary (locked, plain English only)

These words and only these words appear in the UI. Anywhere old
terminology survives in the codebase, replace on sight when touching
that file.

| Use | Don't use |
|---|---|
| RAM | RSS, PeakRSS, memory |
| GPU memory | VRAM, GPU mem, video memory |
| tokens/sec | tok/s (full form in cards/labels; tok/s OK only in dense rows) |
| Duration | uptime, runtime, elapsed |
| Avg CPU | CPU avg, mean CPU |
| Peak RAM | RSS peak, max memory |
| Exited cleanly | clean exit, normal exit, exit 0 |
| Killed by system (out of RAM) | OOM, OOMKill, OOMKilled |
| AI Workloads | Inference Registry, AI processes (in panel titles) |
| Framework processes | Unmapped Processes |
| All processes (top by RAM) | Resource Hogs |
| Recent governor actions | Governor Interventions |

---

## What was dropped from `DESIGN_HANDOFF.md`

For honesty about what's not happening:

- **Tokyo Night palette** — replaced with terminal-default 16 colors
- **`--theme` flag and 4 theme presets** — removed entirely
- **Status dots `●` per row** — not implemented; row state visible
  via columns, not glyphs
- **Partial-block bar chars (`▎▍▌▋▊▉`)** — no Unicode meters; plain
  numeric values only
- **Demo GIF slot in README** — separate concern; UI doesn't change
  to accommodate it
- **Notifications / webhooks** — out of scope for the TUI
- **`report --runs` HTML/markdown export** — out of scope; CLI
  subcommand decision, not UI
- **`clear retention` subcommand** — separate CLI concern

The kept ideas: plain English vocabulary, color semantics for
baseline bands, empty-state hints, post-mortem card field set.

---

## Reference: `UI_CONTRACT.md` relationship

This doc is the design. `UI_CONTRACT.md` is the implementation spec.
When they conflict, this doc wins for *appearance*; the contract wins
for *exact strings, dimensions, and behavior contracts that must
match across Linux/Windows*. Practical rule: if the contract says
"width 64 columns" and this doc shows a 70-column ASCII mockup, the
contract is right and the mockup is approximate.

Updates to either doc should propagate to the other in the same
commit.
