//! Property-based checks for the governor — the place CLAUDE.md says
//! correctness matters most. Two invariants we can't afford to break:
//!
//!   1. Allowlisted processes are never marked for kill, regardless of
//!      category, rate-limit state, or enforce flag.
//!   2. With enforce=true and max_kills=N, no snapshot produces more than
//!      N SignalTermSent decisions in one evaluate() call.

use std::collections::HashMap;

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
        // them may receive a SignalTermSent or DryRunTermWould action.
        let mut policy = GovernorPolicy::safe_default();
        policy.enforce = true;
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
        let decisions = executor.evaluate(&snap);
        for (_, action, _) in &decisions {
            prop_assert!(
                !matches!(
                    action,
                    KillAction::SignalTermSent
                        | KillAction::DryRunTermWould
                        | KillAction::SignalKillSent
                        | KillAction::DryRunKillWould
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
        policy.enforce = true;
        policy.rate_limit_max_kills = max;
        policy.rate_limit_window_secs = 60;
        let mut executor = GovernorExecutor::new(policy);

        // Candidates named "offender" are not in the default allowlist and
        // are classified Inference — every one is kill-eligible.
        let entries: Vec<(u32, &str, Option<AICategory>)> = (0..n_candidates)
            .map(|i| (i as u32 + 500, "offender", Some(AICategory::Inference)))
            .collect();

        let snap = snapshot_of(&entries);
        let decisions = executor.evaluate(&snap);
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
