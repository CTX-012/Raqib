# Phase 4 Design — config-driven policy + INA3221 + Jetson pass

> **Canonical Phase 4 scope.** Promoted to `docs/` per the
> [`docs/ROADMAP.md`](ROADMAP.md) "plan-doc discipline" standing rule —
> the design source survives in version control, not in ephemeral
> chat state.
>
> **Built from:**
> [`tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_SCOPING.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_SCOPING.md)
> (deferred-backlog enumeration) +
> [`tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md)
> (impl-shape mapping) +
> operator decisions from DISPATCH 47 §7 and DISPATCH 49.
>
> Update via PR with each Phase 4 sub-release so the design + code
> stay version-controlled in sync (same discipline as
> [`docs/PHASE3_DESIGN.md`](PHASE3_DESIGN.md)).

---

## 1. What Phase 4 IS

Phase 4 makes the deployment **tunable** — thresholds, sampler
cadences, per-workload alert/recommendation overrides, and INA3221
power-rail visibility — and closes the Jetson-deferred validation
gap.

Four-step incremental cadence (mirrors Phase 3's v1.1.11 → v1.2.0):

| Sub-version | Scope | Status |
|---|---|---|
| **v1.3.0** | `EDGE_MONITOR_THERMAL_ROOT` env override | **shipped** |
| **v1.3.1** | `[thresholds]` deployment overrides (corrigendum: NO `[samplers]`; see v1.3.1 sub-section) | **shipped** |
| **v1.3.2** | `[[workloads]]` per-workload rules + suppression flags | **shipped** |
| v1.3.3 | INA3221 power rails (consumes `ux_contract` v0.3.16) | scoped |
| Jetson pass | Empirical validation on Orin | scoped (post-v1.3.3) |

## 2. What Phase 4 is NOT

**OBSERVE-ONLY (the seventh explicit reaffirmation of the
authority lock).** Phase 4 makes thresholds/intervals/rules
TUNABLE; it does NOT add act-on-rule.

Concretely:

- `default_ai_action = Allow` stays unchanged (the v1.0.1 flip
  holds).
- `send_sigterm` stays manual-only (no tick-path wiring).
- No `--enable-governor` flag.
- No new actuation keybinding (the existing `k` → kill_confirm
  card → SIGTERM path is the only action surface, unchanged
  since v1.0.0).
- `[[workloads]]` rules have NO `action_on_breach` field, NO
  `auto_kill` field, NO `priority` field. **Schema-level
  firewall**: even an operator editing TOML cannot configure
  auto-action because the field doesn't exist.

The authority audit at
[`INSPECTOR_PHASE4_IMPL.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md)
§8 enumerates every proposed Phase 4 element and confirms each is
observation- or display-side. The audit findings carry forward as
the standing position; Phase 4 doc updates re-cite rather than
re-deciding.

## 3. Operator-locked decisions (binding)

The following decisions were locked by operator at DISPATCH 47 §7
and reaffirmed at DISPATCH 49. They are inputs to v1.3.1 / v1.3.2
/ v1.3.3, not open questions.

### Q1 — contract-vs-config tension: **OPTION (iii) HYBRID**

Three categories of contract constants, each with its own policy:

| Category | Examples | Policy |
|---|---|---|
| **Wire-format caps** | `REC_MAX_VISIBLE` (3), `REC_TARGETS_MAX` (3), `ALERT_MAX_VISIBLE` (3), `ACTIVITY_FEED_*_MAX` (5/50/12) | **ABSOLUTE** — govern wire layout; changing them ripples to consumers and breaks compatibility. Not overridable. |
| **Deployment thresholds** | `THERMAL_AMBER_C` (85), `THERMAL_RED_C` (95), `VRAM_ATTENTION_PCT` (85), `VRAM_CRITICAL_PCT` (95), `RAM_ATTENTION_PCT` (90), `RAM_CRITICAL_PCT` (95), `KV_ATTENTION_PCT` (80), `ALERT_SUSTAIN_SECS` (5) | **DEFAULTS** — contract constants are the deployment-wide default; config-side `[thresholds]` section may shadow per-deployment. |
| **Implementation thresholds** | `BAR_*`, `KILL_ARM_WINDOW_SECS`, `BASELINE_WARMUP_SECS` | **ABSOLUTE** — govern UI render layout and timing semantics that must be consistent across consumers. Not overridable. |

Implication: v1.3.1 ships the `[thresholds]` config section
shadowing only the **deployment thresholds** category. Wire caps
and implementation thresholds are NOT exposed. This requires no
contract semantic change (the deployment thresholds were already
documented as "consumer classifies at render-time"; the override
just shifts the classifier's threshold input from contract const
to merged-config value with contract-default fallback).

### Q2 — per-workload match shape: **EXACT name match**

`WorkloadRule::name` is an exact-string match against the process
`comm`. Regex / glob support is **explicitly deferred** to a
future sub-version if operator finds exact-name limiting.
Reversible later; starting exact-name keeps the rule semantics
unambiguous.

### Q3 — suppression flags: **BOTH, INDEPENDENT**

`suppress_alerts: bool` and `suppress_recommendations: bool` are
both present on `WorkloadRule` and independently togglable.
Rationale: some operators want the sustained-pressure signal
(alert) but not the display noise (rec); others want neither.
Per-workload choice.

### Q4 — additional per-workload rule fields: **DEFER TO v1.4.x**

Tempting future fields (`display_name`, `category_override`)
expand surface without addressing a current ask. v1.3.2 ships
ONLY the thresholds + suppress flags. Additions arrive when an
operator surfaces a concrete need.

### Q5 — Agent A dispatch for `ux_contract` v0.3.16: **PARALLEL** (now done)

Agent A shipped v0.3.16 (`HostVitals.power_rails: Vec<PowerRail>`)
ahead of v1.3.3 implementation. The v1.3.0 shipping in this
release carries forced compat for v0.3.16 — every `HostVitals
{ thermal_zones: ... }` initializer in the consumer codebase
now also passes `power_rails: Vec::new()`, matching the
contract's "empty rails is valid" semantic. INA3221 collection
itself still lands in v1.3.3.

### Q6 — Jetson hardware pass owner: **Tester (or operator hand-off)**

Live thermal alerts + recs + INA3221 + multi-zone validation on
actual Orin hardware. Validation artefacts will land at
`tests/empirical/v1_3_3/jetson_pass/`. Specific ownership TBD at
the v1.3.3 ship boundary.

### Q7 — sub-version cadence: **INCREMENTAL**

Four sub-versions (v1.3.0 → v1.3.3) mirror Phase 3's v1.1.11 →
v1.2.0 cadence — smaller PRs, cleaner bisect, Inspector audits
between sub-versions on demand.

## 4. Sub-version detail

### v1.3.0 — `EDGE_MONITOR_THERMAL_ROOT` env override (shipped 2026-06-03)

Shipped via DISPATCH 50. ~35 LoC source + test.

`collect_host_vitals()` in
[`src/platform/host_vitals.rs`](../src/platform/host_vitals.rs) now
reads the `EDGE_MONITOR_THERMAL_ROOT` env var (when set) as the
thermal sysfs root, defaulting to `/sys/class/thermal` when unset.
Invalid override paths degrade to empty thermal_zones (no crash —
same shape as a missing real sysfs root).

**The unblock**: x86 dev hosts run cold and never light up the
v1.1.12 thermal alert + v1.2.0 ThermalPressure rec paths in
production. Pointing the env var at a tempdir of synthetic
`thermal_zoneN/{type,temp}` files drives the whole pipeline
end-to-end without Jetson hardware. A Tester or operator with no
Jetson at hand can now validate the surfaces shipped in v1.1.12
and v1.2.0 from any dev host.

Test:
`thermal_root_env_override_redirects_collection` in
[`src/platform/host_vitals.rs`](../src/platform/host_vitals.rs)
covers the redirect AND the "invalid override degrades to empty"
path. Env-var save/restore inside the test prevents pollution of
parallel tests.

Forced compat — `ux_contract` v0.3.16 landed parallel to this
release (Agent A's Q5 ack):
`HostVitals.power_rails: Vec<PowerRail>` is now required at
construction. Six initializer sites in `src/` adapted to pass
`power_rails: Vec::new()` — the contract's documented valid empty
state for hosts without an INA3221 driver. INA3221 collection
itself still lands in v1.3.3.

Authority: pure observation-path config. Reads a path → reads
thermal from it. No actuation surface added.

### v1.3.1 — deployment threshold + sampler overrides (scoped)

Per impl §2 + Q1 hybrid: new `[thresholds]` + `[samplers]` config
sections. `[thresholds]` shadows the contract's deployment-default
thresholds (THERMAL_AMBER_C, THERMAL_RED_C, VRAM_*, RAM_*,
KV_ATTENTION_PCT, ALERT_SUSTAIN_SECS). `[samplers]` exposes the
in-source sampler-side constants (ROS2 cadences,
EMBEDDINGS_ACTIVE_CPU_PCT).

Both sections are `#[serde(default)]` — old TOMLs continue to load.
Contract-const fallback when a config field is None: the existing
classifier reads from `ux_contract::thresholds::*` get wrapped to
read from the merged config (resolved at config-load time, not at
each tick).

Estimated size: ~150-250 LoC. No contract change needed.

### v1.3.2 — per-workload rules (shipped 2026-06-05 via DISPATCH 57)

Per impl §5 + Q2 exact-name + Q3 both-suppress-flags + Q4 minimal
fields. **As-shipped** schema (note: trimmed from the pre-ship
sketch — `WorkloadThresholds` per-workload threshold overrides
were dropped from v1.3.2 scope per the C2 field-count discipline;
they remain a v1.4.x candidate behind a separate operator
decision):

```rust
pub struct WorkloadRule {
    pub name: String,                  // exact /proc/<pid>/comm match
    pub suppress_alerts: bool,
    pub suppress_recommendations: bool,
    // NO action_on_breach, NO auto_kill, NO priority — schema firewall
    // NO thresholds — Q4 LOCKED, deferred to v1.4.x if needed
}
```

Exactly 3 fields. Adding a 4th — even a benign one — trips the
new field-count guard
`tests/workload_rule_field_count_guard.rs::workload_rule_has_
exactly_three_fields`. The forcing function is the count, not
the name: the existing DISPATCH 60 C1 name-based firewall
catches action-verb additions; the count guard catches even
benign-shaped additions like `display_name` or
`category_override`. Together they pin both the schema shape
and the schema vocabulary.

**Consumption (as-shipped)**:

  * `runtime::observe_alerts` (C3): per-PID metric-driven
    `observe` calls (VRAM / KV / GovernorArmed) are skipped
    when the matched rule's `suppress_alerts == true`. The
    OOM and `WorkloadExited` exit-path alerts are
    STRUCTURALLY un-gated — they fire through
    `observe_exit_alert`, which doesn't consult the rules.
    The OOM carve-out is therefore architectural rather than
    a conditional inside the loop (the Inspector truth table
    assumed option (i); we shipped option (ii) on the lean
    that the structural carve-out is harder to silently break
    in future refactors).

  * `recommend::project_one` (C4): the projector returns
    `None` for any alert whose `entry.workload_name` matches a
    rule with `suppress_recommendations == true`. Q3 (ii)
    LOCKED: this includes OOM recs — the alarm fires
    (un-suppressable), but the "Consider restarting" text can
    be muted.

  * System-scope alerts (RAM, ThermalPressure) have an empty
    `workload_name` by construction (`WorkloadRef::system()`),
    and the resolver rejects rules with empty names, so the
    lookup never matches. System-scope alerts and recs are
    unaffected by any `[[workloads]]` rule.

**Resolver-time validation** (`Config::resolve_workload_rules`):

  * empty `name`         → reject (`RuntimeError::Config`)
  * duplicate `name`     → reject (ambiguous flag-set winner)
  * `name.len() > 15`    → warn-but-accept (Q5: kernel
    `TASK_COMM_LEN` is 16, but `prctl(PR_SET_NAME)` self-set
    is a known escape)
  * name matches no current workload → accept silently (the
    rule lights up when the workload appears)

**Startup audit trail** (Point A closure): the resolver emits
`tracing::info!` listing the loaded rule count and which names
are suppressing alerts. A headless operator running with
`--no-ui` sees in `journalctl -u edge_monitor.service`:

```
INFO ... [[workloads]] rules loaded; OomDetected is
     un-suppressable regardless count=2
     suppressing_alerts=["ollama", "rviz2"]
```

When BOTH flags are true on a rule, a per-rule `tracing::info!`
notes the redundancy (Q6) — naming the simpler shape helps
operators spot the typo path.

**Companion fix (DISPATCH 57 C1)**: web `/assets/*` responses
now carry `Cache-Control: no-cache` + `ETag` headers, closing
the v1.3.x-line "web-zero" staleness bug (Tester DISPATCH 56).
Conditional-GET 304 short-circuiting deferred; correctness
(no stale bundle after rebuild) is the v1.3.2 win.

Actual size: ~600 LoC source + tests (vs ~200-300 LoC pre-
estimate). Drivers: the C2 firewall test + the C3/C4 cross-
test coverage (suppress_alerts × OOM × system-scope and
suppress_recommendations × OOM × system-scope) added more test
LoC than expected; the source additions stayed close to estimate.

### v1.3.3 — INA3221 power rails (scoped, consumes `ux_contract` v0.3.16)

Per impl §4:

- `src/platform/ina3221.rs` (new, ~80 LoC) reads sysfs paths at
  `/sys/bus/i2c/drivers/ina3221/<bus-addr>/hwmon/hwmon<N>/` —
  voltage (`in<channel>_input`, millivolts) × current
  (`curr<channel>_input`, milliamps) ÷ 1000 = milliwatts per rail.
  Rail label from `rail_name_<channel>` file. x86 has no INA3221 →
  `collect_from_root` returns empty per the existing missing-root
  pattern.
- A parallel env override `EDGE_MONITOR_INA3221_ROOT` (mirrors
  v1.3.0's thermal override) for x86 synthetic testing.
- `WirePowerRail` mirrors `ux_contract::PowerRail`; `WireVitals.power_rails`
  with `#[serde(default)]` for backward compat.
- TUI render: new row on the vitals panel, hidden when empty.
- Svelte: matching mirror in `VitalsPanel.svelte`.

Estimated size: ~180 LoC consumer.

### Jetson pass — empirical validation (scoped, post-v1.3.3)

Live thermal alerts + ThermalPressure recs + INA3221 power on
actual Orin hardware. Artefacts at `tests/empirical/v1_3_3/jetson_pass/`.
Multi-zone amber/red rendering, real power-rail values, end-to-end
recommendation surface validation under real heat.

## 5. `ux_contract` prereqs

| Contract version | Provides | Consumed by |
|---|---|---|
| v0.3.13 | `HostVitals` + `ThermalZone` + thermal thresholds | v1.1.12 (Phase 3) |
| v0.3.14 | `Recommendation` + `SuggestedAction: Copy` firewall + display templates | v1.2.0 (Phase 3) |
| v0.3.15 | `AlertId::ThermalPressure` + template | v1.2.0 (Phase 3) |
| **v0.3.16** | **`HostVitals.power_rails: Vec<PowerRail>` + `PowerRail` struct** | v1.3.0 (forced compat) + v1.3.3 (collection) |

No `ux_contract` change is required for v1.3.1 (deployment
thresholds are config-side overrides of existing contract
defaults; the contract semantic doesn't change). No change for
v1.3.2 either (per-workload rules are pure consumer schema).

## 6. Non-goals (explicit)

Out of Phase 4 entirely (carry-forward from Phase 3 non-goals +
authority lock):

- **Automatic actuation of any kind** — the seven-reaffirmation
  observe-only lock. Not in v1.3.x. Crossing the line is its own
  decision track per
  [`docs/ROADMAP.md`](ROADMAP.md) EXPLICITLY NOT DOING.
- **Regex/glob workload match** — Q2 exact-name only. v1.4.x if
  needed.
- **Per-workload action fields** — schema firewall; no
  `action_on_breach`, `auto_kill`, `priority`. v1.4.x if a
  separate authority decision opens the question.
- **Per-workload thermal threshold overrides** — thermal is
  host-scope; per-PID thermal makes no semantic sense.
- **`display_name` / `category_override`** — Q4 defer.
- **NVML temperature / power reads as INA3221 replacement** — INA3221
  is the canonical Jetson power source; NVML is for GPU-internal
  metrics, separate path.

## 7. Process notes

- This file is the canonical Phase 4 scope. Update via PR with
  each v1.3.x sub-release.
- Operator decisions §3 are BINDING for v1.3.1 / v1.3.2 / v1.3.3.
  If implementation reveals a gap (a constant that should have
  been classified differently in Q1, a missing field need that
  Q4 deferred), surface as a Contract Amendment Request or scope
  re-open dispatch — don't silently drift.
- The DISPATCH 47 / 48 / 49 / 50 commit history is the
  authoritative trail; CHANGELOG entries per sub-version are the
  per-ship breakdown.
- **`ux_contract` changes go through Agent A.** v0.3.16 was
  Agent A's parallel dispatch (Q5 ack). v0.3.17+ if needed
  follows the same flow.

## 8. v1.3.1 sub-section (shipped)

v1.3.1 shipped the `[thresholds]` deployment-overridable defaults
layer per DISPATCH 53. 5 atomic commits (C1 resolver, C2 wiring, C3
AlertState clean break, C4 docs, C5 chore). 970 tests passing
(was 952 baseline). Authority lock held — tenth observe-only
confirmation. See `CHANGELOG.md [1.3.1]` for the per-component
breakdown.

### `[samplers]` corrigendum

DISPATCH 48's sub-version table at §1 originally listed v1.3.1 as
"`[thresholds]` + `[samplers]` deployment overrides". DISPATCH 52
Inspector pre-pass surfaced the contradiction: under the locked
hybrid model (operator Q1 at §3 above), every sampler-side constant
is **class-3 ABSOLUTE**:

- `ROS2_ECHO_PROBE_INTERVAL` and `ROS2_ACTIVITY_STALENESS` — pinned
  by the v1.1.9 leak-fix cadence invariant
  (`ROS2_ECHO_PROBE_INTERVAL * 2 <= ROS2_ACTIVITY_STALENESS`),
  which is asserted by an existing test. Loosening either would
  re-open the close()-volume leak that took 9 dispatches to close.
- `ROS2_SHELLOUT_TIMEOUT` — v1.1.6 Humble-compat fix, tied to
  `ros2 topic echo --once` startup time on the empirical host.
- `EMBEDDINGS_ACTIVE_CPU_PCT` — P5 DISPATCH 9B VALIDATED value;
  bimodal calibration pinned by `bimodal_thresholds_match_empirical_values`.

**Resolution**: v1.3.1 ships `[thresholds]` ONLY. `[samplers]` does
not exist. The sub-version table in §1 has been updated. If a future
deployment hits a real need to tune one of the class-3 constants,
the path is a focused dispatch that re-classifies that specific
constant — not a `[samplers]` catch-all that invites future drift.

### What v1.3.1 changed in the schema

The `[thresholds]` section has exactly 9 fields, all `Option<>` (per
the v1.1.11 / v1.1.12 / v1.2.0 additive-config pattern):

```toml
[thresholds]
thermal_amber_c    = ...   # default 85.0; tighter for Jetson Orin
thermal_red_c      = ...   # default 95.0; must be > thermal_amber_c
vram_attention_pct = ...   # default 85.0
vram_critical_pct  = ...   # default 95.0; must be >= vram_attention_pct
ram_attention_pct  = ...   # default 90.0
ram_critical_pct   = ...   # default 95.0; must be >= ram_attention_pct
kv_attention_pct   = ...   # default 80.0
kv_critical_pct    = ...   # default 95.0; must be >= kv_attention_pct
alert_sustain_secs = ...   # default 5; in 1..=600
```

See `docs/configuration.md` for the operator-facing reference, and
`src/thresholds.rs::EffectiveThresholds::resolve` for the
validation implementation. Bad TOML rejects at `Runtime::new` with
an operator-actionable error naming the field — the eighth-times-held
no-silent-clamp discipline.

### Authority lock evidence

Per the audit table in DISPATCH 52 §8, every v1.3.1 element is
observation-side or display-side. The schema-level firewall (no
`action_on_breach` field anywhere in `ThresholdsConfig`) makes
auto-action impossible to configure from TOML. **Tenth observe-only
confirmation.** `send_sigterm` stays manual-only; the existing `k` →
`kill_confirm` card → SIGTERM path is the ONLY actuation surface,
unchanged since v1.0.

### Ships next

v1.3.2 — `[[workloads]]` per-workload threshold overrides +
`suppress_alerts` / `suppress_recommendations` flags. Inspector
pre-pass on the impl shape will fire once the v1.3.1 Tester
validation gate clears (operator Q7 — Tester validates v1.3.1
before v1.3.2 starts).
