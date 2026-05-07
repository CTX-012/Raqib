# BACKLOG

Issues / improvements discovered during in-flight rows that don't fit the
current row's scope. Each entry includes the row that surfaced it.

## v1.1 (post-v1.0)

### telemetry: query Triton `/v2/models` to refine WorkloadCategory for hosted models

**Surfaced by:** L11a workload-category dispatch.

Triton Inference Server and TorchServe are general-purpose model
servers — they can host any model type (LLM, Vision, Embeddings).
Without HTTP introspection of the server's model registry, the
classifier currently maps both to `WorkloadCategory::Unknown` (honest
about the ambiguity but suboptimal UX for users running, say, an
LLM under Triton).

Proposal for v1.1: add a Triton telemetry sampler that scrapes
`/v2/models` (KFServing v2 protocol) at startup for each Triton PID,
inspects the listed models' platforms / config metadata, and refines
the workload category. Same shape for TorchServe via its
`/management` endpoint.

Out of scope for v1.0 — the contract's v1.0 surface has no §2 metric
formatter that would change behavior with a refined category for
these servers.

### test deflake: `telemetry::rapl::tests::delta_math_handles_wraparound`

**Surfaced by:** L11a smoke run.

RAPL delta-math test has timing-sensitive assertions and flakes
under load (passes alone, fails ~1 in 50 in the full workspace
suite). Not a regression — pre-existed across L1-L8.

Proposal: tighten the test bounds or use a synthetic clock to
avoid wall-clock dependency. Low priority; not blocking ship.

## Pending Contract Amendment Requests

### CAR-7: `ux_contract::status::COLD_LOADING`

**Surfaced by:** L11b orchestrator brief referenced
`ux_contract::status::COLD_LOADING` for the Loading-state primary
metric. v0.3.2 doesn't have it. L11b uses a local `"cold-loading"`
literal pending the contract amendment. L25 (footer / status
rewrite) is the natural row to consume the contract const once it
exists.

Suggested addition to `ux_contract::status::*`:
```rust
/// Workload-row primary-metric replacement when the workload is in
/// `WorkloadStatus::Loading`. UX_CONTRACT.md §2.
pub const COLD_LOADING: &str = "cold-loading";
```

### CAR-8: `ux_contract::categories::*` group labels

**Surfaced by:** L11b group-header rendering. The contract §1 region 4
shows "LLM" / "Vision" / "ROS2" / "Embeddings" / "Unknown" as
group-section labels but no const exists. L11b uses a local
`WorkloadCategory::label()` method.

Suggested addition (new module):
```rust
pub mod categories {
    pub const LLM: &str = "LLM";
    pub const VISION: &str = "Vision";
    pub const ROS2: &str = "ROS2";
    pub const EMBEDDINGS: &str = "Embeddings";
    pub const UNKNOWN: &str = "Unknown";
}
```

L25 / contract polish will swap the local labels for the const
references once they exist.
