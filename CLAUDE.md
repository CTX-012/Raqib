# CLAUDE.md — edge_monitor operating rules

You are operating **autonomously** on edge_monitor. The human checks in at milestones, not every step. That trust is conditional on the rules below. Follow them exactly.

## What this project is
A Rust TUI + Svelte web monitor/governor for AI workloads (ollama, claude agents, ROS2, vLLM) on a shared RTX 3060 Ubuntu host. It can autonomously KILL processes. The web companion serves on :7070. Repo layout: Rust in `src/`, web in `web/`, the contract crate is a SIBLING repo at `../ux_contract` (you may NOT edit it — see below).

## The prime directive: dormant-before-live, verify-before-trust
Every capability ships **dormant first** (gated off, invisible), gets **proven**, then is **exposed deliberately**. Never wire a live consumer to unproven machinery. This is how the whole project was built and why it never thrashed.

## 🛑 HARD STOPS — surface to the human, do NOT auto-proceed, even though permissions allow it

Permissions guard destructive *shell*. These rules guard dangerous *judgment*. When any of these is true, STOP and write a note to the human in `docs/state/PENDING.md`, then wait:

1. **The governor / kill / actuation path.** ANY change under `src/governor/`, or to `send_sigterm`/`send_sigkill`/`execute_after_grace`/`libc::kill`/threshold-evaluation/auto_actuate logic, or to the kill decision, sustain gates, or identity guards. Even a "cosmetic" edit there. The kill path is NEVER touched autonomously — it is the one thing that can do irreversible harm. Surface, describe the change, wait.
2. **A contract change is needed.** If the work needs a new wire type, a new `/api` endpoint, or any edit to `../ux_contract`, you CANNOT do it (permissions deny it, correctly). Write a Contract Amendment Request (CAR) to `docs/state/PENDING.md` describing the exact types/constants needed, and STOP. The human routes it.
3. **A design decision that isn't already ratified.** If the task requires choosing between materially different approaches that a doc doesn't already settle (e.g. "which 5 modes", "settings in dashboard vs modal", "merge these two data streams or keep separate"), STOP. Write the options + your recommendation to `docs/state/PENDING.md`. Do not pick a product/UX direction on your own authority — propose, don't decide.
4. **A destructive or irreversible action** the permissions somehow didn't catch (deleting tracked data, rewriting history, anything you can't `git revert`).
5. **You're about to enable auto_actuate=true, arm the killer, or make a kill actually fire.** Never. This requires explicit human action at the console with a verified VRAM signal and a disposable target. Surface.

## Enabled autonomy — do these freely (within the permission allowlist)
- Read/grep/explore the codebase (no limit — explore before acting)
- Write/edit files under `src/`, `tests/`, `web/src/`
- `cargo build`, `cargo test`, `cargo clippy` — run them, read the output, iterate on errors until green
- `npm --prefix web run build`, `npm --prefix web run test:browser` — build the bundle, run the browser render-gate
- Run the binary (`./target/release/edge_monitor`) for smoke checks
- Web search for current information
- `git status/diff/log/add` freely; `git commit`/`push` will prompt (ask rule) — that's the milestone checkpoint

## The build loop (follow this shape for every task)
1. **Read first.** Read `docs/state/BOARD.md` (current state), the relevant design doc in `docs/`, and the code you're about to touch. Read the SKILL/tripwire tests that guard the area.
2. **Design-first for anything non-trivial.** If there's no design doc for the feature, that's a HARD STOP #3 — surface, don't design-and-build in one shot. If a design doc exists, follow it exactly; deviations surface back to the doc.
3. **Build dormant.** New capability lands gated-off / consumer-less first. Add the machinery, don't wire the live consumer yet.
4. **Test hard.** Write tests that pin the behavior AND the invariant (a "nothing-wired-yet" or "still-gated" tripwire, like the existing ones). Run `cargo test` + `test:browser`. Green before proceeding.
5. **Convert tripwires, don't delete them.** When an invariant evolves (e.g. a read path opens that was forbidden), CONVERT the guarding test to pin the NEW shape (reads-only-here), never delete it. See `history_capture_is_wired_exactly_once_in_runtime` for the pattern.
6. **Verify live when it's a behavior/render change.** Build the release binary, run it, confirm the actual behavior. Web render changes especially: the `test:browser` gate (currently 221 assertions) must stay green AND the change should render correctly. Note: the human milestone-verifies; you self-verify first and report what you saw.
7. **One landing = one commit, revert-safe.** Keep each change atomic. The commit prompt is the human's milestone gate — write a clear commit message describing what shipped, what's still dormant, and what (if anything) is pending human action.
8. **Update the journal.** After each landing, append to `docs/state/JOURNAL.md` and update `docs/state/BOARD.md`. This is how context survives between your sessions — WRITE IT DOWN, incrementally, because you won't remember next session.

## Invariants that must never break (the firewalls + tripwires)
Run `cargo test` — these must stay green. If a change would break one, either the change is wrong or the invariant genuinely evolved (convert the tripwire, HARD STOP if it's kill-path):
- SCHEMA firewall (config has no action-verb fields) — 5 tests
- The governor observe-only + kill-gating tripwires (`send_sigterm_actuation_site_is_auto_actuate_gated`, `send_sigkill_callers_are_gated`, etc.) — **if these change, HARD STOP #1**
- History capture wiring (`history_capture_is_wired_exactly_once_in_runtime`)
- The browser render-gate (`npm --prefix web run test:browser`, 221 assertions) — must stay green through any web change; extend it when you add render surface

## The VRAM honesty rule (load-bearing on this host — the GPU driver is unloaded, so "unmeasured" is the COMMON case)
"No VRAM measurement" is NOT "0 MB". Everywhere — captured samples, wire serialization, charts, big kiosk tiles, sparklines — unmeasured VRAM must render as a gap / "—" / absent, NEVER a 0 or 0-line. A giant "0% VRAM" on a wall monitor is a lie (reads as "GPU idle"). This discriminator has survived every layer of the codebase; do not collapse it to a lying zero.

## Contract discipline (even though you can't edit the contract)
When a CAR is needed (HARD STOP #2): the human/Agent A edits `../ux_contract`, bumps the version, tags AND pushes it (no orphan tags — a committed-but-unpushed tag was a past bug). You consume the new symbols after they land (path-dep picks them up on next `cargo build`). Never assume a contract symbol exists before it's tagged.

## Style
- Terse, direct. The operator prefers immediate execution and all-caps emphasis on what matters.
- When you finish a task, report: what changed, tests delta, what's dormant, what's pending human action. Don't over-explain.
- When you hit a HARD STOP, be LOUD about it in PENDING.md — that's the whole safety mechanism.
