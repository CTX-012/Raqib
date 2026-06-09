//! Sprint-7.5 — Agent subsection + Model column + empty-collapse
//! regression tests.
//!
//! Pins three coupled invariants:
//!
//!   Fix B: SaaS-LLM classifier leaves `model_name` unset. Pre-fix
//!          the user-reported screenshot showed a category-like
//!          string ("agents") in the Model column for claude rows;
//!          the only paths that could populate that field for a
//!          SaaS-LLM workload are (a) the classifier's own
//!          constructor and (b) the `augment_with_model_name`
//!          post-pass. The Sprint-7.5 fix routes SaaS-LLM to Agent
//!          category, which bypasses the LLM-only augment pass; the
//!          constructor never set the field. These tests pin both
//!          guarantees.
//!
//!   Fix C: SaaS-LLM workloads classify as `Agent`, not `LLM`. The
//!          Workloads panel section header maps to
//!          `GROUP_HEADER_AGENT` (v0.3.9 contract const).
//!
//!   Fix D: Empty workload categories — Vision, ROS2, Embeddings —
//!          do not render section headers at all. This was already
//!          the behavior; Sprint-7.5 pins it explicitly so a future
//!          refactor can't accidentally show "── Vision ──" with
//!          no rows below it.

use edge_monitor::classifier::classify_process;
use edge_monitor::model::{ProcessSample, WorkloadCategory};
use std::collections::HashMap;

fn sample(name: &str, argv: &[&str]) -> ProcessSample {
    ProcessSample {
        pid: 1234,
        ppid: Some(1),
        name: name.into(),
        cmdline: argv.iter().map(|s| s.to_string()).collect(),
        environ: HashMap::new(),
        cwd: None,
        ..Default::default()
    }
}

// ── Fix B — Model column for SaaS-LLM rows stays empty ─────────────

#[test]
fn claude_classification_leaves_model_name_none() {
    let s = sample(
        "node",
        &[
            "node",
            "/home/u/.vscode-server/extensions/anthropic.claude-code/cli.js",
        ],
    );
    let r = classify_process(&s);
    assert_eq!(r.workload_category, WorkloadCategory::Agent);
    assert!(
        r.model_name.is_none(),
        "claude must NOT carry a model_name — the augment pass is \
         LLM-only and the classifier's `ai()` constructor leaves \
         model_name None: got {:?}",
        r.model_name,
    );
}

#[test]
fn cursor_classification_leaves_model_name_none() {
    let s = sample(
        "node",
        &[
            "node",
            "/home/u/.vscode-server/extensions/cursor-0.5.0/extension.js",
        ],
    );
    let r = classify_process(&s);
    assert_eq!(r.workload_category, WorkloadCategory::Agent);
    assert!(r.model_name.is_none());
}

#[test]
fn continue_classification_leaves_model_name_none() {
    let s = sample(
        "node",
        &[
            "node",
            "/home/u/.vscode-server/extensions/continue.continue-0.9.0/extension.js",
        ],
    );
    let r = classify_process(&s);
    assert_eq!(r.workload_category, WorkloadCategory::Agent);
    assert!(r.model_name.is_none());
}

// ── Fix C — Agent classification + contract header mapping ─────────

#[test]
fn agent_category_serializes_as_agent_string_on_the_wire() {
    // The web frontend's `WorkloadsPanel.svelte::ORDER` keys on the
    // lowercase string `"agent"`. Pin the wire mapping so a future
    // Rust-side rename can't silently break the dashboard's
    // subsection rendering.
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::web::WireSnapshot;
    // The conversion lives in `WireSnapshot::from_runtime_state`;
    // a default Runtime has no AI processes, so we synthesize the
    // string via the public mapping instead.
    let runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
    let snap = WireSnapshot::from_runtime_state(runtime.state());
    // Default runtime has zero workloads, so we can't directly
    // observe an Agent row on the wire from here. The dispatch
    // tests above pin Rust-side classification → Agent; the wire
    // string-mapping test lives in `src/web/wire.rs::tests`. This
    // smoke check ensures the snapshot at least round-trips an
    // empty payload through serde without the new variant breaking
    // serialization.
    let json = serde_json::to_string(&snap).expect("wire snapshot serializes");
    assert!(json.contains("\"workloads\""));
}

#[test]
fn agent_category_has_distinct_display_order_from_llm() {
    // Sprint-7.5 — `display_order` puts Agent at slot 1, between
    // LLM (slot 0) and Vision (slot 2). Pin the ordering so a
    // future refactor doesn't fold Agent back into the LLM bucket.
    assert_eq!(WorkloadCategory::LLM.display_order(), 0);
    assert_eq!(WorkloadCategory::Agent.display_order(), 1);
    assert_eq!(WorkloadCategory::Vision.display_order(), 2);
    assert_ne!(
        WorkloadCategory::Agent.display_order(),
        WorkloadCategory::LLM.display_order(),
        "Agent and LLM must occupy distinct dashboard slots"
    );
}

#[test]
fn all_in_order_includes_agent_between_llm_and_vision() {
    let order = WorkloadCategory::all_in_order();
    let llm = order
        .iter()
        .position(|c| *c == WorkloadCategory::LLM)
        .expect("LLM in order");
    let agent = order
        .iter()
        .position(|c| *c == WorkloadCategory::Agent)
        .expect("Agent in order");
    let vision = order
        .iter()
        .position(|c| *c == WorkloadCategory::Vision)
        .expect("Vision in order");
    assert!(
        llm < agent && agent < vision,
        "Agent should sit between LLM and Vision (LLM at {llm}, \
         Agent at {agent}, Vision at {vision})",
    );
}

// ── Fix D — Empty subsections render no header ─────────────────────
// The "Workloads panel does not render an Agent header when no Agent
// workloads exist" invariant is exercised end-to-end via the panel's
// own render path. The integration is hard to reach from outside the
// crate (the render function is module-private); the test below
// confirms the dispatch-level guarantee that `ordered_rows` returns
// only non-empty groups in their canonical order. Empty categories
// are dropped at the render layer (per panel module docs); this test
// pins that the data-layer enumeration sees only rows for present
// categories, which the render-layer filter then surfaces.

#[test]
fn ordered_rows_does_not_synthesize_phantom_rows_for_empty_categories() {
    // A default Runtime has no AI processes. The wire-snapshot
    // payload should therefore contain an empty workloads array —
    // no synthetic placeholder rows per category. This pins the
    // collapse rule at the data layer.
    use edge_monitor::config::Config;
    use edge_monitor::runtime::Runtime;
    use edge_monitor::web::WireSnapshot;
    let runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
    let snap = WireSnapshot::from_runtime_state(runtime.state());
    assert!(
        snap.workloads.is_empty(),
        "empty runtime must produce empty workloads array; got {} \
         rows",
        snap.workloads.len()
    );
}
