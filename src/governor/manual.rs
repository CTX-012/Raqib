use chrono::{DateTime, Utc};
use libc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::governor::GovernorExecutor;
use crate::lifecycle::LifecycleSnapshot;
use crate::model::AICategory;

#[derive(Debug, Clone, Error)]
pub enum ManualKillError {
    #[error("kill error: {0}")]
    KillError(String),
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("invalid action: {0}")]
    InvalidAction(String),
}

pub type ManualKillResult<T> = Result<T, ManualKillError>;

/// Distinguishes user-initiated kills from governor-automated kills in the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillSource {
    Manual,
    Automated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManualKillAction {
    SendSigterm,
    SendSigkill,
    Cancelled,
    /// PID-reuse guard fired: SIGKILL refused because the captured
    /// identity (pidfd or `/proc/<pid>/stat` starttime) no longer
    /// matches the live process at this PID. The audit trail records
    /// the abort so operators can correlate with system logs.
    PidReusedAborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub timestamp: DateTime<Utc>,
    pub action: ManualKillAction,
    pub source: KillSource,
    pub pid: u32,
    pub process_name: String,
    pub category: Option<AICategory>,
    pub reason: String,
    pub success: bool,
    pub error_msg: Option<String>,
}

impl AuditLogEntry {
    pub fn success(
        action: ManualKillAction,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            action,
            source: KillSource::Manual,
            pid,
            process_name: name,
            category,
            reason,
            success: true,
            error_msg: None,
        }
    }

    pub fn failure(
        action: ManualKillAction,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
        error: String,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            action,
            source: KillSource::Manual,
            pid,
            process_name: name,
            category,
            reason,
            success: false,
            error_msg: Some(error),
        }
    }

    /// DISPATCH 80 / C4 — audit constructor for the automated
    /// (governor-driven) kill path. Sets `source = KillSource::Automated`
    /// so the audit trail distinguishes operator-initiated kills
    /// (via `Runtime::manual_kill`) from auto-actuation kills (via
    /// `Runtime::record_governor_audit` when `governor.auto_actuate
    /// == true`). Display surfaces (TUI audit panel, JSONL trail)
    /// already render the source field — the new variant lights up
    /// existing UI without code changes there.
    pub fn automated_success(
        action: ManualKillAction,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            action,
            source: KillSource::Automated,
            pid,
            process_name: name,
            category,
            reason,
            success: true,
            error_msg: None,
        }
    }

    /// DISPATCH 80 / C4 — audit constructor for an automated kill
    /// attempt that the OS rejected (ESRCH, EPERM, …). The PID
    /// stays OFF `governor_killed_pids` (the failed kill must not
    /// pre-tag the eventual exit as a governor kill — see
    /// `runtime::manual_kill`'s analogous guard) but the audit
    /// trail still records the attempt so operators can correlate
    /// with system logs.
    pub fn automated_failure(
        action: ManualKillAction,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
        error: String,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            action,
            source: KillSource::Automated,
            pid,
            process_name: name,
            category,
            reason,
            success: false,
            error_msg: Some(error),
        }
    }

    pub fn to_audit_line(&self) -> String {
        let action_str = match self.action {
            ManualKillAction::SendSigterm => "SIGTERM",
            ManualKillAction::SendSigkill => "SIGKILL",
            ManualKillAction::Cancelled => "CANCELLED",
            ManualKillAction::PidReusedAborted => "PidReusedAborted",
        };
        let status_str = if self.success { "OK" } else { "FAIL" };
        let source_str = match self.source {
            KillSource::Manual => "manual",
            KillSource::Automated => "automated",
        };
        let cat_str = self
            .category
            .map(|c| format!("{:?}", c))
            .unwrap_or_else(|| "N/A".to_string());
        let error_str = self
            .error_msg
            .as_ref()
            .map(|e| format!(" ({})", e))
            .unwrap_or_default();

        format!(
            "[{}] {} {} source={} PID={} name={} category={} reason={}{}\n",
            self.timestamp.format("%Y-%m-%d %H:%M:%S"),
            action_str,
            status_str,
            source_str,
            self.pid,
            self.process_name,
            cat_str,
            self.reason,
            error_str,
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditLog {
    entries: Vec<AuditLogEntry>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn log(&mut self, entry: AuditLogEntry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[AuditLogEntry] {
        &self.entries
    }

    pub fn successful_kills(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.success && e.action != ManualKillAction::Cancelled)
            .count()
    }

    pub fn failed_kills(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !e.success && e.action != ManualKillAction::Cancelled)
            .count()
    }

    pub fn to_report(&self) -> String {
        let mut report = String::new();
        report.push_str("=== AUDIT LOG ===\n");
        report.push_str(&format!(
            "Total entries: {}, Successful: {}, Failed: {}\n\n",
            self.entries.len(),
            self.successful_kills(),
            self.failed_kills(),
        ));
        for entry in &self.entries {
            report.push_str(&entry.to_audit_line());
        }
        report
    }
}

/// Executor for manual kill operations with audit logging.
/// Manual kills bypass automated policy thresholds but still respect the allowlist.
pub struct ManualKiller {
    audit_log: AuditLog,
}

impl ManualKiller {
    pub fn new() -> Self {
        Self {
            audit_log: AuditLog::new(),
        }
    }

    /// Find process by PID in the lifecycle snapshot.
    pub fn find_by_pid(
        pid: u32,
        snapshot: &LifecycleSnapshot,
    ) -> ManualKillResult<(u32, String, Option<AICategory>)> {
        snapshot
            .processes
            .get(&pid)
            .map(|lc| (lc.pid, lc.name.clone(), lc.category))
            .ok_or_else(|| ManualKillError::TargetNotFound(format!("PID {} not found", pid)))
    }

    /// Find process by index in sorted PID list (serial-ID lookup for TUI).
    pub fn find_by_index(
        index: usize,
        snapshot: &LifecycleSnapshot,
    ) -> ManualKillResult<(u32, String, Option<AICategory>)> {
        let mut pids: Vec<u32> = snapshot.processes.keys().copied().collect();
        pids.sort();
        pids.get(index)
            .map(|pid| {
                let lc = &snapshot.processes[pid];
                (lc.pid, lc.name.clone(), lc.category)
            })
            .ok_or_else(|| {
                ManualKillError::TargetNotFound(format!("Process index {} out of range", index))
            })
    }

    /// Return true if the process is on the governor allowlist.
    /// Allowlisted processes surface the inline `(ALLOWLISTED)` tag
    /// on the kill_confirm card so the operator can see the
    /// allowlist hit before confirming.
    ///
    /// Sprint-7 Item 1 — pre-Sprint-7 this delegated to
    /// `policy.evaluate(name, None)`, which falls through to the
    /// "non-AI processes: allow by default" branch and returned
    /// `PolicyAction::Allow` for EVERY process. Every kill_confirm
    /// card flashed `(ALLOWLISTED)` regardless of whether the
    /// process was on the actual allowlist. The fix is to check
    /// `whitelist_names` directly; the rest of `evaluate`'s logic
    /// (blacklist + default_ai_action) belongs to the automated
    /// governor path and doesn't apply to "is this process on the
    /// allowlist?".
    pub fn is_allowlisted(name: &str, governor: &GovernorExecutor) -> bool {
        governor.policy().whitelist_names.contains(name)
    }

    /// Send SIGTERM to process (graceful shutdown).
    pub fn kill_sigterm(
        &mut self,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
    ) -> ManualKillResult<()> {
        tracing::info!("Sending SIGTERM to PID {}: {}", pid, name);

        // SAFETY: libc::kill is always safe to call with a valid signal number.
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc != 0 {
            let error = format!("SIGTERM failed: {}", std::io::Error::last_os_error());
            self.audit_log.log(AuditLogEntry::failure(
                ManualKillAction::SendSigterm,
                pid,
                name,
                category,
                reason,
                error.clone(),
            ));
            return Err(ManualKillError::KillError(error));
        }

        self.audit_log.log(AuditLogEntry::success(
            ManualKillAction::SendSigterm,
            pid,
            name,
            category,
            reason,
        ));
        Ok(())
    }

    /// Send SIGKILL to process (forceful termination).
    pub fn kill_sigkill(
        &mut self,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
    ) -> ManualKillResult<()> {
        tracing::info!("Sending SIGKILL to PID {}: {}", pid, name);

        // SAFETY: libc::kill is always safe to call with a valid signal number.
        let rc = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if rc != 0 {
            let error = format!("SIGKILL failed: {}", std::io::Error::last_os_error());
            self.audit_log.log(AuditLogEntry::failure(
                ManualKillAction::SendSigkill,
                pid,
                name,
                category,
                reason,
                error.clone(),
            ));
            return Err(ManualKillError::KillError(error));
        }

        self.audit_log.log(AuditLogEntry::success(
            ManualKillAction::SendSigkill,
            pid,
            name,
            category,
            reason,
        ));
        Ok(())
    }

    pub fn audit_log(&self) -> &AuditLog {
        &self.audit_log
    }

    pub fn audit_report(&self) -> String {
        self.audit_log.to_report()
    }
}

impl Default for ManualKiller {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_lifecycle(
        pid: u32,
        name: &str,
        category: Option<AICategory>,
    ) -> crate::lifecycle::ProcessLifecycle {
        let sample = crate::model::ProcessSample {
            pid,
            ppid: Some(1),
            name: name.to_string(),
            cmdline: vec![name.to_string()],
            environ: HashMap::new(),
            cwd: None,
            ..Default::default()
        };
        crate::lifecycle::ProcessLifecycle::new(&sample, category)
    }

    #[test]
    fn manual_killer_new() {
        let killer = ManualKiller::new();
        assert_eq!(killer.audit_log().entries().len(), 0);
    }

    #[test]
    fn find_by_pid_found() {
        let mut snapshot = LifecycleSnapshot::new();
        snapshot.processes.insert(
            100,
            make_lifecycle(100, "test", Some(AICategory::Inference)),
        );

        let result = ManualKiller::find_by_pid(100, &snapshot);
        assert!(result.is_ok());
        let (pid, name, cat) = result.unwrap();
        assert_eq!(pid, 100);
        assert_eq!(name, "test");
        assert_eq!(cat, Some(AICategory::Inference));
    }

    #[test]
    fn find_by_pid_not_found() {
        let snapshot = LifecycleSnapshot::new();
        assert!(ManualKiller::find_by_pid(999, &snapshot).is_err());
    }

    #[test]
    fn find_by_index_sorted() {
        let mut snapshot = LifecycleSnapshot::new();
        snapshot
            .processes
            .insert(101, make_lifecycle(101, "proc2", None));
        snapshot
            .processes
            .insert(100, make_lifecycle(100, "proc1", None));

        let (pid, _, _) = ManualKiller::find_by_index(0, &snapshot).unwrap();
        assert_eq!(pid, 100); // lowest PID comes first after sort
    }

    #[test]
    fn find_by_index_out_of_range() {
        let snapshot = LifecycleSnapshot::new();
        assert!(ManualKiller::find_by_index(0, &snapshot).is_err());
    }

    /// Audit-log surface test. Constructs entries directly through the
    /// `AuditLogEntry::success` constructor (the path `kill_sigterm`
    /// takes when the kernel signal returns 0) so the assertion doesn't
    /// depend on whether PID 100 happens to be alive on the test host —
    /// post-CAR-17 there is no dry-run mode to stub the signal out.
    #[test]
    fn audit_report_contains_expected_fields() {
        let mut log = AuditLog::new();
        log.log(AuditLogEntry::success(
            ManualKillAction::SendSigterm,
            100,
            "proc".to_string(),
            Some(AICategory::Inference),
            "manual".to_string(),
        ));
        let report = log.to_report();
        assert!(report.contains("AUDIT LOG"));
        assert!(report.contains("SIGTERM"));
        assert!(report.contains("manual"));
    }

    #[test]
    fn audit_entry_source_is_manual() {
        let entry = AuditLogEntry::success(
            ManualKillAction::SendSigterm,
            100,
            "proc".to_string(),
            None,
            "r".to_string(),
        );
        assert_eq!(entry.source, KillSource::Manual);
    }

    // ── Sprint-7 Item 1 — is_allowlisted scope regression guards ──

    #[test]
    fn is_allowlisted_only_matches_whitelist_entries() {
        // Pre-Sprint-7 this delegated to `policy.evaluate(name, None)`
        // which returned Allow for any process not on the blacklist
        // — every kill_confirm card flashed (ALLOWLISTED) even for
        // ollama, claude, vllm, etc. Pin the fix: only names in the
        // explicit whitelist count as allowlisted.
        let executor = crate::governor::executor::GovernorExecutor::new(
            crate::governor::policy::GovernorPolicy::safe_default(),
        );
        assert!(ManualKiller::is_allowlisted("sshd", &executor));
        assert!(ManualKiller::is_allowlisted("systemd", &executor));
        assert!(ManualKiller::is_allowlisted("init", &executor));
        // The user-reported smoke bug: ollama IS NOT on the default
        // whitelist but pre-fix `is_allowlisted` returned true,
        // surfacing the (ALLOWLISTED) tag on the kill_confirm card.
        assert!(!ManualKiller::is_allowlisted("ollama", &executor));
        assert!(!ManualKiller::is_allowlisted("claude", &executor));
        assert!(!ManualKiller::is_allowlisted("node", &executor));
        assert!(!ManualKiller::is_allowlisted("python3", &executor));
        assert!(!ManualKiller::is_allowlisted("vllm", &executor));
    }

    #[test]
    fn is_allowlisted_respects_runtime_added_entries() {
        // The whitelist is a runtime-mutable set (operators add
        // to it via the `[policy].allowlist` TOML field). A name
        // added after the fact must register as allowlisted, and
        // a name not added must NOT.
        let mut policy = crate::governor::policy::GovernorPolicy::safe_default();
        policy.whitelist("my_critical_app");
        let executor = crate::governor::executor::GovernorExecutor::new(policy);
        assert!(ManualKiller::is_allowlisted("my_critical_app", &executor));
        assert!(!ManualKiller::is_allowlisted("nginx", &executor));
    }

    #[test]
    fn default_whitelist_protects_only_system_processes() {
        // Sprint-7 Item 1 invariant: the safe-default whitelist
        // covers SHELLS + INIT + KERNEL THREADS only. No AI
        // workload, browser, IDE, or daemon should land here by
        // default. If a future commit accidentally adds e.g.
        // "ollama" to safe_default, this test breaks loudly.
        let policy = crate::governor::policy::GovernorPolicy::safe_default();
        let expected: std::collections::HashSet<String> = [
            "sshd", "bash", "zsh", "sh", "systemd", "init", "kworker", "kthreadd",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            policy.whitelist_names, expected,
            "safe_default whitelist drifted from system-only scope; \
             non-system additions must justify a CLAUDE.md update"
        );
    }
}
