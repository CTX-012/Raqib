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
    dry_run: bool,
}

impl ManualKiller {
    pub fn new(dry_run: bool) -> Self {
        Self {
            audit_log: AuditLog::new(),
            dry_run,
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
    /// Allowlisted processes require explicit override confirm before killing.
    pub fn is_allowlisted(name: &str, governor: &GovernorExecutor) -> bool {
        matches!(
            governor.policy().evaluate(name, None),
            crate::governor::policy::PolicyAction::Allow
        )
    }

    /// Send SIGTERM to process (graceful shutdown). Dry-run mode logs only.
    pub fn kill_sigterm(
        &mut self,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
    ) -> ManualKillResult<()> {
        if self.dry_run {
            tracing::info!("DRY-RUN: SIGTERM would be sent to PID {}: {}", pid, name);
            self.audit_log.log(AuditLogEntry::success(
                ManualKillAction::SendSigterm,
                pid,
                name,
                category,
                format!("{} (dry-run)", reason),
            ));
            return Ok(());
        }

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

    /// Send SIGKILL to process (forceful termination). Dry-run mode logs only.
    pub fn kill_sigkill(
        &mut self,
        pid: u32,
        name: String,
        category: Option<AICategory>,
        reason: String,
    ) -> ManualKillResult<()> {
        if self.dry_run {
            tracing::info!("DRY-RUN: SIGKILL would be sent to PID {}: {}", pid, name);
            self.audit_log.log(AuditLogEntry::success(
                ManualKillAction::SendSigkill,
                pid,
                name,
                category,
                format!("{} (dry-run)", reason),
            ));
            return Ok(());
        }

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

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Toggle dry-run mode at runtime (UI keybinding).
    pub fn set_dry_run(&mut self, dry_run: bool) {
        self.dry_run = dry_run;
    }

    pub fn audit_report(&self) -> String {
        self.audit_log.to_report()
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
        let killer = ManualKiller::new(true);
        assert!(killer.is_dry_run());
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

    #[test]
    fn dry_run_sigterm_logs_not_kills() {
        let mut killer = ManualKiller::new(true);
        killer
            .kill_sigterm(
                100,
                "proc".to_string(),
                Some(AICategory::Inference),
                "test".to_string(),
            )
            .unwrap();
        assert_eq!(killer.audit_log().entries().len(), 1);
        assert_eq!(killer.audit_log().successful_kills(), 1);
    }

    #[test]
    fn dry_run_sigkill_logs_not_kills() {
        let mut killer = ManualKiller::new(true);
        killer
            .kill_sigkill(100, "proc".to_string(), None, "test".to_string())
            .unwrap();
        assert_eq!(killer.audit_log().entries().len(), 1);
    }

    #[test]
    fn audit_tracks_multiple_entries() {
        let mut killer = ManualKiller::new(true);
        killer
            .kill_sigterm(
                100,
                "p1".to_string(),
                Some(AICategory::Inference),
                "r1".to_string(),
            )
            .ok();
        killer
            .kill_sigkill(
                101,
                "p2".to_string(),
                Some(AICategory::Training),
                "r2".to_string(),
            )
            .ok();
        assert_eq!(killer.audit_log().entries().len(), 2);
        assert_eq!(killer.audit_log().successful_kills(), 2);
    }

    #[test]
    fn audit_report_contains_expected_fields() {
        let mut killer = ManualKiller::new(true);
        killer
            .kill_sigterm(
                100,
                "proc".to_string(),
                Some(AICategory::Inference),
                "manual".to_string(),
            )
            .ok();
        let report = killer.audit_report();
        assert!(report.contains("AUDIT LOG"));
        assert!(report.contains("SIGTERM"));
        assert!(report.contains("manual"));
    }

    #[test]
    fn audit_entry_source_is_manual() {
        let mut killer = ManualKiller::new(true);
        killer
            .kill_sigterm(100, "proc".to_string(), None, "r".to_string())
            .ok();
        assert_eq!(killer.audit_log().entries()[0].source, KillSource::Manual);
    }
}
