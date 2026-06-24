//! v1.3.2 / DISPATCH 91 / PHASE 5 step 5 — `HistoryEvent` builders
//! from the three runtime event sources.
//!
//! These mirror the WIRE-side formatters in
//! [`crate::web::wire`] (the private `format_exit_summary` /
//! `format_audit_summary` / `format_regression_summary`) so the
//! history archive surfaces the SAME text the live activity feed
//! shows. The future `/api/history` endpoint (PHASE 5 step 6) and
//! the Svelte view (step 7) read straight from the archive, so
//! summary parity here means parity in the operator's eyes.
//!
//! ## Why builders, not impls
//!
//! The three source types ([`LifecycleSummary`],
//! [`crate::governor::manual::AuditLogEntry`],
//! [`crate::analysis::RegressionEvent`]) live in modules that
//! shouldn't grow knowledge of the history layer. Putting builders
//! HERE keeps the dependency direction one-way: history reads
//! source types; source types don't reach into history.
//!
//! ## Structural dedup
//!
//! The three runtime sources are themselves bounded rings (state.completed,
//! state.audit, state.regressions) that receive each event ONCE.
//! The archive is written at the same "moment the event lands"
//! sites — the exit-drain loop, the audit push sites, the
//! regression-count loop. There's no re-emission per tick. The
//! resulting archive is structurally deduped without a key check.

use super::{HistoryEvent, HistoryEventKind};
use crate::analysis::{RegressionEvent, Severity};
use crate::governor::manual::{AuditLogEntry, KillSource, ManualKillAction};
use crate::lifecycle::LifecycleSummary;

/// Build an Exit history event from a freshly-surfaced
/// [`LifecycleSummary`]. Caller is the runtime's exit-drain loop;
/// the AI-only filter is enforced at the call site (mirrors the
/// wire/TUI activity feed's filter).
pub fn exit_event(s: &LifecycleSummary) -> HistoryEvent {
    HistoryEvent {
        timestamp: s.exit_time,
        pid: s.pid,
        name: s.name.clone(),
        kind: HistoryEventKind::Exit,
        summary: format_exit_summary(s),
    }
}

/// Build a Kill history event from a just-pushed
/// [`AuditLogEntry`]. Caller is the runtime path that pushed the
/// entry into `state.audit` (manual_kill, manual_force_kill, the
/// two record_governor_audit sites).
pub fn kill_event(e: &AuditLogEntry) -> HistoryEvent {
    HistoryEvent {
        timestamp: e.timestamp,
        pid: e.pid,
        name: e.process_name.clone(),
        kind: HistoryEventKind::Kill,
        summary: format_audit_summary(e),
    }
}

/// Build a Regression history event from a just-pushed
/// [`RegressionEvent`]. Caller is the runtime's regression-count
/// iteration loop (the same `state.regressions.iter().skip(regs_before)`
/// scan that increments the Prom counter).
///
/// PID is `0` — regressions are model-scoped, not PID-scoped, and
/// the wire shape carries that sentinel already
/// ([`crate::web::wire::WireActivityEntry::from_regression_event`]).
/// The model name lands in `name` so the future view still has a
/// human label.
pub fn regression_event(r: &RegressionEvent) -> HistoryEvent {
    HistoryEvent {
        timestamp: r.timestamp,
        pid: 0,
        name: r.model.clone(),
        kind: HistoryEventKind::Regression,
        summary: format_regression_summary(r),
    }
}

// ── summary formatters — kept byte-identical to the wire/TUI ──────
//
// If the wire's private formatters in `web/wire.rs` drift, surface
// the divergence rather than copy-pasting again; the long-term
// resolution is for the wire to consume these (PHASE 5 step 6
// refactor — single source of truth). For D91 the duplication is
// intentional and bounded to the three small functions below.

fn format_exit_summary(s: &LifecycleSummary) -> String {
    let cat = s.category.map(|c| format!("{c:?}")).unwrap_or_default();
    let mut row = format!("exit pid={} {} {}", s.pid, s.name, cat);
    if let Some(model) = s.model_name.as_deref().filter(|m| !m.is_empty()) {
        row.push_str(&format!(" model={model}"));
    }
    row.push_str(&format!(
        " cpu avg={:.0}% peak={:.0}% RAM {} MB",
        s.avg_cpu_pct, s.peak_cpu_pct, s.peak_rss_mb,
    ));
    if s.peak_vram_mb > 0 {
        row.push_str(&format!(", GPU memory {} MB", s.peak_vram_mb));
    }
    row.push_str(&format!(" up={}s", s.uptime_secs));
    row
}

fn format_audit_summary(e: &AuditLogEntry) -> String {
    let action = match e.action {
        ManualKillAction::SendSigterm => "SIGTERM",
        ManualKillAction::SendSigkill => "SIGKILL",
        ManualKillAction::Cancelled => "CANCELLED",
        ManualKillAction::PidReusedAborted => "ABORT-PID-REUSE",
    };
    let source = match e.source {
        KillSource::Manual => "manual",
        KillSource::Automated => "auto",
    };
    let status = if e.success { "OK" } else { "FAIL" };
    format!(
        "{} {} {} pid={} {} - {}",
        action, status, source, e.pid, e.process_name, e.reason,
    )
}

fn format_regression_summary(r: &RegressionEvent) -> String {
    let _ = Severity::Critical; // proof-of-import; severity prints via Debug below
    format!(
        "REGRESSION {:?} {} {} {:+.1}% (n={})",
        r.regression.severity,
        r.model,
        r.regression.metric,
        r.regression.delta_pct,
        r.baseline_size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn fake_summary() -> LifecycleSummary {
        LifecycleSummary {
            pid: 42,
            name: "ollama".into(),
            category: Some(crate::model::AICategory::Inference),
            model_name: Some("llama3-8b".into()),
            spawn_time: DateTime::from_timestamp(0, 0).unwrap(),
            exit_time: DateTime::from_timestamp(60, 0).unwrap(),
            uptime_secs: 60,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 17.5,
            peak_cpu_pct: 80.0,
            peak_rss_mb: 4500,
            peak_vram_mb: 4800,
            samples: 60,
            trajectory: None,
        }
    }

    #[test]
    fn exit_event_carries_kind_pid_name_and_summary() {
        let ev = exit_event(&fake_summary());
        assert!(matches!(ev.kind, HistoryEventKind::Exit));
        assert_eq!(ev.pid, 42);
        assert_eq!(ev.name, "ollama");
        assert!(ev.summary.starts_with("exit pid=42 ollama"));
        assert!(ev.summary.contains("Inference"));
        assert!(ev.summary.contains("model=llama3-8b"));
        assert!(ev.summary.contains("GPU memory 4800 MB"));
        assert!(ev.summary.contains("up=60s"));
    }

    #[test]
    fn kill_event_renders_action_source_and_status() {
        let entry = AuditLogEntry {
            timestamp: Utc::now(),
            action: ManualKillAction::SendSigterm,
            source: KillSource::Manual,
            pid: 100,
            process_name: "python3".into(),
            category: None,
            reason: "user request".into(),
            success: true,
            error_msg: None,
        };
        let ev = kill_event(&entry);
        assert!(matches!(ev.kind, HistoryEventKind::Kill));
        assert_eq!(ev.pid, 100);
        assert_eq!(ev.name, "python3");
        assert!(ev.summary.contains("SIGTERM OK manual pid=100 python3 - user request"));
    }

    #[test]
    fn regression_event_uses_model_name_with_pid_zero_sentinel() {
        use crate::analysis::compare::{Regression, Severity};
        let r = RegressionEvent {
            timestamp: Utc::now(),
            model: "phi3-mini".into(),
            regression: Regression {
                metric: "tokens_per_sec".into(),
                baseline: 100.0,
                current: 82.0,
                delta_pct: -18.0,
                severity: Severity::Warn,
            },
            baseline_size: 10,
        };
        let ev = regression_event(&r);
        assert!(matches!(ev.kind, HistoryEventKind::Regression));
        assert_eq!(
            ev.pid, 0,
            "regressions are model-scoped; PID 0 sentinel matches the \
             wire (D71) and TUI activity branches"
        );
        assert_eq!(ev.name, "phi3-mini");
        assert!(ev.summary.contains("REGRESSION"));
        assert!(ev.summary.contains("phi3-mini"));
        assert!(ev.summary.contains("tokens_per_sec"));
        assert!(ev.summary.contains("-18.0%"));
        assert!(ev.summary.contains("n=10"));
    }
}
