# edge_monitor — UI Contract v2

> **Status:** LOCKED. Both Linux and Windows implementations must match
> this contract. Deviations require an explicit cross-platform conflict
> report (filed in `BUILDER_STATUS.md` under cross-builder requests),
> not a silent workaround.
>
> **Supersedes:** `UI_CONTRACT.md` (v1). v1 was written before either
> repo's existing code was read; v2 reflects the better design surfaced
> by reading `crates/cli/src/ui/post_mortem_card.rs` on Windows.
>
> **What changed from v1:**
> - Post-mortem card title is now `display_name` (was: literal `Run summary`)
> - Field set updated: `Duration`, `Avg CPU`, `Peak RAM`, `Peak GPU memory`,
>   `Throughput`, `Exited` (was: Model/Ran for/Tokens/sec/Peak RAM/
>   Peak GPU memory/Exited/Compared to baseline)
> - Baseline indicator is a color-coded headline below the field block
>   (was: labeled row "Compared to baseline:")
> - Stderr is ephemeral — built at exit time on a transient `PostMortem`
>   struct, never persisted to `RunRecord`. v1 added `stderr_lines` to
>   `RunRecord` on Linux; v2 reverts that.
> - Esc is context-sensitive: cascades through dismiss/disarm, falls
>   through to quit when nothing to dismiss

---

## Keybindings

| Key | Action | Notes |
|---|---|---|
| `q` | Quit | Always quits. |
| `k` | Arm kill on focused PID (1st press) / confirm kill (2nd press within 5s) | Allowlist still respected; allowlisted targets show ALLOWLISTED variant. |
| `g` | Open dashboard for focused workload | Reads `[dashboard].url_template` from config; env var `EDGE_MONITOR_GRAFANA_URL` overrides; hardcoded fallback last. |
| `Esc` | **Context-sensitive cascade**: (1) dismiss post-mortem card if visible, else (2) disarm pending kill if armed, else (3) close any active overlay, else (4) quit | Single key, four behaviors based on UI state. |
| `Enter` | Dismiss post-mortem card (when card is visible) | Otherwise reserved. |
| `h` | Toggle history overlay | Existing behavior. |

## Strings (verbatim, no exceptions)

| Context | String |
|---|---|
| Armed banner, normal (enforce) | `ARMED kill PID={pid} ({name}) — press k to confirm, Esc/5s to disarm — {n}s` |
| Armed banner, allowlisted (enforce) | `ARMED kill PID={pid} ({name}) — ALLOWLISTED, press k to override — {n}s` |
| Armed banner, normal (dry-run) | `ARMED kill (DRY-RUN — won't die) PID={pid} ({name}) — press k to confirm, Esc/5s to disarm — {n}s` |
| Armed banner, allowlisted (dry-run) | `ARMED kill (DRY-RUN — won't die) PID={pid} ({name}) — ALLOWLISTED, press k to override — {n}s` |
| Status footer, kill confirmed in dry-run | `DRY-RUN: would have sent SIGTERM to PID {pid} ({name}) — press d to enforce` (yellow, bold; auto-clears at 3s) |
| Status bar, kill sent (enforce) | `Sent SIGTERM to PID {pid} ({name})` (Linux) / `Sent termination to PID {pid} ({name})` (Windows) |
| Status bar, kill blocked by rate limit | `Kill rate-limited (max {max} per {window}s) — try again in {wait}s` |
| Status bar, `g` with no focus | `No workload focused — select a row first` |
| Status bar, `g` with empty url source | `Set [dashboard].url_template in your config or EDGE_MONITOR_GRAFANA_URL env var` |
| Status bar, `g` browser-open failed | `Could not open browser — URL: {url}` |
| Status bar, `g` succeeded | `Opened {url}` |
| Post-mortem card title (border) | ` {display_name} ` |
| Post-mortem field labels | `Duration:`, `Avg CPU:`, `Peak RAM:`, `Peak GPU memory:`, `Throughput:`, `Exited:` |
| Post-mortem stderr header | `Last stderr lines:` |
| Post-mortem stderr line truncated marker | `…` (single character, end of line) |
| Post-mortem footer | `[Esc] dismiss · [Enter] dismiss · auto-closes in {n}s` |
| Post-mortem baseline headline (critical, ≥20% slowdown) | `{delta_pct:.0}% slower than baseline` (red, bold) |
| Post-mortem baseline headline (attention, ≥10% slowdown) | `{delta_pct:.0}% slower than baseline` (yellow, bold) |
| Post-mortem baseline headline (healthy, <-10% i.e. faster) | `{abs_delta_pct:.0}% faster than baseline` (green, bold) |
| Post-mortem baseline headline (matching, between -10% and +10%) | `matches baseline` (muted) |
| Post-mortem baseline headline (no baseline available) | (no headline rendered) |

## Layout dimensions

**Armed banner**: row 0 (top of screen, above existing title row). Height: 1.
Full width. Background: red. Foreground: white, bold. Countdown updates
every render tick (10 Hz), shown as integer seconds remaining
(`5`, `4`, `3`, `2`, `1`, then auto-disarms).

**Post-mortem card**: centered. Width: **fixed 64 columns** (was 60% in v1
— Windows already uses 64; locked here). Height: computed from content,
clamped to `[8, 22]` rows. Padding: 1 column inside the border, all four
sides. Border: rounded, single line. Title in border-top per ratatui
convention.

**Field alignment inside post-mortem card**:
- Labels left-aligned at column 1 (just inside left padding)
- Label column width: 18 chars (longest label `Peak GPU memory:` is 16
  + 2 padding)
- Values left-aligned at column 19
- Numbers within values formatted with 1 decimal place where applicable
  (e.g. `38.4 tokens/sec`, `4.2 GB`, `12.3%`)

**Field order inside post-mortem card** (top to bottom, locked):
1. `Duration:`
2. `Avg CPU:`
3. `Peak RAM:`
4. `Peak GPU memory:` (omitted entirely if zero or unavailable)
5. `Throughput:` (omitted entirely if no tokens/sec data)
6. `Exited:`
7. (blank line)
8. (color-coded baseline headline, if any)
9. (blank line, if stderr block present)
10. `Last stderr lines:` header
11. up to 3 stderr lines, each clamped to inner width with `…` truncation marker
12. (blank line)
13. footer

## Color semantics

| Context | Color | Source |
|---|---|---|
| Armed banner background | red | `Color::Red` |
| Armed banner text | white bold | `Color::White` + `Modifier::BOLD` |
| Post-mortem title | accent | matches existing TUI accent (Cyan or theme-equivalent) |
| Post-mortem field labels | bold | `Modifier::BOLD` |
| Post-mortem field values | default | no special color |
| Post-mortem baseline headline (critical) | red bold | `Color::Red` + `Modifier::BOLD` |
| Post-mortem baseline headline (attention) | yellow bold | `Color::Yellow` + `Modifier::BOLD` |
| Post-mortem baseline headline (healthy) | green bold | `Color::Green` + `Modifier::BOLD` |
| Post-mortem baseline headline (matching) | muted | `Color::DarkGray` |
| Post-mortem stderr header | bold | `Modifier::BOLD` |
| Post-mortem stderr lines | default | no special color |
| Post-mortem footer | muted | `Color::DarkGray` |

## Behavior contracts

**Kill flow timing**: 1st `k` press records `Instant::now()`. The arm is
valid for exactly 5 seconds. 2nd `k` press within that window confirms
and dispatches via the existing kill path (allowlist still respected;
manual kill still flows through audit log with manual-source tagging).
The FIRE branch dispatches on the *armed* PID, not on whatever is
currently selected — selection drift between presses (PID list reshuffle
across a tick boundary) must not silently re-arm. Any other key press
during the armed window does NOT cancel the arm — only `Esc` does, or
auto-disarm at 5s.

**Dry-run vs enforce rendering**: when the runtime is in dry-run mode
(`policy.enforce = false`, the default per CLAUDE.md safety rule 3),
the armed banner inserts ` (DRY-RUN — won't die)` between `kill` and
`PID=` for both the normal and allowlisted variants. After the second
`k` press confirms, `kill_sigterm` swallows the signal and returns
Ok(()); the status footer surfaces the dry-run message above so the
operator gets feedback that the press was received but the process is
intentionally still alive. Status footer auto-clears at 3 seconds and
is rendered in yellow + bold to match the `DRY-RUN` label colour in
the status bar.

**Post-mortem trigger**: the card appears when an AI-classified process
exits. Two paths:
- **Exec-wrapped exits**: card includes stderr from the runtime's
  ephemeral `PostMortem` struct (last 3 lines, clamped to card width).
- **Headless-monitored AI exits**: card is shown without the stderr block
  (no stderr was captured for the monitored process).

Non-AI process exits do **not** trigger the card. The card is replaced
by any subsequent AI exit (latest wins; no queue). The card auto-dismisses
30 seconds after appearing.

**Stderr is ephemeral.** The runtime constructs a `PostMortem` struct
at exit time, populates `stderr_tail` from whatever buffer is available
(exec wrapper has 64 lines; headless has none), hands it to the
renderer, then drops it. Stderr is never written to the persistent
`RunRecord` JSON. If a future feature needs stderr post-hoc (e.g.
"show me what the last 3 failing runs printed"), that's a separate
schema decision, filed as a new feature, not a side effect.

**Esc cascade order** (highest priority first):
1. If post-mortem card is visible → dismiss card, return.
2. Else if pending kill is armed → disarm, return.
3. Else if any other overlay is open (history, help, etc.) → close, return.
4. Else → quit (same as `q`).

**`g` URL source priority** (highest first):
1. `[dashboard].url_template` from TOML config, if set and non-empty
2. `EDGE_MONITOR_GRAFANA_URL` environment variable, if set
3. Hardcoded fallback: `http://localhost:3000/d/edge_monitor`

**`g` substitution**: `{model}` is replaced with the focused row's
`model_name` if present, empty string otherwise. `{pid}` is replaced
with the focused row's PID as a decimal integer. URL is opened via
the `webbrowser` crate (Linux) or platform-specific `start` / `open` /
`xdg-open` (Windows existing). Refuses if no row is focused.

---

## Cross-platform implementation note

Two repos (Linux on WSL at `~/edge_monitor`, Windows on Windows at
`C:\Users\intel\edge_monitor`) are independent codebases targeting
different OS-specific platform layers but matching this contract on
all user-visible behavior.

When the contract and existing code disagree on either side, file a
cross-builder request in `BUILDER_STATUS.md` and stop. Do not silently
change either side. The contract is the arbiter.

The two repos will be merged into a Cargo workspace on GitHub later
(see `MIGRATION_PLAN.md` Phase 7). Until that merge, the contract is
the only enforcement; line endings should be LF-normalized in both.
