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
| v1.3.1 | `[thresholds]` + `[samplers]` deployment overrides | scoped |
| v1.3.2 | `[[workloads]]` per-workload rules + suppression flags | scoped |
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

### v1.3.2 — per-workload rules (scoped)

Per impl §5 + Q2 exact-name + Q3 both-suppress-flags + Q4 minimal
fields:

```rust
pub struct WorkloadRule {
    pub name: String,                           // exact match
    #[serde(default)]
    pub thresholds: Option<WorkloadThresholds>, // per-workload override
    #[serde(default)]
    pub suppress_recommendations: bool,
    #[serde(default)]
    pub suppress_alerts: bool,
    // NO action_on_breach, NO auto_kill, NO priority — SCHEMA FIREWALL
}

pub struct WorkloadThresholds {
    pub vram_attention_pct: Option<f64>,
    pub ram_attention_pct: Option<f64>,
    pub kv_attention_pct: Option<f64>,
    // NOT thermal — thermal is host-scope, not per-workload
}
```

Consumption at `runtime::observe_alerts` (alert observation) and
`src/recommend.rs::project_one` (rec projection): rule lookup by
exact `comm` match; if found, use the override threshold (with
contract fallback for None fields) and honor the suppress flags.

Estimated size: ~200-300 LoC. No contract change needed.

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
