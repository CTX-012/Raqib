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

    #[test]
    fn rate_limit_is_a_hard_ceiling(
        max in 0u32..10,
        n_candidates in 0usize..20
    ) {
        let mut policy = GovernorPolicy::safe_default();
        // v1.0.1 B-NEW-1 — `safe_default()` now leaves
        // `default_ai_action = Allow`, so no rate-limit ceiling can
        // be exercised without first opting back in to kills. This
        // property test is specifically about the rate-limit
        // invariant, so the opt-in is the right scope.
        policy.default_ai_action = PolicyAction::Kill;
        policy.rate_limit_max_kills = max;
        policy.rate_limit_window_secs = 60;
        let mut executor = GovernorExecutor::new(policy);

        // Candidates named "offender" are not in the default allowlist and
        // are classified Inference — every one is kill-eligible.
        let entries: Vec<(u32, &str, Option<AICategory>)> = (0..n_candidates)
            .map(|i| (i as u32 + 500, "offender", Some(AICategory::Inference)))
            .collect();

        let snap = snapshot_of(&entries);
        // v1.3.2 / DISPATCH 78 — every candidate breaches so the
        // rate-limit ceiling is the binding constraint, not the
        // breach gate. Mirrors the executor's
        // `executor_rate_limits_enforced_kills` test fixture.
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
        let killed = decisions
            .iter()
            .filter(|(_, a, _)| *a == KillAction::SignalTermSent)
            .count();

        if max == 0 {
            prop_assert_eq!(killed, n_candidates, "max=0 means unlimited");
        } else {
            prop_assert!(
                killed <= max as usize,
                "killed {} exceeds rate limit max {}",
                killed,
                max,
            );
        }
    }
}
