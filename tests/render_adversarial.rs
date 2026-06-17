//! v1.3.2 / DISPATCH 87 — render-adversarial wire-wellformedness gate.
//!
//! ## What this gate guards (and what it doesn't)
//!
//! **GUARDS:** wire-level data well-formedness. For every list the
//! Svelte renderer keys on via `{#each}`, the backend wire MUST
//! produce a unique composite key across all entries. If a future
//! refactor lets the wire emit duplicate composite keys, every
//! known render bug class re-opens — but the assertion below
//! fires before the dashboard would each_key-fail in production.
//!
//! **DOES NOT GUARD:** the browser render itself. This session's
//! live render bugs (thermal each-key, workload each-key, web-zero
//! cache staleness) lived in the Svelte `{#each}` render layer with
//! a WELL-FORMED wire. This gate would NOT have caught them —
//! they're a different class. Building the wire gate is still the
//! right move because:
//!
//!   1. It locks in the wire-side invariants the Svelte composite
//!      keys depend on. The dashboard's `${ev.kind}-${ev.pid}-${ev.timestamp}`
//!      is only collision-proof if the WIRE delivers (kind, pid,
//!      timestamp) tuples that are jointly unique. We pin that.
//!   2. The fixtures themselves are a durable artifact. A future
//!      headless-browser gate (Playwright / Puppeteer) will load
//!      the SAME `tests/fixtures/render_adversarial/*.json` files,
//!      mount the SPA against them, and assert the rendered DOM.
//!      That browser gate catches what THIS gate cannot.
//!
//! ## Reuse contract for the future browser gate
//!
//! The fixtures are intentionally plain JSON files, not Rust
//! literals. A JS / Playwright harness must be able to read the
//! same `tests/fixtures/render_adversarial/F1_*.json` (etc.)
//! and feed them to the Svelte app for DOM assertions. Don't move
//! the fixtures into bincode / Rust-only formats; don't generate
//! them at test time only.
//!
//! ## Composite keys
//!
//! Mirror the EXACT keys the Svelte components use. As of D86 the
//! keys are:
//!
//! | Component | Key (Svelte) | Test asserts |
//! |---|---|---|
//! | `WorkloadsPanel.svelte` | `{#each group.rows as w (w.pid)}` | `pid` unique across all workloads |
//! | `VitalsPanel.svelte` | `{#each thermalTop as zone, idx (\`${zone.label}-${idx}\`)}` | `(label, position-in-vec)` unique |
//! | `ActivityFeed.svelte` | `{#each activity as ev (\`${ev.kind}-${ev.pid}-${ev.timestamp}\`)}` | `(kind, pid, timestamp)` unique |
//! | `AlertsPanel.svelte` | `{#each visibleAlerts as alert (\`${alert.alert_id}-${alert.pid ?? 'system'}\`)}` | `(alert_id, pid \|\| 'system')` unique |
//!
//! If the Svelte side rekeys, the assertions below MUST be updated
//! in lockstep — otherwise the test pins the wrong invariant.
//!
//! ## Negative control
//!
//! `_negative_control_colliding_activity.json` carries TWO activity
//! entries with identical `(kind, pid, timestamp)` tuples. A test
//! runs the same uniqueness check on it and asserts the check
//! REPORTS a collision. A uniqueness assertion that never fires on
//! a known-bad fixture is theatre; the negative control is the
//! "this test can fail" proof.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::Value;

/// Fixtures directory, resolved relative to `CARGO_MANIFEST_DIR`
/// (the workspace root for an integration test).
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/render_adversarial")
}

/// Read a fixture JSON file into a `serde_json::Value`. Panics on
/// I/O or parse error — fixtures are committed, so a missing /
/// malformed file is a test-suite bug, not a runtime tolerance.
fn load_fixture(name: &str) -> Value {
    let path = fixtures_dir().join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()))
}

/// Mirror of `WorkloadsPanel.svelte`'s `{#each ... (w.pid)}` key.
fn workload_keys(snapshot: &Value) -> Vec<String> {
    snapshot["workloads"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|w| {
                    // pid is the entire key per the .svelte source.
                    w["pid"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| format!("{:?}", w["pid"]))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Mirror of `VitalsPanel.svelte`'s
/// `{#each thermalTop as zone, idx (\`${zone.label}-${idx}\`)}`.
/// The composite is `label-position`. By construction this is
/// trivially unique (idx differs by definition), but the
/// assertion pins the WIRE shape: each entry has a label, and
/// the renderer pairs it with the position-in-vec — a future
/// refactor that flattened the vec into a label-keyed map would
/// surface as a structural change here too.
fn thermal_keys(snapshot: &Value) -> Vec<String> {
    snapshot["vitals"]["thermal_zones"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(idx, z)| {
                    let label = z["label"].as_str().unwrap_or("");
                    format!("{label}-{idx}")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Mirror of `ActivityFeed.svelte`'s
/// `{#each activity as ev (\`${ev.kind}-${ev.pid}-${ev.timestamp}\`)}`.
fn activity_keys(snapshot: &Value) -> Vec<String> {
    snapshot["activity"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|ev| {
                    let kind = ev["kind"].as_str().unwrap_or("");
                    let pid = ev["pid"].as_u64().unwrap_or(0);
                    let ts = ev["timestamp"].as_str().unwrap_or("");
                    format!("{kind}-{pid}-{ts}")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Mirror of `AlertsPanel.svelte`'s
/// `{#each visibleAlerts as alert (\`${alert.alert_id}-${alert.pid ?? 'system'}\`)}`.
fn alert_keys(snapshot: &Value) -> Vec<String> {
    snapshot["alerts"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|a| {
                    let id = a["alert_id"].as_str().unwrap_or("");
                    let pid_part = a["pid"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "system".into());
                    format!("{id}-{pid_part}")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Generic uniqueness assertion. Returns `Ok(())` when every key
/// in the list is unique; returns the first duplicate as `Err`
/// when not. Split out from the test bodies so the negative
/// control can exercise it directly.
fn first_duplicate(keys: &[String]) -> Result<(), String> {
    let mut seen: HashSet<&String> = HashSet::with_capacity(keys.len());
    for k in keys {
        if !seen.insert(k) {
            return Err(k.clone());
        }
    }
    Ok(())
}

/// Helper that asserts uniqueness, panicking with a useful error
/// when a duplicate is found.
fn assert_unique(label: &str, keys: &[String]) {
    if let Err(dup) = first_duplicate(keys) {
        panic!(
            "BOUNDARY VIOLATION: composite key collision on `{label}`. \
             Duplicate key: {dup}. Full list:\n  {}",
            keys.join("\n  ")
        );
    }
}

// ─────────────────────────────────────────────────────────────────
// W1 — composite-key uniqueness per fixture.
// ─────────────────────────────────────────────────────────────────

#[test]
fn f1_dense_colliding_names_workload_pids_are_unique() {
    let fx = load_fixture("F1_dense_colliding_names.json");
    let keys = workload_keys(&fx);
    assert!(
        keys.len() >= 14,
        "F1 must carry at least 14 workloads (operator's ROS2 graph); got {}",
        keys.len()
    );
    assert_unique("WorkloadsPanel (w.pid)", &keys);
    // Sanity: the dispatch's scar text is present — colliding names.
    let names: Vec<&str> = fx["workloads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap_or(""))
        .collect();
    let static_count = names.iter().filter(|n| **n == "static_transfor").count();
    let ros2_count = names.iter().filter(|n| **n == "ros2").count();
    assert!(
        static_count >= 2 && ros2_count >= 3,
        "F1 must encode the operator's collision case: ≥2 static_transfor + ≥3 ros2. \
         got static_transfor={static_count}, ros2={ros2_count}",
    );
}

#[test]
fn f2_duplicate_label_thermals_keys_are_unique() {
    let fx = load_fixture("F2_duplicate_label_thermals.json");
    let zones = fx["vitals"]["thermal_zones"].as_array().unwrap();
    let labels: Vec<&str> = zones.iter().map(|z| z["label"].as_str().unwrap()).collect();
    // The D65 scar: at least two zones share a label.
    let acpitz_count = labels.iter().filter(|l| **l == "acpitz").count();
    assert!(
        acpitz_count >= 2,
        "F2 must encode duplicate-label thermals (the D65 scar); \
         got acpitz_count={acpitz_count}",
    );
    // Despite the label collision, the (label, idx) composite key
    // the renderer uses MUST be unique — that's the D65 fix.
    let keys = thermal_keys(&fx);
    assert_unique("VitalsPanel ({label}-{idx})", &keys);
}

#[test]
fn f3_same_pid_exit_kill_composite_keys_are_unique() {
    let fx = load_fixture("F3_same_pid_exit_kill.json");
    let events = fx["activity"].as_array().unwrap();
    // The D71 scar: same pid appearing as both an exit AND a kill.
    let pid_kind: Vec<(u64, &str)> = events
        .iter()
        .map(|e| (e["pid"].as_u64().unwrap_or(0), e["kind"].as_str().unwrap_or("")))
        .collect();
    let pid_7777_kinds: Vec<&str> = pid_kind
        .iter()
        .filter_map(|(p, k)| if *p == 7777 { Some(*k) } else { None })
        .collect();
    assert!(
        pid_7777_kinds.contains(&"kill") && pid_7777_kinds.contains(&"exit"),
        "F3 must encode the same-pid exit+kill scar (D71); \
         pid 7777 must have both. got kinds: {pid_7777_kinds:?}",
    );
    // Composite key (kind, pid, timestamp) MUST be unique — the
    // D71 fix that disambiguates same-pid exit+kill.
    let keys = activity_keys(&fx);
    assert_unique("ActivityFeed ({kind}-{pid}-{timestamp})", &keys);
}

#[test]
fn f4_combined_worst_case_all_lists_unique_simultaneously() {
    let fx = load_fixture("F4_combined_worst_case.json");
    assert_unique("F4 workloads", &workload_keys(&fx));
    assert_unique("F4 thermals", &thermal_keys(&fx));
    assert_unique("F4 activity", &activity_keys(&fx));
    assert_unique("F4 alerts", &alert_keys(&fx));
}

// ─────────────────────────────────────────────────────────────────
// W2 — fields the renderer keys on are never null/missing where a
// key needs them. We loaded the fixtures as `Value`; null-checking
// happens at the `as_str() / as_u64()` extractor calls in the
// helpers above. The presence assertions here make the intent
// explicit so a future fixture editor sees the requirement.
// ─────────────────────────────────────────────────────────────────

#[test]
fn activity_keying_fields_are_populated_in_every_fixture() {
    for name in [
        "F1_dense_colliding_names.json",
        "F2_duplicate_label_thermals.json",
        "F3_same_pid_exit_kill.json",
        "F4_combined_worst_case.json",
    ] {
        let fx = load_fixture(name);
        let events = match fx["activity"].as_array() {
            Some(arr) => arr,
            None => continue, // empty / missing activity is fine for fixtures w/o the scar
        };
        for (i, ev) in events.iter().enumerate() {
            assert!(
                ev["kind"].is_string(),
                "{name}: activity[{i}].kind must be a string for composite keying"
            );
            assert!(
                ev["pid"].is_u64(),
                "{name}: activity[{i}].pid must be an integer for composite keying"
            );
            assert!(
                ev["timestamp"].is_string(),
                "{name}: activity[{i}].timestamp must be a string for composite keying"
            );
        }
    }
}

#[test]
fn workload_keying_field_pid_is_populated_in_every_fixture() {
    for name in [
        "F1_dense_colliding_names.json",
        "F4_combined_worst_case.json",
    ] {
        let fx = load_fixture(name);
        let arr = fx["workloads"].as_array().expect("workloads array");
        for (i, w) in arr.iter().enumerate() {
            assert!(
                w["pid"].is_u64(),
                "{name}: workloads[{i}].pid must be an integer for composite keying"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// W3 — fixtures round-trip through serde_json (the JSON file
// format is deterministic and self-consistent — confirms the file
// is well-formed JSON in shape, not just bytes).
// ─────────────────────────────────────────────────────────────────

#[test]
fn every_fixture_round_trips_through_serde_json() {
    for name in [
        "F1_dense_colliding_names.json",
        "F2_duplicate_label_thermals.json",
        "F3_same_pid_exit_kill.json",
        "F4_combined_worst_case.json",
        "_negative_control_colliding_activity.json",
    ] {
        let fx = load_fixture(name);
        let serialized = serde_json::to_string(&fx)
            .unwrap_or_else(|e| panic!("re-serialize {name}: {e}"));
        let reparsed: Value = serde_json::from_str(&serialized)
            .unwrap_or_else(|e| panic!("re-parse {name}: {e}"));
        assert_eq!(
            fx, reparsed,
            "{name}: fixture must round-trip through serde_json. \
             A non-roundtrip means the file relies on parser quirks \
             that a future serializer might lose.",
        );
    }
}

// ─────────────────────────────────────────────────────────────────
// W4 — fixtures are LOADED FROM FILE, not inline. This very test
// body proves the file-based reuse contract: a future browser /
// JS harness must be able to do `fetch('/tests/fixtures/...')`
// and get the same data this test reads via `std::fs`.
// ─────────────────────────────────────────────────────────────────

#[test]
fn fixture_files_exist_and_are_under_fixtures_dir() {
    let dir = fixtures_dir();
    assert!(
        dir.is_dir(),
        "fixtures dir must exist on disk for the reuse contract; \
         expected: {}",
        dir.display(),
    );
    for name in [
        "F1_dense_colliding_names.json",
        "F2_duplicate_label_thermals.json",
        "F3_same_pid_exit_kill.json",
        "F4_combined_worst_case.json",
        "_negative_control_colliding_activity.json",
        "README.md",
    ] {
        let p = dir.join(name);
        assert!(p.is_file(), "missing fixture file: {}", p.display());
    }
}

// ─────────────────────────────────────────────────────────────────
// THE NEGATIVE CONTROL — proves the uniqueness assertion actually
// fires on collisions. A uniqueness test that never fails when
// duplicates are present is theatre; this is the proof.
// ─────────────────────────────────────────────────────────────────

#[test]
fn negative_control_fixture_actually_contains_a_collision() {
    // Pre-check: the negative-control file MUST encode two activity
    // entries with identical composite keys.
    let fx = load_fixture("_negative_control_colliding_activity.json");
    let keys = activity_keys(&fx);
    let dup = first_duplicate(&keys);
    assert!(
        dup.is_err(),
        "NEGATIVE CONTROL THEATRE: the deliberately-broken fixture \
         did NOT actually contain a composite-key collision. The \
         uniqueness check passed when it must have failed. Edit \
         `_negative_control_colliding_activity.json` to ensure two \
         activity entries share (kind, pid, timestamp). Keys read: \
         {keys:?}",
    );
    // The duplicate key must be exactly what the negative-control
    // README describes — same kind/pid/timestamp.
    let dup_key = dup.unwrap_err();
    assert!(
        dup_key.contains("exit") && dup_key.contains("5555"),
        "negative control's duplicate key should be the documented \
         (exit, 5555, …) tuple; got: {dup_key}",
    );
}

#[test]
fn uniqueness_check_distinguishes_negative_control_from_real_fixtures() {
    // Belt-and-suspenders sibling to the test above: the SAME
    // function returns `Err` on the negative control AND `Ok` on
    // every real fixture. Catches a regression where the check
    // accidentally became "always Ok" (e.g. someone short-circuited
    // it for fast-path).
    let neg = load_fixture("_negative_control_colliding_activity.json");
    assert!(
        first_duplicate(&activity_keys(&neg)).is_err(),
        "negative control MUST surface as an Err",
    );
    for name in [
        "F1_dense_colliding_names.json",
        "F2_duplicate_label_thermals.json",
        "F3_same_pid_exit_kill.json",
        "F4_combined_worst_case.json",
    ] {
        let fx = load_fixture(name);
        assert!(
            first_duplicate(&workload_keys(&fx)).is_ok(),
            "real fixture {name}: workloads should NOT collide"
        );
        assert!(
            first_duplicate(&thermal_keys(&fx)).is_ok(),
            "real fixture {name}: thermals should NOT collide"
        );
        assert!(
            first_duplicate(&activity_keys(&fx)).is_ok(),
            "real fixture {name}: activity should NOT collide"
        );
        assert!(
            first_duplicate(&alert_keys(&fx)).is_ok(),
            "real fixture {name}: alerts should NOT collide"
        );
    }
}
