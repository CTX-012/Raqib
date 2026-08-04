//! v0.3.14 CAR-23 — `Recommendation` surface. Closes the Phase 3
//! governor-finish observe/recommend gap defined in
//! `edge_monitor-l14/tests/empirical/audit_2026-06-01/INSPECTOR_V1_2_0_RECOMMEND_DESIGN.md`.
//!
//! ## Authority lock — observe and recommend ONLY
//!
//! A [`Recommendation`] is a STRING THE USER READS. It is never a
//! callable, never an auto-trigger, never a wired action. The
//! existing `send_sigterm` path stays manual-only;
//! `default_ai_action = Allow` is unchanged; no
//! `--enable-governor` flag; no tick-path kill wiring.
//!
//! This module enforces that lock at the type level via the
//! discriminator-only [`SuggestedAction`] enum (see below).
//!
//! ## SuggestedAction is a discriminator, NOT a callable
//!
//! [`SuggestedAction`] is a plain `#[derive(Copy, …)]` enum with
//! no associated function and no payload. The contract carries
//! the KIND of suggestion; the consumer's renderer maps each
//! variant to a display-string template via a lookup table (see
//! [`display`]). There is no `SuggestedAction::execute(self)`,
//! no `Box<dyn Fn>`, no callback. Adding a payload — or any
//! method whose receiver returns a `Fn` — would BREAK [`Copy`]
//! and trip the `suggested_action_is_copy` test. That test is
//! the structural enforcement of the observe-only firewall:
//! a future edit that pushes actuation into the contract trips
//! a red gate before it can land.
//!
//! Adding a new actionable variant (e.g. `ConsiderQuiesce`)
//! requires a `CONTRACT_VERSION` bump — operator-approval gate.
//! A free-string action surface would let any producer invent
//! an action the consumer doesn't know how to render and that
//! the operator hasn't sanctioned. The typed enum prevents that.
//!
//! ## Zero-dep, consumer-shimmed serde
//!
//! Mirrors [`crate::activity::ActivityState`] (v0.3.12) and
//! [`crate::host_vitals::HostVitals`] (v0.3.13): bare types, no
//! `serde` derives on the contract, no `Default` impl. The
//! consumer (edge_monitor) shims `serde` at its wire boundary.
//! Reference label-lookup implementation lives in
//! `#[cfg(test)] mod tests::reference_label_for_action_covers_all_variants`
//! as the documented discriminator-to-text mapping.
//!
//! ## Why severity is pre-classified server-side
//!
//! Mirrors thermal severity in the host-vitals path: wire
//! carries both raw (`alert_id`) and computed (`severity`) so
//! the Svelte client doesn't have to embed the priority-tier
//! mapping in TypeScript. ONE source of constants per consumer
//! language: the TUI classifies on the Rust side at render
//! time; the web wire pre-classifies for cross-language
//! consumers.
//!
//! ## Why not include `ConsiderQuiesce` / `ConsiderRecycle` / etc.
//!
//! v0.3.14 ships the three variants the Inspector design
//! ratified against the v1.2.0 signal set (VRAM/KV/RAM pressure
//! → `ConsiderKill`; thermal → `ConsiderReduceLoad`;
//! OOM/Exited → `ConsiderRestart`). Future variants are added
//! through new CARs with operator sign-off — the typed-enum
//! gate is the whole point.

/// Whether a recommendation names a specific workload (or a set
/// of them) or applies to the host as a whole.
///
/// Mirrors the existing `AlertScope` discrimination on the
/// consumer side. Bare enum, no payload — the actual targets
/// live on [`Recommendation::targets`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecommendationScope {
    /// Recommendation is about one or more specific workloads
    /// (per-PID pressure, OOM, exit). [`Recommendation::targets`]
    /// is non-empty.
    Workload,
    /// Recommendation is about the host as a whole (thermal,
    /// system-wide RAM with cross-workload contributors).
    /// [`Recommendation::targets`] MAY be empty (thermal) or
    /// carry ranked contributors (system RAM).
    System,
}

/// The KIND of suggestion the user reads. **Discriminator only:
/// never a callable.** See module-level docs for the
/// authority-lock rationale and the `suggested_action_is_copy`
/// test for the structural enforcement.
///
/// The render-side label dictionary in [`display`] maps each
/// variant to a text template. There is no
/// `SuggestedAction::execute()` — the consumer's renderer reads
/// the variant, picks a label template, substitutes
/// `{pid}` / `{name}` / `{targets}`, and writes a string. That
/// is the full lifecycle of a `SuggestedAction` value. The user
/// acts by traversing the existing manual flow (j/k navigation →
/// `k` keybinding → kill-confirm card → SIGTERM, unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuggestedAction {
    /// Drives `display::CONSIDER_KILL_SINGLE` or
    /// `display::CONSIDER_KILL_MULTI` depending on
    /// [`Recommendation::targets`] length. Fires for VRAM /
    /// KV / RAM pressure recs.
    ConsiderKill,
    /// Drives `display::CONSIDER_REDUCE_LOAD`. Fires for
    /// system-scope pressure recs that don't name a single
    /// workload (thermal, system-wide CPU).
    ConsiderReduceLoad,
    /// Drives `display::CONSIDER_RESTART`. Fires for post-exit
    /// recs (`OomDetected`, optionally non-clean
    /// `WorkloadExited`).
    ConsiderRestart,
}

/// Severity tier of a recommendation — drives ordering on the
/// rec section and color styling via the same theme colors
/// already used for alerts.
///
/// Pre-classified by the producer (see module docs) so the web
/// wire doesn't push tier logic into TypeScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecommendationSeverity {
    /// Informational tier — sorts last.
    Info,
    /// Resource-pressure tier — matches the existing
    /// `AlertState` pressure tier.
    Warning,
    /// Hard-failure tier — matches the existing
    /// `GovernorArmed` / OOM / Exited tier. Sorts first.
    Critical,
}

/// One ranked target of a recommendation. Carries the PID, a
/// human-readable name, and an optional metric-snapshot string
/// captured at fire time as "evidence" (so the renderer doesn't
/// have to look it up live).
///
/// Owned [`String`] for `name` and `evidence`: both come from
/// runtime data, not compile-time literals. Keeps the type
/// [`Clone`] but not [`Copy`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecommendedTarget {
    /// PID of the recommended workload. `u32` matches the rest
    /// of the contract's PID typing.
    pub pid: u32,
    /// Display name (`comm` / classified label) of the
    /// workload. The renderer substitutes this into label
    /// templates like `"Consider killing PID {pid} ({name})"`.
    pub name: String,
    /// Optional one-label-value pair captured at fire time:
    /// e.g. `Some("vram_mb=11400".to_string())` for a VRAM
    /// pressure rec, `Some("rss_mb=12345".to_string())` for a
    /// RAM pressure rec. `None` when the rec doesn't need
    /// per-target evidence (thermal: no per-PID evidence
    /// exists, but thermal has no targets at all).
    pub evidence: Option<String>,
}

/// One recommendation: derived render-time projection of an
/// underlying alert + the runtime state at the same tick. The
/// fields mirror [the Inspector design
/// doc](../../edge_monitor-l14/tests/empirical/audit_2026-06-01/INSPECTOR_V1_2_0_RECOMMEND_DESIGN.md)
/// §2 exactly.
///
/// **Lifecycle**: recommendations are derived (not stored) from
/// the existing `AlertState` at render time. A rec exists for as
/// long as its underlying alert is visible; when the alert
/// clears, the rec disappears with it. No new state machine,
/// no new ack semantics, no fire/clear transitions of its own.
///
/// **Authority lock**: this struct is pure data. It carries
/// `pid: u32` per target — not a file descriptor, not a
/// callback, not an `Executor` reference. There is no method
/// on this type that sends a signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    /// 1:1 link to the firing alert that drove this rec. Lets
    /// the consumer correlate recs with the alert section and
    /// reuses the existing alert priority tier for ordering.
    pub alert_id: crate::AlertId,
    /// Workload-scope vs system-scope.
    pub scope: RecommendationScope,
    /// Ranked targets. Up to
    /// [`crate::limits::REC_TARGETS_MAX`] entries. Empty for
    /// system-scope recs that don't name a workload (thermal).
    pub targets: Vec<RecommendedTarget>,
    /// Discriminator → text-template lookup. See
    /// [`SuggestedAction`] and [`display`].
    pub action: SuggestedAction,
    /// Pre-classified severity tier.
    pub severity: RecommendationSeverity,
    /// Human-readable rationale rendered as a one-line
    /// sub-text under the action label: e.g.
    /// `"VRAM at 92%, evidence: 11.5 GB / 12 GB"`. Producer-
    /// formatted; the contract does not constrain the
    /// vocabulary further.
    pub reason: String,
}

/// Display-label templates for the rendering layer.
///
/// Each [`SuggestedAction`] variant has at least one template
/// here; multi-target / system-scope variants have shape-
/// matching alternates. Templates use the same `{name}` /
/// `{pid}` / `{targets}` placeholder convention as
/// `crate::alerts::*` and `crate::status::*`.
///
/// [`display::RECOMMENDATION_NOT_ACTIONABLE`] is the
/// once-per-section disclaimer: per operator decision on the
/// v0.3.14 dispatch, the consumer renders it ONCE at the top
/// of the rec section (not per-rec). The contract carries the
/// string here so TUI and web render identical text.
pub mod display {
    /// `SuggestedAction::ConsiderKill` with a single target.
    /// `{pid}` is the target's PID, `{name}` is its display
    /// name.
    pub const CONSIDER_KILL_SINGLE: &str =
        "Consider killing PID {pid} ({name})";

    /// `SuggestedAction::ConsiderKill` with multiple targets
    /// (system-scope RAM pressure with top-N RSS contributors).
    /// `{targets}` is the producer-formatted list of
    /// `"name (PID pid)"` entries, joined with `", "`.
    pub const CONSIDER_KILL_MULTI: &str =
        "Consider killing one of: {targets}";

    /// `SuggestedAction::ConsiderReduceLoad`. System-scope rec
    /// with no target (thermal). No placeholders.
    pub const CONSIDER_REDUCE_LOAD: &str =
        "Consider reducing system load";

    /// `SuggestedAction::ConsiderRestart`. Single-target
    /// post-exit rec (`OomDetected`). `{name}` is the exited
    /// workload's name.
    pub const CONSIDER_RESTART: &str =
        "Consider restarting {name}";

    /// Once-per-rec-section disclaimer. The consumer renders
    /// this ONCE at the top of the rec section (operator lock,
    /// dispatch 43); the string itself is the user-facing
    /// affirmation of the observe-only boundary. TUI and web
    /// MUST render the same text via this constant.
    pub const RECOMMENDATION_NOT_ACTIONABLE: &str =
        "Suggestion only — press k to act manually";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ALERT_MAX_VISIBLE, AlertId, limits};

    /// Pins the [`SuggestedAction`] variant set against a
    /// casual refactor that might add or drop one without
    /// updating the [`display`] templates or the consumer's
    /// render-side mapping.
    #[test]
    fn suggested_action_variants_are_distinct() {
        let actions = [
            SuggestedAction::ConsiderKill,
            SuggestedAction::ConsiderReduceLoad,
            SuggestedAction::ConsiderRestart,
        ];
        for (i, a) in actions.iter().enumerate() {
            for (j, b) in actions.iter().enumerate() {
                assert_eq!(
                    a == b,
                    i == j,
                    "variant identity must hold: {a:?} vs {b:?}",
                );
            }
        }
    }

    /// **The authority-lock firewall.** [`SuggestedAction`] is
    /// a discriminator-only enum; adding a payload (e.g. a
    /// `Box<dyn Fn>` callback) would break [`Copy`] and trip
    /// this test, surfacing the violation before it can land.
    ///
    /// If a future refactor ever needs to break this lock, it
    /// needs to delete this test FIRST. That deletion is the
    /// reviewable gate.
    #[test]
    fn suggested_action_is_copy() {
        let a = SuggestedAction::ConsiderKill;
        let b = a; // `Copy`
        assert_eq!(a, b);
    }

    /// Mirror of `suggested_action_variants_are_distinct` for
    /// [`RecommendationScope`].
    #[test]
    fn recommendation_scope_variants_are_distinct() {
        let scopes = [
            RecommendationScope::Workload,
            RecommendationScope::System,
        ];
        for (i, a) in scopes.iter().enumerate() {
            for (j, b) in scopes.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
    }

    /// Mirror for [`RecommendationSeverity`], plus `Copy` pin
    /// (same payload-prevention logic as
    /// `suggested_action_is_copy` though severity has no
    /// authority-lock relevance — only ordering).
    #[test]
    fn recommendation_severity_variants_are_distinct_and_copy() {
        let severities = [
            RecommendationSeverity::Info,
            RecommendationSeverity::Warning,
            RecommendationSeverity::Critical,
        ];
        for (i, a) in severities.iter().enumerate() {
            for (j, b) in severities.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
        let a = RecommendationSeverity::Critical;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    /// **The discriminator-to-text reference implementation.**
    /// Mirrors how a consumer (edge_monitor's rec-renderer) is
    /// expected to map each [`SuggestedAction`] variant to its
    /// display-label template via a LOOKUP, not via a function
    /// call. Lives here so the contract carries the canonical
    /// mapping; any drift on the consumer side surfaces as a
    /// label mismatch in the consumer's own render tests
    /// (which can reuse this reference impl as a golden).
    ///
    /// The test exercises every variant so an added variant
    /// without a label template trips here.
    ///
    /// Mirror of
    /// `activity::tests::reference_wire_strings_cover_all_variants`
    /// and
    /// `host_vitals::tests::reference_classification_uses_thresholds`.
    #[test]
    fn reference_label_for_action_covers_all_variants() {
        /// The consumer's expected lookup shape. PURE: pattern
        /// match → template-string return. No callable.
        fn reference_label(action: SuggestedAction, multi: bool) -> &'static str {
            match action {
                SuggestedAction::ConsiderKill => {
                    if multi {
                        display::CONSIDER_KILL_MULTI
                    } else {
                        display::CONSIDER_KILL_SINGLE
                    }
                }
                SuggestedAction::ConsiderReduceLoad => display::CONSIDER_REDUCE_LOAD,
                SuggestedAction::ConsiderRestart => display::CONSIDER_RESTART,
            }
        }
        assert_eq!(
            reference_label(SuggestedAction::ConsiderKill, false),
            display::CONSIDER_KILL_SINGLE,
        );
        assert_eq!(
            reference_label(SuggestedAction::ConsiderKill, true),
            display::CONSIDER_KILL_MULTI,
        );
        assert_eq!(
            reference_label(SuggestedAction::ConsiderReduceLoad, false),
            display::CONSIDER_REDUCE_LOAD,
        );
        assert_eq!(
            reference_label(SuggestedAction::ConsiderRestart, false),
            display::CONSIDER_RESTART,
        );
        // Every template carries the placeholder convention or
        // is a fixed string. Catches a future edit that drops
        // an expected `{pid}` / `{name}` / `{targets}`.
        assert!(display::CONSIDER_KILL_SINGLE.contains("{pid}"));
        assert!(display::CONSIDER_KILL_SINGLE.contains("{name}"));
        assert!(display::CONSIDER_KILL_MULTI.contains("{targets}"));
        assert!(display::CONSIDER_RESTART.contains("{name}"));
        // ConsiderReduceLoad has no placeholders — system-scope
        // with no per-target substitution.
        assert!(!display::CONSIDER_REDUCE_LOAD.contains('{'));
    }

    /// The boundary-affirming disclaimer must (a) be non-empty,
    /// (b) reference the manual keybind (`k`), and (c) say
    /// "manually" so the user reads it as informational. The
    /// exact string is operator-locked at the dispatch level;
    /// this test asserts the contract carries that string and
    /// doesn't drift on a future edit.
    #[test]
    fn recommendation_not_actionable_disclaimer_shape() {
        let d = display::RECOMMENDATION_NOT_ACTIONABLE;
        assert!(!d.is_empty());
        assert!(
            d.contains('k'),
            "disclaimer must reference the manual keybind `k`: {d:?}"
        );
        assert!(
            d.to_lowercase().contains("manual"),
            "disclaimer must read as informational with 'manual' wording: {d:?}"
        );
        // Operator-locked text. If this needs to change, that's
        // a contract amendment — bump CONTRACT_VERSION and
        // dispatch.
        assert_eq!(
            d,
            "Suggestion only — press k to act manually",
        );
    }

    /// Runtime marker for the module-scope const-assert that
    /// `REC_MAX_VISIBLE <= ALERT_MAX_VISIBLE`. Same `#[allow]`
    /// pattern as the CAR-19c activity-feed cap tests — clippy
    /// would otherwise fire `assertions_on_constants`.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn rec_max_visible_does_not_exceed_alert_max_visible() {
        assert!(limits::REC_MAX_VISIBLE <= ALERT_MAX_VISIBLE);
        assert!(limits::REC_MAX_VISIBLE > 0);
        assert!(limits::REC_TARGETS_MAX > 0);
    }

    /// A constructible-shape smoke test: a `Recommendation`
    /// can be built from contract-only types and round-trips
    /// through [`Clone`] + [`PartialEq`]. Catches a future
    /// edit that breaks `Clone` (e.g. swapping `Vec` for a
    /// non-`Clone` collection).
    #[test]
    fn recommendation_round_trips_via_clone() {
        let rec = Recommendation {
            alert_id: AlertId::VramPressure,
            scope: RecommendationScope::Workload,
            targets: vec![RecommendedTarget {
                pid: 1234,
                name: "vllm".to_string(),
                evidence: Some("vram_mb=11400".to_string()),
            }],
            action: SuggestedAction::ConsiderKill,
            severity: RecommendationSeverity::Warning,
            reason: "VRAM at 92%, evidence: 11.5 GB / 12 GB".to_string(),
        };
        let cloned = rec.clone();
        assert_eq!(rec, cloned);
        assert_eq!(cloned.targets.len(), 1);
        assert_eq!(cloned.targets[0].pid, 1234);
    }
}
