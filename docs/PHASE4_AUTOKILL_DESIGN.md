# edge_monitor — Auto-Kill Actuation Design (Phase 4 frontier)

**Status:** **SHIPPED — dormant + hardware-verified inert (AUDIT sweep 2026-07-15).** Auto-kill actuation completed across DISPATCH 59-81: gated SIGTERM (D80), gated SIGKILL escalation with `sigterm_grace_secs` activation (D81), `pending_kills` lifecycle drain, tripwires `send_sigterm_actuation_site_is_auto_actuate_gated` + `send_sigkill_callers_are_gated` + `default_off_emits_no_sigterm_and_no_sigkill` all green. `auto_actuate` defaults FALSE (schema-firewalled). C4 (Manual-k force-SIGKILL UX) **deferred** — see §"HARD-BLOCKING follow-up" below; it remains a HARD STOP #1 review item, not autonomously actionable. Prior status: "design locked 2026-06-05 · promotes to a `docs/PHASE4_DESIGN.md` sub-section at sign-off" (DISPATCH 59 Inspector pre-pass).
**Basis:** DISPATCH 59 Inspector pre-pass (read-only against v1.3.1 / HEAD `f3e7607`)
**Authority change:** this is the **deliberate crossing of the observe-only line** (held as a lock 9×). It is gated, off-by-default, and lands incrementally.

## The one thing that governs everything

The v1.0.1 **phantom-kill scar** is held at **three independent layers**, all currently intact:

1. **Policy default** — `safe_default().default_ai_action = Allow` (`policy.rs:59`)
2. **Severed tick path** — no production code calls `send_sigterm` (grep: zero non-self/non-test call sites)
3. **Audit silence** — `record_governor_audit` deliberately no-ops on kill verbs (`runtime.rs:1301-1321`)

Tear down any one and the other two still prevent a phantom kill. **Tearing down all three is the gate this arc walks through openly** — never silently, never as a side effect. Every step below preserves at least one layer until the operator has explicitly opted in.

## Locked decisions (Q1–Q6)

| Q | Decision | Rationale |
|---|---|---|
| **Q1 — SIGTERM site** | **A: tick-loop.** A new function adjacent to `record_governor_audit` walks `state.decisions` for `SignalTermSent` and calls `executor.send_sigterm` (+ `execute_after_grace`). | Mandated by the standing **network-never-in-safety-path** lock. Keeps `libc::kill` off the unauthenticated bind, preserves single-owner-of-`&mut Runtime`, no cross-thread mutation channel. **Web stays a policy editor, never a kill driver.** |
| **Q2 — Opt-in default** | **OFF.** `default_ai_action` stays `Allow`; actuation requires explicit `governor.auto_actuate = true`. No first-launch enable, no silent flip. | The scar. Operator must name the verb. |
| **Q3 — Sustain gate** | New `kill_sustain_secs`, default **≥10s**, validated **≥ `alert_sustain_secs`**. Exact value empirical (design/test). | A kill must never undercut alert-smoothing; a briefly-flashing breach must not kill. |
| **Q4 — Tiebreaker** | **Deterministic lowest-PID now** (explicit sort — `LifecycleSnapshot.processes` is a HashMap, order non-deterministic). Least-recent-activity is **v-next**, landing with `LiveTelemetry::last_active_at`. | Cheap, correct, auditable today; the better tiebreaker needs a timestamp field that doesn't exist yet. |
| **Q5 — Firewall teardown** | Opt-in flag (+ auth if ever B) **first**; replace doc-only Firewall 3 with an automated test **before** actuation; **Firewall 1 untouched** (actuation reads a separate `KillIntent`-style type, not `SuggestedAction`); actuation lives in `runtime.rs`/`governor/`, **never `recommend.rs`** → **Firewall 2 stays intact**. | Gate the path before it exists; preserve every lock that has zero teardown cost. |
| **Q6 — Trigger scope** | **VRAM%-first.** Thermal + RAM triggers are a **follow-up pass**. | VRAM% *is* an OOM signal → still delivers the OOM-prevention goal. Only GPU **thermal-protection** defers. Avoids landing two large changes in one ratification. |

## The three firewalls (status + fate)

| # | Firewall | Where | Fate in this arc |
|---|---|---|---|
| 1 | Type (`SuggestedAction` discriminator-not-callable) | `ux_contract/src/recommendation.rs:97-112` + pin test `:281-286` | **Untouched** — actuation reads a separate type |
| 2 | Wiring (forbidden-token scan of `recommend.rs`) | `tests/recommendation_observe_only_guard.rs:56-105` | **Intact** — actuation never touches `recommend.rs` |
| 3 | Config schema (documentary only — weakest) | `thresholds.rs:46-51` + v1.3.2 `WorkloadRule` | **Replaced by an automated test BEFORE any actuation** |

## Prerequisite invariant (DISPATCH 62-E)

Before actuation wiring (step 5) can be safe, `GovernorExecutor::evaluate()` must short-circuit PIDs already on the kill queue. Today it re-emits `SignalTermSent` every tick for the same PID; once the tick-loop actuation site reads decisions, a single stubborn process drains the entire 3-kills/60s budget over three ticks and starves other AI processes. Fix: add `KillAction::AlreadyPending`, returned for `pending_kills.contains(pid)` before the per-process evaluate branch — mirrors the existing `AlreadyExited` shortcut. This lands as **step 0 (prerequisite)** below, before or with step 3.

## Build sequence (9 steps, gated, bisectable)

Steps **1–2 are observe-only-safe and fire now** (DISPATCH 60). Steps **3+ are gated behind v1.3.2 ratification.** **Actuation goes live only at step 5, behind `auto_actuate == true` (default false)** — shipped-but-dark until opt-in.

| Step | Work | Touches | Actuation live? |
|---|---|---|---|
| **0 (PREREQ)** | **`KillAction::AlreadyPending`** — `evaluate()` returns it for `pending_kills.contains(pid)` *before* the evaluate branch, symmetric with the existing `AlreadyExited` shortcut. Without it a stubborn post-SIGTERM PID re-emits every tick and drains the 3/60s rate-limit budget alone, starving other kills. (DISPATCH 62-E, latent today, bites at step 5.) | `governor/executor.rs` | no |
| 1 | `tests/config_schema_firewall.rs` — token-list guard mirroring the recommendation guard; replaces the documentary Firewall 3 | tests | no |
| 2 | `governor.auto_actuate: bool` config field, **default false**; no actuation code | config | no |
| 3 | Widen `evaluate()` to accept a **threshold-breach projection** (M4 option b), **VRAM%-only** | `governor/`, runtime projection | no |
| 4 | Deterministic **lowest-PID** sort in the candidate ordering | `governor/` | no |
| 5 | ✅ DISPATCH 80 — **Tick-loop actuation site** lives at `runtime::Runtime::record_governor_audit`; walks `state.decisions`, calls `send_sigterm` only if `governor.auto_actuate`, mirrors `state.audit` with `KillSource::Automated`, populates `governor_killed_pids` for exit attribution. Workspace-wide tripwire (`send_sigterm_actuation_site_is_auto_actuate_gated`) pins **exactly one** runtime caller AND its `auto_actuate` proximity. Default-OFF guard `default_off_emits_zero_kills` pins layer 2 of the v1.0.1 scar. | `runtime.rs` | **yes (gated, default-off)** |
| 6 | ✅ DISPATCH 81 — **`execute_after_grace` SIGKILL escalation** in the same gated loop. After the SIGTERM pass, `record_governor_audit` calls `governor.execute_after_grace()` (PID-reuse guard engages via `send_sigkill`); each result audits with `KillSource::Automated` (success: `SendSigkill`; refused: `PidReusedAborted`; OS error: `SendSigkill` failure). Activates `policy.sigterm_grace_secs` (was dead config). `pending_kills` entries cleared on PID exit at the lifecycle drain. Tripwire `send_sigkill_callers_are_gated` pins one internal caller (`execute_after_grace`'s loop) + one runtime orchestrator (`execute_after_grace` itself), both auto_actuate-gated. Headline guard `default_off_emits_no_sigterm_and_no_sigkill` extends D80's default-OFF invariant to cover SIGKILL too. **Manual-k force-SIGKILL UX (C4) deferred — see HARD-BLOCKING FOLLOW-UP below.** | `runtime.rs`, `executor.rs` (test-only `insert_pending_kill_for_test`) | **yes (gated, default-off)** |
| 7 | ✅ DISPATCH 80 (landed with step-5) — `governor.kill_sustain_secs` (default 10 s), validated ≥ `thresholds.alert_sustain_secs` at config load; actuation site reads per-PID `breach_since` (refreshed in `tick()`) and holds for the window. Q3 sustain gate is live behind `auto_actuate`. | config, `runtime.rs` | yes (gated) |
| 8 | ✅ DISPATCH 84 — Per-PID RAM% added to `ThresholdBreach` (`ram_pct`, `ram_breached`). Host-level thermal lives on a new sibling `HostBreach { thermal_breached, max_temp_c, hottest_zone }` — max-across-zones aggregation surfaces the hottest zone label. `evaluate()` reads BOTH projections; gate widens to `(vram_breached OR ram_breached OR host_thermal_breached) AND policy permits`. `breach_since` extended to track RAM-breachers AND (when thermal breached) all AI PIDs so the sustain gate satisfies the Q6 thermal-shed-load case. Observe-only preserved: NO new actuation. Default-off invariant unchanged (`default_off_emits_no_sigterm_and_no_sigkill` STILL passes). Schema firewall green: `ram_critical_pct` and `thermal_red_c` already existed in `EffectiveThresholds` (numeric tuning fields, NOT action verbs). Narrow-projection compile-time pin extended to accept `&HostBreach` (widening the projection TYPE is allowed; widening to `&RuntimeState` is not). | `governor/threshold_breach.rs`, `governor/executor.rs`, `runtime.rs` | yes (gated) |
| 9 | (v-next) `LiveTelemetry::last_active_at` + least-recent-activity tiebreaker (Q4) | runtime, `governor/` | yes (gated) |

Tester validation before tagging the actuation release: real OOM-pressure scenario, opt-in toggle on/off, sustain gate, **phantom-kill regression guard** (default-off path fires no kill), and the web-render gate.

## Web's role (explicitly bounded)

Web becomes a **policy editor** — thresholds, sustain, the `auto_actuate` toggle (written to TOML, read by the tick loop). Web **never** drives a kill. The mutation endpoint + auth (Phase 4 settings work) carries *config*, not *actuation verbs*. Current web surface confirmed read-only/unauthenticated (routes enumerated `web/mod.rs:77-84`; "NO AUTH, trusted LAN only" `main.rs:278-284`).

## HARD-BLOCKING follow-up — DISPATCH 81 / C4 (manual-k force-SIGKILL UX)

DISPATCH 81's design called for the manual-k path to share the SAME escalation
machinery (D72 Position A): first `k` sends SIGTERM (as today); the
`kill_confirm` card flips to a "waiting {grace}s for graceful shutdown…
[Enter to force-SIGKILL]" state; if the PID is still alive after grace, the
operator's Enter triggers `send_sigkill` (consent-gated, not auto_actuate).
That was deferred. Why it's HARD-BLOCKING:

1. **Contract amendment required.** The new operator-facing strings
   ("waiting Ns…", "[Enter to force-SIGKILL]", a cancellation cue) live in
   `~/ux_contract` and Agent A owns that crate. CLAUDE.md forbids editing
   it from this repo without an Amendment Request.
2. **Card state-machine extension.** `KillConfirmCard` today is a fixed
   "Enter = confirm SIGTERM" snapshot; adding a post-SIGTERM "waiting →
   force" sub-state means a state enum on the card (Confirm → Waiting →
   ForceConfirm) and corresponding `apply_action` Enter/Esc dispatch
   updates in `src/ui/mod.rs`.
3. **Manual-kill must populate `pending_kills`.** Today `manual_kill` calls
   `ManualKiller::kill_sigterm` which uses `libc::kill` directly, NOT
   `governor.send_sigterm`. The shared machinery requires routing manual
   kills through `send_sigterm` (or a parallel pidfd-capture path) so the
   PID-reuse guard's identity tokens are available at force-SIGKILL time.
4. **Force-SIGKILL caller adds a new entry to the `send_sigkill_callers_are_gated`
   tripwire** — and the proximity check must be extended to pin "operator
   consent (an Enter dispatch from a `Waiting` card state)" instead of
   `auto_actuate`.

Until C4 lands, operators who `k` a SIGTERM-ignoring process (ollama is the
live test case — see "k didn't kill ollama" finding) get a stuck SIGTERM
with no force-kill follow-through in the TUI. Workaround today: a second
manual kill resends SIGTERM (still ignored) or `kill -9` from a shell.

## Out of scope here
- GPU thermal-protection trigger (Q6 follow-up)
- RAM trigger (Q6 follow-up)
- Activity-aware tiebreaker (Q4 v-next)
- Web settings-UI build (separate Phase 4 settings dispatch)

---

*Promotes to `docs/PHASE4_DESIGN.md` at operator sign-off. Steps 1–2 = DISPATCH 60 (observe-only-safe). Steps 3+ gated behind v1.3.2 ratification.*
