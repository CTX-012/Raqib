//! Property-based checks for the governor — the place CLAUDE.md says
//! correctness matters most. Two invariants we can't afford to break:
//!
//!   1. Allowlisted processes are never marked for kill, regardless of
//!      category or rate-limit state.
//!   2. With max_kills=N, no snapshot produces more than N
//!      SignalTermSent decisions in one evaluate() call.

use std::collections::HashMap;

use edge_monitor::governor::policy::PolicyAction;
use edge_monitor::governor::{GovernorExecutor, GovernorPolicy, KillAction};
use edge_monitor::lifecycle::{LifecycleSnapshot, ProcessLifecycle};
use edge_monitor::model::{AICategory, ProcessSample};
use proptest::prelude::*;

fn sample(pid: u32, name: &str) -> ProcessSample {
    ProcessSample {
        pid,
        ppid: Some(1),
        name: name.into(),
        cmdline: vec![name.into()],
        environ: HashMap::new(),
        cwd: None,
        ..Default::default()
    }
}

fn snapshot_of(entries: &[(u32, &str, Option<AICategory>)]) -> LifecycleSnapshot {
    let mut snap = LifecycleSnapshot::new();
    for (pid, name, cat) in entries {
        snap.processes
            .insert(*pid, ProcessLifecycle::new(&sample(*pid, name), *cat));
    }
    snap
}

proptest! {
    #[test]
    fn allowlisted_processes_never_killed(n_procs in 1usize..30, cats in proptest::collection::vec(any::<bool>(), 1..30)) {
        // Random set of processes, all named "sshd" (an allowlisted name in
        // the default policy). Regardless of AI category flipping, none of
        // them may receive a SignalTermSent action.
        let policy = GovernorPolicy::safe_default();
        let mut executor = GovernorExecutor::new(policy);

        let entries: Vec<(u32, &str, Option<AICategory>)> = cats
            .iter()
            .take(n_procs)
            .enumerate()
            .map(|(i, is_ai)| {
                let cat = if *is_ai { Some(AICategory::Inference) } else { None };
                (i as u32 + 100, "sshd", cat)
            })
            .collect();

        let snap = snapshot_of(&entries);
        let decisions = executor.evaluate(&snap, &[], &edge_monitor::governor::threshold_breach::HostBreach::default());
        for (_, action, _) in &decisions {
            prop_assert!(
                !matches!(
                    action,
                    KillAction::SignalTermSent | KillAction::SignalKillSent,
                ),
                "allowlisted process must never be killed, got {:?}",
                action
            );
        }
    }

    /// CONVERTED (formerly `rate_limit_is_a_hard_ceiling`). Post-
    /// Candidate-E, the rate-limit enforcement moved from decision
    /// time (`evaluate()`) to the actuation site
    /// (`Runtime::record_governor_audit`). At the executor layer,
    /// evaluate() no longer caps — it emits SignalTermSent for
    /// every sustain-eligible candidate. The property this test now
    /// pins is the DECISION-layer half of the split-of-
    /// responsibilities: an empty budget doesn't spuriously cap the
    /// decision count. The "hard ceiling" property itself is proven
    /// by the runtime test
    /// `actuation_site_rate_limit_defers_when_budget_exhausted`
    /// (which drives record_governor_audit end-to-end) — that's where
    /// the OS-level "no more than N kills per window" invariant now
    /// lives.
    #[test]
    fn evaluate_emits_signal_term_for_all_eligible_candidates(
        max in 0u32..10,
        n_candidates in 0usize..20
    ) {
        let mut policy = GovernorPolicy::safe_default();
        policy.default_ai_action = PolicyAction::Kill;
        policy.rate_limit_max_kills = max;
        policy.rate_limit_window_secs = 60;
        let mut executor = GovernorExecutor::new(policy);

        let entries: Vec<(u32, &str, Option<AICategory>)> = (0..n_candidates)
            .map(|i| (i as u32 + 500, "offender", Some(AICategory::Inference)))
            .collect();

        let snap = snapshot_of(&entries);
        let breaches: Vec<edge_monitor::governor::threshold_breach::ThresholdBreach> = entries
            .iter()
            .map(|(pid, _, _)| edge_monitor::governor::threshold_breach::ThresholdBreach {
                pid: *pid,
                vram_pct: Some(99.0),
                vram_breached: true,
                ..edge_monitor::governor::threshold_breach::ThresholdBreach::default()
            })
            .collect();
        let decisions = executor.evaluate(&snap, &breaches, &edge_monitor::governor::threshold_breach::HostBreach::default());
        let signal_term = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
            .count();

        // Post-Candidate-E: recent_kills is empty (freshly-
        // constructed executor), so rate_limit_exceeded() returns
        // false for every candidate → all N get SignalTermSent.
        // The actuation-site enforcement is what caps this at the
        // budget when it fires them — that path is tested
        // separately in `actuation_site_rate_limit_defers_when_budget_exhausted`.
        prop_assert_eq!(
            signal_term, n_candidates,
            "post-fix executor emits SignalTermSent for every sustain-eligible \
             candidate; rate cap enforcement moved to record_governor_audit. \
             Got signal_term={} for n_candidates={} (max was {}).",
            signal_term, n_candidates, max
        );
    }
}
