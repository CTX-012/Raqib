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

## Resolved Contract Amendment Requests

### CAR-7 — RESOLVED in v0.3.4

`ux_contract::status::COLD_LOADING = "cold-loading"`. Adopted by
L11c in `src/ui/panels/workloads.rs::primary_metric`; the local
`COLD_LOADING` literal was retired.

### CAR-8 — RESOLVED in v0.3.4

`ux_contract::workload_category::GROUP_HEADER_*` (LLM / VISION /
ROS2 / EMBEDDINGS / UNKNOWN). Contract refined the proposal to
ship the full rule-line strings (`"── LLM ──"` etc.), not just
labels — saves a `format!` at every render. Adopted by L11c via
`panels::workloads::category_header`. The local
`WorkloadCategory::label()` helper was retired; the
`WorkloadCategory` enum itself stays in this tree per the
orchestrator's "KEEP CONST-ONLY for v1.0" decision.

## v1.1 (post-v1.0)

### enum migration: move `WorkloadCategory` into `ux_contract`

**Surfaced by:** L11c. Contract refined CAR-8 to const-only group
headers rather than making the enum contract-owned. For v1.0 the
enum lives in `crate::model::WorkloadCategory` and the panel maps
it locally to the contract const. v1.1+ may want to move the enum
itself to `ux_contract::WorkloadCategory` so both the Linux and
Windows binaries share the type. Mid-v1.0 refactor is out of
scope.

### telemetry: query Triton `/v2/models` to refine WorkloadCategory for hosted models
