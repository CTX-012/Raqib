# Render-adversarial fixture set — DISPATCH 87

Standalone JSON fixtures capturing the exact wire-snapshot shapes
that broke the web dashboard render during this session. Each file
is a full `WireSnapshot` — what `/api/snapshot` returns — designed
to stress one or more `{#each}`-key collision modes the Svelte
components had to fix.

The fixtures are intentionally **plain JSON files**, not Rust
literals. The reuse contract: a future headless-browser gate must
be able to `fetch()` or read these same files and assert against
the rendered DOM. Don't move them into Rust-only formats.

| File | Scar it encodes | Live bug |
|---|---|---|
| `F1_dense_colliding_names.json` | 14 workloads w/ truncated `comm` collisions (2× `static_transfor`, 2× `parameter_bridg`, 3× `ros2`) | each_key on workloads (operator's live ROS2 graph) |
| `F2_duplicate_label_thermals.json` | 2× `acpitz` thermal zones, same label different reading | D65 thermal each_key (`label-idx` fix) |
| `F3_same_pid_exit_kill.json` | activity entries where the same PID has BOTH an exit AND a kill | D71 activity composite key (`kind-pid-timestamp`) |
| `F4_combined_worst_case.json` | All three above in one snapshot | densest adversarial board |
| `_negative_control_colliding_activity.json` | Deliberate: two activity entries with identical `kind-pid-timestamp` | proves the uniqueness test can fail (not theater) |

## Scope limit

This fixture set + the Rust assertions in
`tests/render_adversarial.rs` guard the **WIRE** (data
well-formedness), NOT the browser render. The session's render bugs
(thermal each-key, workload each-key, web-zero) lived in the Svelte
`{#each}` render layer with a **well-formed wire** — this gate
would NOT have caught them. It catches a DIFFERENT class
(wire-level malformedness) and lays the fixture foundation for a
future browser gate that WOULD catch them.

The composite-key assertions in the Rust tests mirror the EXACT
keys the Svelte components use:

- `WorkloadsPanel.svelte`: `{#each group.rows as w (w.pid)}` — pin
  `pid` unique across all workloads.
- `VitalsPanel.svelte`: `{#each thermalTop as zone, idx (`${zone.label}-${idx}`)}` — pin `(label, idx)` unique.
- `ActivityFeed.svelte`: `{#each activity as ev (`${ev.kind}-${ev.pid}-${ev.timestamp}`)}` — pin `(kind, pid, timestamp)` unique.
- `AlertsPanel.svelte`: `{#each visibleAlerts as alert (`${alert.alert_id}-${alert.pid ?? 'system'}`)}` — pin `(alert_id, pid?? 'system')` unique.

If the frontend rekeys (different fields, different composite),
the Rust assertions in `render_adversarial.rs` must change in
lockstep — otherwise they test the wrong thing.
