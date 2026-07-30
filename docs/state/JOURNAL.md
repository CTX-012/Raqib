# JOURNAL — append-only session log

*Append a dated entry after EVERY landing. This is how you (the agent) remember what happened across sessions — you have no memory between them, so WRITE IT DOWN. Newest at the bottom. Keep entries short: what shipped, tests delta, commit, anything pending.*

*Format:*
```
## YYYY-MM-DD — <short title>
- Commit: <hash> — <one-line what>
- Tests: <before> → <after> (delta reason)
- Gate: <browser gate before> → <after> if web-touched
- Dormant/pending: <what's built-but-not-live, or what needs human>
- Notes: <anything the next session needs to know>
```

---

## 2026-07-15 — autonomy structure installed (baseline)
- Baseline: v1.3.2-36-g729bdf7, 1181 tests, gate 221/0, contract v0.3.21.
- Three arcs complete (auto-kill, history, display modes), both cleanup chunks done.
- This JOURNAL + BOARD + CLAUDE.md + PENDING.md installed for autonomous operation.
- Next candidate work: TUI essentials-only (needs design first — HARD STOP #3), or GPU temp/power tile (low-risk).
- Two human decisions pending: versioning (v2.0.0?), observer→supervisor.

## 2026-07-15 — landing 1: orchestration structure committed
- Commit: 73857b6 — chore: autonomous-orchestration structure (CLAUDE.md replaced, .claude/agents added, docs/state populated)
- Tests: 1181 → 1181 (meta-config only; no code touch)
- Gate: 221/0 → 221/0 (no web touch)
- Dormant/pending: commit not pushed — awaits operator review/push (milestone gate).
- Notes: replaces prior multi-agent-workflow CLAUDE.md with autonomous-orchestrator flavor. Old content preserved in git history if needed. The `.claude/agents/investigator` and `tester` subagent types aren't registered in the current SDK session — the files exist for future sessions; this session invokes their behavior directly under the same read-only / verify-only discipline.

## 2026-07-15 — landing 2: GPU tile investigation → HARD STOP #3
- No commit — surfacing-only landing. Investigation report written to `docs/state/PENDING.md`.
- Signal availability confirmed at `src/platform/gpu_nvidia.rs:220-227` (temp + power both `Option<f32>` from NVML). Prometheus surface exists at `exporter.rs:191-207`. NOT on TUI or web wire today.
- Wire gap: `WireGpu` is repo-local (`src/web/wire.rs:466-472`), NOT in `../ux_contract`. Adding temp_c/power_w fields is additive consumer-side — NO CAR needed.
- HARD STOP #3 fired: 3 design choices (placement scope, kiosk tile shape, aggregation) — no doc settles them. Inspector proposed a lean (1c/2a/3a) with rationale; operator ratifies before landing 3.
- Loop status: hit EXIT condition on the autonomous side. Everything else in BOARD is human-blocked or hardware-blocked. Awaiting operator ratification of the GPU-tile design to resume.

## 2026-07-15 — landing 4: GPU tile web consumers (dashboard + kiosk)
- Commit: e4772d3 — feat(web): DISPATCH 109 landing 4 — GPU tile web consumers (VitalsPanel + KioskView + gate matrix)
- Tests: 1184 → 1184 (web-only landing; rust unchanged from landing 3)
- Gate: 221 → 223 (+2 assertions on F6 kiosk's GPU measured halves)
- Bundle etag flipped `f3715c48...` → `be2dcacf...`. **Rust-embed rebuild note:** release binary needed a `touch src/web/assets.rs` before `cargo build --release` picked up the new bundle. Documenting: rust-embed's proc-macro doesn't always invalidate when only web/dist/ changes; if a smoke shows the old etag after a release rebuild, `touch src/web/assets.rs`.
- Web surfaces: `WireGpu` types.ts mirror gains `temp_c?/power_w?`; VitalsPanel adds GPU inline line beneath VRAM (dashboard); KioskView grows 3→4 tiles (RAM/VRAM/GPU/THERMAL); F6 fixture gains `temp_c:87 power_w:175`.
- Honesty discriminator carried through: per-half unmeasured shows "—" with `data-testid-unmeasured="true"`; belt-and-braces guard fails LOUDLY if `0°C`/`0W` ever appears.
- Live smoke: `/api/snapshot` → `temp_c: 63.0, power_w: 83.234, devices: 1`. Wire clean end-to-end.
- Dormant/pending: 3 landings (1 + 3 + 4) sitting unpushed. Loop hit EXIT — everything else in BOARD is human-blocked (versioning, observer→supervisor) or HARD-STOP-blocked (auto-kill tiebreaker, contract cleanup, TUI essentials rework).
- Notes: no governor touch. No `ux_contract` touch. Design doc-worthy note: kiosk 4-tile layout is fine at md+ widths; on sm it wraps 2-column, on xs it stacks 1-column. Acceptable — kiosk targets wall monitor / secondary display, always wide.

## 2026-07-15 — operator ratified GPU tile design → landing 3: wire + TUI additions
- Ratification: operator confirmed inspector lean 1c/2a/3a ("yes, begin now"). Design settled: VitalsPanel+KioskView surfaces, one combined kiosk tile, MAX temp / SUM watts aggregation.
- Commit: 814c1b3 — feat(vitals): DISPATCH 109 landing 3 — GPU temp/power wire additions + TUI row
- Tests: 1181 → 1184 (+3 wire-honesty tests pinning `Some`/`None`/`Some(0)` serialization)
- Gate: 221/0 unchanged (no web touch this landing — types.ts mirror + fixture + probe land in landing 4)
- Live smoke: `/api/snapshot` returns `temp_c: 68.0`, `power_w: 123.276` on release binary (RTX 3060 driver LOADED this session; BOARD's "unmeasured common" note may be stale — driver state changes between reboots, both paths are pinned by tests regardless).
- Dormant/pending: web consumers not yet built. Landings 1 + 3 both unpushed — await operator push.
- Notes: no `ux_contract` touch (WireGpu lives in `src/web/wire.rs`). No governor touch. HARD STOP #2 didn't fire — additive consumer-side change.

## 2026-07-29 — top-processes 3-panel: TUI+web parity (RAM/VRAM/CPU side-by-side)
- Rust: `top_n_by_vram_honest` fn (VRAM-honest filter — drops `None` entries before truncation), `render_three_panels` TUI renderer (horizontal split with vertical-stack fallback on narrow area.width < 3×28). Callsite in `panels/mod.rs` swapped from the cycled-by-`t` single panel to the 3-column render. Legacy `TopProcessesSort` + `t` key stay for compat but no longer drive the render.
- Wire: `WireTopProcess` + `WireTopProcesses` (repo-local, additive; `#[serde(default)]`, `#[serde(skip_serializing_if = "Option::is_none")]` on `vram_mb`). Mapper `WireSnapshot::build_top_processes` uses the same sort fns as TUI → identical ranking + PID-asc tiebreak on both surfaces.
- Web: `TopProcessesPanel.svelte` (3 sub-panels, `grid-cols-1 md:grid-cols-3` responsive), types.ts mirror, App.svelte dashboard mount (between main grid and Settings toggle).
- Tests: 1233 → 1237 (+4 top_n_by_vram_honest unit tests: drops-None-entries, empty-when-no-GPU-users, tiebreaks-by-PID, excludes-self-PID).
- Gate: 258 → 269 (+11 D115 assertions: 3-panel present, sorted-descending on RAM/CPU, honest-short VRAM, no fake 0-MB rows, responsive 3→1 tracks, empty-vram → "no GPU users" empty state).
- Live smoke: `/api/snapshot.top_processes.by_vram` returned 2 entries on host (Xorg 56MB + gnome-shell 4MB — real GPU users; honest short list held, NOT padded to 5). Screenshot confirms all 3 panels render side-by-side.
- Commit: (pending — this landing).
- Notes: no governor, no contract, no new sampling. Purely display + wire projection. HARD STOP #2 didn't fire — additive consumer-side change (D109 precedent).

## 2026-07-30 — onboarding: config discovery + `init` subcommand + friendly errors
- NEW `src/onboarding.rs`: config_search_paths (CWD → XDG → /etc), default_init_path (XDG), write_starter_config (creates dirs, refuses overwrite without --force), no_config_error_message, DEFAULT_CONFIG_TOML (heavily commented safe-off template — governor OFF, allow_no_auth=true with loud comment). 8 unit tests including "starter config parses + validates" pin.
- CLI: new `edge_monitor init [--force] [--path <p>]` subcommand. Dispatched BEFORE `load_config` (chicken-and-egg — init exists to bootstrap the config). Writes to XDG by default.
- `load_config` refactored: returns `(Config, ConfigSource)` where ConfigSource is Explicit/Discovered/Defaults{searched}. Discovery walks the new search paths in order.
- Auth-error path in main.rs: when validate_web_auth fails AND ConfigSource == Defaults (no config found anywhere), swap the auth wall for the actionable `no_config_error_message` naming exact commands (`edge_monitor init`, `--no-web`, `--config`).
- `validate_web_auth` message rewritten: named both settings verbatim inside a `[web]` block snippet, dropped "pre-D85 posture" jargon, offered `--no-web` alternative. Test `default_web_config_rejects_validate_web_auth` updated to pin the new checklist shape (both fields named + `--no-web` mentioned + no D85 language + no token value echoed).
- Live smoke verified 4 flows on a fake `$HOME`: (1) no config → actionable error with search paths + fix commands; (2) `init` → writes safe-off config to `$HOME/.config/edge_monitor/edge_monitor.toml` + prints "now run edge_monitor" reminder; (3) `init` again → refuses ("pass --force to replace"); (4) `edge_monitor` again → picks up the XDG config, starts cleanly.
- STOP #1 (auth default): took dispatch's recommended option (a) — `allow_no_auth = true` with prominent comment explaining LAN/remote risk. First-run friendliness > paper-secure default that requires token-hunting.
- Tests: 1237 → 1245 (+8 onboarding). Gate 269/0 unchanged (no web surface touched). clippy clean.
- Notes: no governor / kill-path / contract change. Starter config ships SAFE-OFF (auto_actuate=false + default_ai_action=Allow — killer inert on fresh install). HARD STOP #3 (auth-default choice) surfaced in dispatch text; operator's dispatch already recommended option (a), so no ratification round-trip needed.

## 2026-07-30 — TUI header web-link (URL shown in mission line)
- NEW `panels/header::web_display_url(IpAddr, u16)`: pure fn, tests pin: loopback IPv4/IPv6 → `localhost`, 0.0.0.0/:: → `localhost` (not the un-browsable zero-address), specific IPv4 → verbatim, IPv6 bracketed per RFC 3986. 6 unit tests.
- NEW `mission_line_text_with_web(set, n, m, Option<&str>)`: appends ` <sep> web: <url>` to the mission template when the URL is `Some(_)`. `None` (--no-web / bind failure) omits the tail entirely — no advertising a server that isn't running. 4 tests + a 5th pin that the emitted text carries NO ANSI/OSC-8 escape bytes.
- `App.web_url: Option<String>` field + setter/accessor. `ui::run` gained a `web_url: Option<String>` param; main builds it via `web_display_url(cli.bind, cli.port)` ONLY when `web_tx_for_loop.is_some()` (i.e. web spawn succeeded — not just when --no-web was absent — so a bind failure also hides the link).
- STOP #1 (OSC 8 clickability): shipped PLAIN-TEXT URL. ratatui doesn't cleanly emit OSC 8 through its `Span` layer (either filters or renders visible escape junk depending on backend); the dispatch's explicit fallback says "readable-not-clickable is fine; visible garbage is NOT." Modern terminals (iTerm2, Kitty, WezTerm, GNOME Terminal, Konsole, Windows Terminal) auto-linkify bare URLs for Ctrl-click anyway, so plain text gets ~90% of clickability for free.
- Live verify: standalone rustc of the URL fn produced the expected 4 shapes (loopback/0.0.0.0/LAN-IP/custom-port). Full TUI render not exercised (no TTY in bash tool) but the render path is `header::render → mission_line_text_with_web(…, app.web_url())` — all three seams have unit tests.
- Tests: 1245 → 1256 (+11). clippy clean. No web-render change (browser gate untouched).
- Notes: TUI-only display change. No governor, no contract, no web/wire touch.
