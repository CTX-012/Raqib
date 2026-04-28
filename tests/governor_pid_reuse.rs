//! TEST.md G.1.11 — PID-reuse race in SIGKILL escalation.
//!
//! The governor must refuse to send SIGKILL when the captured PID
//! identity (pidfd or `/proc/<pid>/stat` starttime) no longer matches
//! the live process at that PID. Otherwise the kernel's free
//! reassignment of recycled PIDs can cause SIGKILL to land on an
//! unrelated process — possibly an allowlisted one.
//!
//! These tests do not depend on actually winning a PID-reuse race
//! (which is non-deterministic on a quiet host). Instead they spawn
//! a real child, exercise `request_kill`, force-exit the child, and
//! then call `execute_after_grace`. With the original gone, both the
//! pidfd path (process refers to the now-dead instance — kernel
//! returns ESRCH on `pidfd_send_signal`) and the starttime-fallback
//! path (`/proc/<pid>/stat` no longer exists) abort the SIGKILL.

use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

use edge_monitor::governor::manual::ManualKillAction;
use edge_monitor::governor::{
    AuditWriter, GovernorExecutor, GovernorPolicy, KillAction, audit::replay, manual::AuditLogEntry,
};
use edge_monitor::model::AICategory;

fn enforcing_executor(grace_secs: u64) -> GovernorExecutor {
    let mut policy = GovernorPolicy::safe_default();
    policy.enforce = true;
    policy.sigterm_grace_period_secs = grace_secs;
    GovernorExecutor::new(policy)
}

/// Spawn a child that ignores SIGTERM so we control its exit. Reading
/// from /dev/zero in a tight loop means the child stays alive until we
/// SIGKILL or exit it explicitly via the TestPid helper. SIGTERM has the
/// default action of "terminate", so a plain `sleep` would die from our
/// own SIGTERM and confuse the test fixture; using `cat /dev/zero` would
/// flood stdout. We use a small shell loop that traps SIGTERM.
fn spawn_long_lived_child() -> Child {
    Command::new("sh")
        .arg("-c")
        .arg("trap '' TERM; while :; do sleep 1; done")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn test child")
}

fn force_exit(child: &mut Child) {
    // SIGKILL bypasses the `trap '' TERM` above and reaps cleanly.
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn sigkill_aborted_when_target_already_exited() {
    let mut executor = enforcing_executor(0);
    let mut child = spawn_long_lived_child();
    let pid = child.id();

    // Step 1 — request_kill: captures pidfd + starttime, sends SIGTERM.
    // The child traps SIGTERM so it stays alive; the executor's pending
    // entry holds the captured identity tokens.
    executor
        .request_kill(pid, "sh".into(), AICategory::Inference)
        .expect("request_kill");

    // Step 2 — force-exit the child so it's reaped before grace expires.
    // /proc/<pid>/stat now disappears AND any captured pidfd refers to a
    // dead process (ESRCH from pidfd_send_signal).
    force_exit(&mut child);

    // Give the kernel a moment to settle the reap.
    thread::sleep(Duration::from_millis(50));

    // Step 3 — escalate. The PID-reuse guard must abort.
    let results = executor.execute_after_grace();
    assert_eq!(results.len(), 1, "expected one expired entry");
    let (escalated_pid, action) = &results[0];
    assert_eq!(*escalated_pid, pid);
    let action = action.as_ref().expect("send_sigkill returned err");
    assert_eq!(
        *action,
        KillAction::PidReusedAborted,
        "process exited during grace period: SIGKILL must be aborted, not sent"
    );
}

#[test]
fn pidfd_path_completes_kill_when_target_still_alive() {
    // Counterpart to the above: when the captured identity still matches,
    // the SIGKILL escalation goes through. Verifies we haven't broken the
    // happy path while adding the guard.
    let mut executor = enforcing_executor(0);
    let mut child = spawn_long_lived_child();
    let pid = child.id();

    executor
        .request_kill(pid, "sh".into(), AICategory::Inference)
        .expect("request_kill");

    // Child is still alive — escalation should send SIGKILL successfully.
    let results = executor.execute_after_grace();
    assert_eq!(results.len(), 1);
    let action = results[0].1.as_ref().expect("send_sigkill returned err");
    assert_eq!(*action, KillAction::SignalKillSent);

    // Reap so we don't leave a zombie.
    let _ = child.wait();
}

#[test]
fn audit_log_records_pid_reused_aborted() {
    // End-to-end audit-trail check. The runtime layer maps
    // KillAction::PidReusedAborted into ManualKillAction::PidReusedAborted
    // when it logs the decision; here we exercise the audit-writer with
    // that variant directly so the JSONL contains the expected token.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    let writer = AuditWriter::open(&path).expect("audit writer");

    let entry = AuditLogEntry {
        timestamp: chrono::Utc::now(),
        action: ManualKillAction::PidReusedAborted,
        source: edge_monitor::governor::manual::KillSource::Automated,
        pid: 4242,
        process_name: "sh".into(),
        category: Some(AICategory::Inference),
        reason: "PID-reuse guard fired".into(),
        success: true,
        error_msg: None,
    };
    writer.append(&entry).expect("append");
    drop(writer);

    let raw = std::fs::read_to_string(&path).expect("read audit");
    assert!(
        raw.contains("PidReusedAborted"),
        "audit JSONL must contain the literal token \"PidReusedAborted\"; \
         got:\n{raw}"
    );

    let replayed = replay(&path).expect("replay");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].action, ManualKillAction::PidReusedAborted);
    assert_eq!(replayed[0].pid, 4242);
}
