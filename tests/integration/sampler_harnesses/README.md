# Production-Shape Integration Harness (P5 meta-deliverable)

## Intent

Every Phase-2 sampler bug that reached a shipped tag shared one root
cause: **a unit-test fixture built a `ProcessSnapshot` (or an `/api/ps`
mock) by hand, symmetrically, and so never reproduced the asymmetry
the real classifier + real runtime filter introduce.** The sampler
logic was correct against the fixture and wrong against reality.

| Bug | Shipped in | What the symmetric fixture hid |
|---|---|---|
| B1 digest-vs-name | v1.1.0 | Fixture set `my_model` and `loaded` to the *same* string. Reality: `model_extract` gives the runner's **blob digest** (`sha256-eb2c71…`); `/api/ps` gives the **model name** (`smollm:135m`). Never equal → activity locked to NotDetected. |
| B2 bash-child invisibility | v1.1.1 | Fixture passed the bash child in the sampler's input slice. Reality: `runtime.rs` filters to `AICategory != NotAi` before `tick`, and `bash` is `NotAi` → absent from `ai_procs`. Sampler read `ai_procs`, saw no children → activity locked to Idle. |
| B3 `--timeout` Humble-reject | v1.1.5 | Harness tested `ros2 topic hz`; production code shelled `ros2 topic echo --once --timeout <T>`. Harness silently passed because it never invoked the actual subprocess shape. Humble's `ros-humble-ros2cli 0.18.18` rejected `--timeout` (added in Iron/Jazzy/Rolling) — every probe failed `unrecognized arguments`, every topic locked to Idle. Caught by Tester-B; v1.1.6 ITEM 1 dropped the flag, ITEM 2 (this rewrite) makes the harness mirror the real shape. |

A **production-shape harness** closes this class: instead of
hand-building the sampler's input, it spawns a *real* workload, runs
it through edge_monitor's *real* classifier + *real* runtime filter,
captures the exact slice the dispatcher would hand the sampler, and
then asserts the sampler's output against that real slice.

If such a harness had existed, both bugs would have surfaced the
first time the sampler ran against a real workload — which is exactly
what P5's empirical vectors do, but ad-hoc. This harness is the
reusable, documented form.

## What this directory is (and isn't)

- **IS**: documentation + reference bash scripts showing the
  capture-real-slice → run-sampler workflow per sampler. Tester
  tooling, run by hand or in an empirical sweep.
- **IS NOT**: production Rust code. It does not live in `src/`. It
  does not modify the samplers. It is observation scaffolding.

## The general workflow

```
1. SPAWN a real workload          (ollama runner / claude session / ros2 node)
2. CLASSIFY via the real binary    (edge_monitor --no-ui, read /api/snapshot)
3. CAPTURE the real slice          (what the dispatcher hands the sampler:
                                     - ai_procs   = workloads where category != NotAi
                                     - all_procs  = the full /proc list)
4. COMPARE                         (does the field the sampler keys on actually
                                     exist / match in the captured real slice?)
5. ASSERT                          (sampler output state == expected for the
                                     observed workload behavior)
```

The crux is step 4: the harness checks the **identity assumption** the
sampler makes (B1: digest == name? B2: child in ai_procs? B3: topic
list non-empty within timeout?) against real data, not a fixture.

## Per-sampler harness scripts

- `b1_ollama_harness.sh` — spawns an ollama runner, captures the
  runner's `model_name` as edge_monitor classifies it (blob digest)
  AND ollama's `/api/ps` name, and **asserts they differ** — the
  exact mismatch that was the v1.1.0 bug. A correct B1 must reconcile
  the two.
- `b2_agent_claude_harness.sh` — confirms a `bash` tool-child of a
  claude PID is `NotAi` and therefore absent from the AI-filtered
  slice, so a child-detecting sampler MUST read `all_procs` not
  `ai_procs` — the v1.1.1 bug.
- `b3_ros2_harness.sh` — v1.1.6 rewrite. Spawns the EXACT B3
  subprocess shape (`ros2 topic echo --once <topic>`, no
  `--timeout` — see v1.1.6 ITEM 1) against a live publisher,
  measures first-message latency vs `ROS2_SHELLOUT_TIMEOUT`, AND
  guards against Humble re-supporting `--timeout` (asserts
  `--once --timeout 1` STILL fails on the host).

Each script is self-contained, read-only against `src/`, runs
locally (see "Host dependencies" + "How to run locally" below), and
exits `0` on PASS / `1` on FAIL (a named missing dependency is a
FAIL).

## What the harness would have caught (concrete)

### B1 (v1.1.0)
```
$ ./b1_ollama_harness.sh
runner model_name (as classified):  sha256-eb2c714d40d4…   ← from --model cmdline path
/api/ps model name:                 smollm:135m            ← from ollama API
ASSERTION: classified-name == api-name?  FALSE
  → B1's `loaded.iter().any(|m| m == my_model)` can never match.
  → FAIL surfaced before ship.
```

### B2 (v1.1.1)
```
$ ./b2_agent_claude_harness.sh
claude PID 6051 bash tool-child: PID 88123  comm=bash  category=NotAi
present in ai_procs slice (category!=NotAi filter)?  FALSE
present in all_procs slice (unfiltered)?             TRUE
  → a sampler reading ai_procs sees zero children → activity stuck Idle.
  → FAIL surfaced before ship.
```

### B3 (v1.1.5 ship bug — `--timeout` Humble-reject)
```
$ ./b3_ros2_harness.sh
GUARD OK: Humble ros2cli rejects `--timeout` on `topic echo` (rc=2).
rate=1Hz  first-message=1.04s  inner-timeout=3s  margin=1.96s
PASS: echo-once observes a message in 1.04s with 1.96s margin under the 3s inner timeout.
```
Pre-v1.1.6 the harness invoked `ros2 topic hz` and PASSED green
against the broken v1.1.5 `ros2 topic echo --once --timeout <T>`
invocation — the harness-drift gap the harness exists to prevent.
v1.1.6 ITEM 2 makes the harness mirror the EXACT B3 invocation +
adds a Humble-compat guard step (rc != 0 + stderr mentions
`--timeout`) so a future re-introduction of the flag fails the
harness on Humble. See `b3_echo_once_no_timeout_flag_detects_active_topic`
in `src/telemetry/samplers/ros2_shellout.rs` for the Rust-side
regression pin.

### B3 prior P5 finding — refinement, not a ship bug
v1.1.3 raised `ROS2_SHELLOUT_TIMEOUT` 5s → 8s on a 1 Hz-marginal /
sub-Hz-timeout finding from the `ros2 topic hz` mechanism. v1.1.5
ITEM D replaced the mechanism with `ros2 topic echo --once` +
30 s staleness window — first-message latency at 1 Hz is now ~1 s
(no minimum-3-message wait), so `INNER` came back down to 3 s. The
"sub-Hz topics are structurally unobservable" property the hz
mechanism had is gone — echo-arrival is observable at any non-zero
rate; the staleness window decides Active vs Idle.

## Host dependencies

Each harness spawns / inspects **real** production workloads — that
is the whole point (it reproduces the asymmetry hand-built fixtures
can't). It therefore needs those workloads present:

| Harness | Needs |
|---|---|
| `b1_ollama_harness.sh` | a running **ollama daemon** (`localhost:11434`) with a pullable model, **and** a running `edge_monitor` on `$PORT` |
| `b2_agent_claude_harness.sh` | a running `edge_monitor` on `$PORT` **and** at least one **claude** agent the classifier recognises (`--output-format stream-json`, under `.vscode-server/` or `.vscode/` extensions) |
| `b3_ros2_harness.sh` | **ROS2** installed + sourceable (`/opt/ros/<distro>/setup.bash`), `ros2` on PATH, `bc` |

All exit `0` on PASS, `1` on FAIL (including "required dependency
absent"). The FAIL message names the missing dependency.

## How to run locally

Start an `edge_monitor` instance bound to a known port, then run the
relevant harness:

```bash
# In one terminal — the monitor the harnesses query:
edge_monitor --bind 127.0.0.1:7273

# In another terminal:
# B1 — needs ollama running locally
PORT=7273 MODEL=smollm:135m ./b1_ollama_harness.sh

# B2 — needs a claude agent running (e.g. this very session)
PORT=7273 ./b2_agent_claude_harness.sh

# B3 — needs ROS2 sourced; defaults to the v1.1.6 3s inner timeout
RATE=1 INNER=3 ROS_SETUP=/opt/ros/humble/setup.bash ./b3_ros2_harness.sh
```

Each prints its captured real-world values and a final `PASS:` or
`FAIL:` line.

## CI status

**CI integration is deferred.** Stock CI runners (GitHub Actions
`ubuntu-22.04`, etc.) lack ollama, claude, and ROS2 and cannot
exercise these harnesses — an unconditional CI job would red-flag
every PR. When self-hosted infrastructure with the production deps
is provisioned, the smoke-stage wiring lands as a separate
refinement dispatch.

The **structural fix is the harness being available** for local
developers to run before a PR — not the CI automation, which is a
multiplier on the fix, not the fix itself. A developer running these
scripts against a real workload catches the symmetric-fixture defect
class even without CI.

## Why this matters (the structural fix)

Every Phase-2 sampler bug that reached a shipped tag (v1.1.0 B1,
v1.1.1 B2) shared one root cause: a unit-test fixture built the
sampler's input by hand, **symmetrically**, and so never reproduced
the asymmetry the real classifier + real runtime filter introduce.
The sampler logic was correct against the fixture and wrong against
reality. Unit tests cannot catch this class by construction — the
fixture IS the blind spot. A production-shape harness that captures
the *real* slice closes it.

## How to add a harness for a new sampler

1. Copy the shape of `b1_ollama_harness.sh`:
   - `#!/usr/bin/env bash` + `set -euo pipefail`.
   - A `fail()` helper; probe every required dep up front and
     `fail` with a specific message if absent.
   - Spawn / locate the real workload.
   - Capture the exact field(s) the sampler keys on, from the
     **real** surface (the live `/api/snapshot`, `/proc`, the
     runtime API) — never a hand-built value.
   - Assert the sampler's identity assumption against the captured
     real data. Exit `0` on PASS, `1` on FAIL.
2. Document the new harness's dep + PASS/FAIL criterion in the
   tables above.
3. Keep it read-only against `src/` — harnesses observe, they do
   not modify the samplers.
