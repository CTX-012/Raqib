# v1.3.1 / DISPATCH 53 — gate sanity proofs

Captured at v1.3.1 ship time (`./target/release/edge_monitor --version`
= `edge_monitor 1.3.1`).

## Proof 1: default behaves as v1.3.0

`./target/release/edge_monitor --no-ui --ticks 2` (no `--config`)
runs with built-in contract defaults; tick log + RAPL "energy_uj
unreadable" line confirm the existing v1.3.0 behavior is preserved.
(See dispatch C5 report; not captured to file because the output
is the standard headless tick stream.)

## Proof 2: bad config REJECTS with operator-actionable error

[`bad_thresholds.toml`](bad_thresholds.toml) sets an inverted thermal
pair (amber=95, red=85). Running:

```sh
./target/release/edge_monitor --config bad_thresholds.toml --no-ui
```

produces:

```
Error: invalid configuration; aborting startup

Caused by:
    invalid threshold config: invalid config:
    thermal_red_c (85.0) must be > thermal_amber_c (95.0)
```

The error message NAMES the offending field AND shows actual vs
expected — the operator-actionable error contract from
`EffectiveThresholds::validate`. Reject, not silent clamp. v1.0.1
phantom-kill lesson held.

## Proof 3: valid override TAKES EFFECT (combined env + config)

A synthetic sysfs at `/tmp/ft/thermal_zone0/{type,temp}` holds a
single 60.0 °C zone. Two runs of the SAME binary against the SAME
sysfs differ ONLY in whether [`good_thresholds.toml`](good_thresholds.toml)
(override `thermal_amber_c=50, thermal_red_c=70`) is loaded:

| Config | Expected behavior | Result |
|---|---|---|
| Default (no override) | 60 < contract amber 85 → nominal → no alert | [`control_60c_no_alert.log`](control_60c_no_alert.log) — zero `alert.fire=ThermalPressure` lines |
| Override (amber=50, red=70) | 60 ≥ override amber 50 → amber → ThermalPressure fires after sustain | [`override_60c_alerts.log`](override_60c_alerts.log) — repeated `alert.fire=ThermalPressure scope=System` lines (sustain crosses, alert holds while breach holds) |

The combined v1.3.0 env-override (`EDGE_MONITOR_THERMAL_ROOT=/tmp/ft`)
+ v1.3.1 config-override (`[thresholds]`) is the end-to-end proof
that an operator's threshold override reaches the AlertState
machine on the headless surface. Same code, same hardware reading,
DIFFERENT behavior driven entirely by the config layer.

## Authority lock evidence

Neither proof crosses the actuation line. The override changes
WHAT VALUE the breach comparison reads — the comparison
result still feeds the same observe-only AlertState machinery, the
same recommendation-projection logic, the same TUI/web display
paths. No `send_sigterm`. No `--enable-governor`. No new keybinding.
Tenth observe-only confirmation.
