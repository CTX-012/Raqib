//! Tier 3.5 — "Why did this die?" classifier.
//!
//! Layered on top of `ExitReason::from_summary` (which knows only the
//! signal + exit code) by consulting two extra sources:
//!
//! * **dmesg / kernel ring** — the Linux OOM killer logs a line like
//!   `Out of memory: Killed process 12345 (python) ...` when it kills
//!   a process for memory pressure. We read recent dmesg lines and
//!   match on PID + the well-known phrase.
//! * **Process stderr tail** — CUDA emits `CUDA out of memory` /
//!   `CUDA error: ...` to stderr just before the process exits. The
//!   runtime feeds us recent stderr lines (when available; only
//!   processes started under `edge_monitor exec` have stdio capture
//!   today, so this is best-effort).
//!
//! All readers degrade gracefully: dmesg unreadable / journalctl
//! missing / stderr unavailable each fall back to the simpler
//! `from_summary` answer rather than producing wrong attributions.
//!
//! **PID misattribution guard.** TEST.md W.1.9 specifically calls out
//! "dmesg has OOM line for an UNRELATED PID — does NOT misattribute".
//! We never declare OOM on a SIGKILL unless the dmesg line names this
//! PID *exactly*; matching by process name is forbidden because two
//! `python` processes are common and the OOM-killer's name field is
//! truncated to 15 characters anyway.

use std::process::Command;

use crate::lifecycle::LifecycleSummary;
use crate::storage::run_store::ExitReason;

/// Maximum stderr line length to keep in the `CudaError::last_msg`
/// envelope. Long traceback lines are truncated to keep `RunRecord`
/// JSON size bounded.
const MAX_LAST_MSG_LEN: usize = 512;

/// Inputs to [`classify_exit`]. Caller assembles them; the function
/// is pure so tests can drive every branch without hitting `journalctl`.
#[derive(Debug, Clone, Default)]
pub struct ExitContext {
    /// Lines of `dmesg` / kernel-log output captured around the exit
    /// time. Order doesn't matter — we scan for matches.
    pub dmesg_lines: Vec<String>,
    /// Recent stderr lines for the process, if available.
    pub stderr_lines: Vec<String>,
    /// True when the runtime can attribute the kill to its own
    /// governor (we issued SIGTERM/SIGKILL and the exit followed).
    pub killed_by_governor: bool,
    /// Reason text the governor recorded, surfaced in
    /// `GovernorKill { reason }`.
    pub governor_reason: Option<String>,
}

/// Pure classifier — no I/O. Spec test fixtures for every arm live
/// in the unit-test module below.
pub fn classify_exit(summary: &LifecycleSummary, ctx: &ExitContext) -> ExitReason {
    // Governor kills win over everything else: if we sent the signal,
    // we own the attribution.
    if ctx.killed_by_governor {
        return ExitReason::GovernorKill {
            reason: ctx.governor_reason.clone().unwrap_or_default(),
        };
    }

    // SIGSEGV → Segfault (already handled by `from_summary` but we
    // re-encode it here so the precedence is explicit and the
    // downstream OOM/CUDA checks don't override it).
    if summary.signal == Some(11) {
        return ExitReason::Segfault;
    }

    // OOM detection. Two independent signals:
    //  * Kernel OOM-killer fires SIGKILL and emits a dmesg line
    //    naming the PID. Match on PID, not name — two `python`
    //    processes are too common.
    //  * CUDA out-of-memory shows up in stderr (no kernel record).
    //
    // v1.3.2 / DISPATCH 73 (P1#2) — admit `signal=None` as well as
    // `Some(9)`. Passive-monitored exits never get a wait() status
    // (tracker.rs:56 honestly emits `mark_exit(None, None)` because
    // we don't own the child), so the previous strict
    // `signal == Some(9)` gate locked passive OOMs out — an
    // ollama-shaped workload killed by the kernel OOM-killer
    // recorded `exit_kind="unknown"` even when dmesg had a
    // matching `Killed process N` line. We open the gate to
    // `Some(9) | None` but keep it closed for other Some(_)
    // values: a `SIGTERM` (15), `SIGSEGV` (11), or `SIGINT` (2)
    // exit MUST NOT be misclassified as OOM even when an
    // unrelated dmesg OOM line for a PID-reuse predecessor sits
    // in the journal lookback. The `dmesg_killed_pid` AND-clause
    // remains the primary specificity guard (OOM phrase AND
    // matching PID).
    let signal_admits_oom = matches!(summary.signal, Some(9) | None);
    let kernel_oom = signal_admits_oom && dmesg_killed_pid(ctx, summary.pid);
    let cuda_oom = stderr_matches_cuda_oom(&ctx.stderr_lines);
    if kernel_oom || cuda_oom {
        return ExitReason::OutOfMemory {
            ram: kernel_oom,
            vram: cuda_oom,
        };
    }

    // CUDA error other than OOM (illegal access, driver crash).
    if let Some(msg) = stderr_first_cuda_error(&ctx.stderr_lines) {
        return ExitReason::CudaError {
            last_msg: Some(truncate(&msg, MAX_LAST_MSG_LEN)),
        };
    }

    // Fall through to the trivial cases (signal / exit code / unknown).
    ExitReason::from_summary(summary)
}

/// v1.3.2 / DISPATCH 74 — project an [`ExitReason`] to the
/// wire-stable `(kind, detail)` string pair the activity-feed
/// detail surface consumes. Mirrors the per-variant table inline
/// in [`crate::web::wire::WireRunRecord::from_record`] so both
/// projections stay in sync — single source of truth, no string
/// taxonomy divergence between the legacy history surface and the
/// new shape-A activity-feed detail.
///
/// The kind strings (`"clean" | "governor" | "oom" | "signal" |
/// "segfault" | "cuda" | "crash" | "unknown"`) are pinned by the
/// per-variant tests in `src/web/wire.rs::tests::exit_kind_…`.
pub fn exit_reason_to_wire_strings(reason: &ExitReason) -> (String, Option<String>) {
    match reason {
        ExitReason::CleanExit => ("clean".into(), None),
        ExitReason::UserSignal { signal } => {
            ("signal".into(), Some(format!("signal {signal}")))
        }
        ExitReason::GovernorKill { reason } => ("governor".into(), Some(reason.clone())),
        ExitReason::Segfault => ("segfault".into(), None),
        ExitReason::OutOfMemory { ram, vram } => {
            let detail = match (ram, vram) {
                (true, true) => "RAM and GPU memory",
                (true, false) => "RAM",
                (false, true) => "GPU memory",
                (false, false) => "unknown",
            };
            ("oom".into(), Some(detail.to_string()))
        }
        ExitReason::CudaError { last_msg } => ("cuda".into(), last_msg.clone()),
        ExitReason::Crash { exit_code } => {
            ("crash".into(), Some(format!("exit {exit_code}")))
        }
        ExitReason::Unknown => ("unknown".into(), None),
    }
}

/// Returns true iff at least one dmesg line matches both the OOM
/// phrase and the literal `pid=NNN` (or `process NNN`) pattern.
fn dmesg_killed_pid(ctx: &ExitContext, pid: u32) -> bool {
    let pid_token_a = format!("process {}", pid); // "Killed process 1234 (python)"
    let pid_token_b = format!("pid={}", pid); // newer kernels use this form
    for line in &ctx.dmesg_lines {
        let lower = line.to_ascii_lowercase();
        let mentions_oom = lower.contains("out of memory")
            || lower.contains("oom-kill")
            || lower.contains("oom_reaper");
        if mentions_oom && (line.contains(&pid_token_a) || line.contains(&pid_token_b)) {
            return true;
        }
    }
    false
}

fn stderr_matches_cuda_oom(stderr_lines: &[String]) -> bool {
    stderr_lines.iter().any(|l| {
        let lower = l.to_ascii_lowercase();
        lower.contains("cuda out of memory")
            || lower.contains("cuda_error_out_of_memory")
            || lower.contains("cudamalloc") && lower.contains("out of memory")
    })
}

fn stderr_first_cuda_error(stderr_lines: &[String]) -> Option<String> {
    for line in stderr_lines {
        let lower = line.to_ascii_lowercase();
        if lower.contains("cuda error")
            || lower.contains("cudaerror")
            || lower.contains("cuda_error_")
        {
            return Some(line.clone());
        }
    }
    None
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Read recent kernel log lines via `journalctl -k --since "10 seconds
/// ago"`. Returns `Vec::new()` on any failure so the caller doesn't
/// have to special-case "no journalctl on this distro".
///
/// Why journalctl over /dev/kmsg or `dmesg` directly:
///  * /dev/kmsg requires `CAP_SYSLOG` on most distros — not granted.
///  * `dmesg` itself is also gated on recent kernels.
///  * journalctl runs as the user and exits 0 with empty output if
///    nothing is available, making the read easy to script.
pub fn read_recent_kernel_log(seconds_ago: u64) -> Vec<String> {
    let arg = format!("--since=-{}s", seconds_ago);
    let out = match Command::new("journalctl")
        .args(["-k", "--no-pager", "--output=cat", &arg])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AICategory;
    use chrono::Utc;

    fn summary(signal: Option<i32>, exit_code: Option<i32>) -> LifecycleSummary {
        LifecycleSummary {
            pid: 1234,
            name: "python".into(),
            category: Some(AICategory::Inference),
            model_name: Some("phi3-mini".into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 30,
            exit_code,
            signal,
            avg_cpu_pct: 0.0,
            peak_cpu_pct: 0.0,
            peak_rss_mb: 0,
            peak_vram_mb: 0,
            samples: 0,
        }
    }

    #[test]
    fn clean_exit_zero_status() {
        let r = classify_exit(&summary(None, Some(0)), &ExitContext::default());
        assert_eq!(r, ExitReason::CleanExit);
    }

    #[test]
    fn segfault_on_sigsegv() {
        let r = classify_exit(&summary(Some(11), None), &ExitContext::default());
        assert_eq!(r, ExitReason::Segfault);
    }

    #[test]
    fn governor_kill_attribution_wins_over_signal() {
        let ctx = ExitContext {
            killed_by_governor: true,
            governor_reason: Some("VRAM > limit".into()),
            ..ExitContext::default()
        };
        let r = classify_exit(&summary(Some(15), None), &ctx);
        assert_eq!(
            r,
            ExitReason::GovernorKill {
                reason: "VRAM > limit".into()
            }
        );
    }

    #[test]
    fn user_signal_when_unknown_signal_and_no_dmesg() {
        let r = classify_exit(&summary(Some(2), None), &ExitContext::default());
        assert_eq!(r, ExitReason::UserSignal { signal: 2 });
    }

    #[test]
    fn kernel_oom_detected_via_dmesg_pid() {
        let ctx = ExitContext {
            dmesg_lines: vec![
                "[12345.678] Out of memory: Killed process 1234 (python) total-vm:9000".into(),
            ],
            ..ExitContext::default()
        };
        let r = classify_exit(&summary(Some(9), None), &ctx);
        assert_eq!(
            r,
            ExitReason::OutOfMemory {
                ram: true,
                vram: false
            }
        );
    }

    /// Spec test: TEST.md W.1.9 — dmesg has OOM for an UNRELATED PID,
    /// and the process under test happens to have been SIGKILL'd. We
    /// must NOT misattribute.
    #[test]
    fn unrelated_dmesg_oom_does_not_misattribute() {
        let ctx = ExitContext {
            dmesg_lines: vec!["[12345.678] Out of memory: Killed process 999 (cron) ...".into()],
            ..ExitContext::default()
        };
        // Our PID is 1234 (default). SIGKILL but no matching dmesg
        // line for OUR pid → falls through to UserSignal.
        let r = classify_exit(&summary(Some(9), None), &ctx);
        assert_eq!(r, ExitReason::UserSignal { signal: 9 });
    }

    #[test]
    fn cuda_oom_via_stderr() {
        let ctx = ExitContext {
            stderr_lines: vec![
                "torch.cuda.OutOfMemoryError: CUDA out of memory. Tried to allocate 4.00 GiB"
                    .into(),
            ],
            ..ExitContext::default()
        };
        // Note: not signal-killed; CUDA-OOM typically surfaces as a
        // crash with non-zero exit code.
        let r = classify_exit(&summary(None, Some(1)), &ctx);
        assert_eq!(
            r,
            ExitReason::OutOfMemory {
                ram: false,
                vram: true
            }
        );
    }

    #[test]
    fn cuda_error_other_than_oom() {
        let ctx = ExitContext {
            stderr_lines: vec![
                "RuntimeError: CUDA error: an illegal memory access was encountered".into(),
            ],
            ..ExitContext::default()
        };
        let r = classify_exit(&summary(None, Some(1)), &ctx);
        match r {
            ExitReason::CudaError { last_msg } => {
                let msg = last_msg.unwrap();
                assert!(msg.contains("illegal memory access"));
            }
            other => panic!("expected CudaError, got {:?}", other),
        }
    }

    #[test]
    fn crash_with_nonzero_exit_no_signals() {
        let r = classify_exit(&summary(None, Some(139)), &ExitContext::default());
        assert_eq!(r, ExitReason::Crash { exit_code: 139 });
    }

    #[test]
    fn unknown_when_no_signals_no_exit_code() {
        let r = classify_exit(&summary(None, None), &ExitContext::default());
        assert_eq!(r, ExitReason::Unknown);
    }

    #[test]
    fn truncate_keeps_utf8_boundaries() {
        let s = "日本語の長い文字列".repeat(100);
        let t = truncate(&s, 50);
        assert!(t.len() <= 51); // 50 bytes + the ellipsis
        assert!(t.ends_with('…'));
    }

    /// `read_recent_kernel_log` must NOT panic on hosts without
    /// journalctl (containers, BSD, macOS via WSL adapter). It
    /// returns an empty vec.
    #[test]
    fn read_recent_kernel_log_never_panics() {
        let _ = read_recent_kernel_log(5);
    }

    // ── v1.3.2 / DISPATCH 73 (P1#2) — passive-OOM attribution ──────

    /// The core P1#2 fix: a passive-monitored exit
    /// (`summary.signal == None` — the tracker emits None when it
    /// can't observe wait status, which is the steady state for
    /// processes edge_monitor doesn't own) MUST classify as OOM
    /// when dmesg has a matching `Killed process <pid>` line.
    /// Pre-v1.3.2 this exit recorded `exit_kind="unknown"` because
    /// `exit_classify.rs:77` strictly required `signal==Some(9)`.
    #[test]
    fn passive_oom_signal_none_with_dmesg_match_classifies_oom() {
        let ctx = ExitContext {
            dmesg_lines: vec![
                "[12345.678] Out of memory: Killed process 1234 (ollama) total-vm:9000".into(),
            ],
            ..ExitContext::default()
        };
        // signal = None (passive monitoring), exit_code = None.
        let r = classify_exit(&summary(None, None), &ctx);
        assert_eq!(
            r,
            ExitReason::OutOfMemory {
                ram: true,
                vram: false,
            },
            "passive-monitored OOM (signal=None) with a matching \
             dmesg line MUST classify as OOM, not Unknown — the \
             P1#2 fix admits `Some(9) | None` to the OOM gate",
        );
    }

    /// False-OOM guard #1: a `SIGTERM` exit (signal=Some(15)) MUST
    /// NOT misclassify as OOM even when dmesg has a matching
    /// `Killed process <pid>` line. The dmesg line would belong to
    /// a PID-reuse predecessor — the kernel doesn't OOM-kill via
    /// SIGTERM. This is the load-bearing case the relaxed gate
    /// preserves (`signal_admits_oom = Some(9) | None`).
    #[test]
    fn sigterm_exit_with_dmesg_match_does_not_misattribute_to_oom() {
        let ctx = ExitContext {
            dmesg_lines: vec![
                "[12345.678] Out of memory: Killed process 1234 (predecessor) total-vm:9000".into(),
            ],
            ..ExitContext::default()
        };
        let r = classify_exit(&summary(Some(15), None), &ctx);
        assert_eq!(
            r,
            ExitReason::UserSignal { signal: 15 },
            "SIGTERM + dmesg OOM line MUST NOT classify as OOM — \
             the kernel doesn't OOM-kill via SIGTERM; this dmesg \
             entry can only be a PID-reuse predecessor's record. \
             Got: {r:?}",
        );
    }

    /// False-OOM guard #2: a `SIGSEGV` exit (signal=Some(11)) is
    /// `ExitReason::Segfault` per the existing precedence at
    /// `classify_exit` line ~68 — it short-circuits BEFORE the OOM
    /// gate. Even with a matching dmesg OOM line, the segfault
    /// classification wins. Pinned so a future refactor that
    /// reorders the checks doesn't silently regress.
    #[test]
    fn sigsegv_short_circuits_before_oom_classification() {
        let ctx = ExitContext {
            dmesg_lines: vec![
                "[12345.678] Out of memory: Killed process 1234 (...) total-vm:9000".into(),
            ],
            ..ExitContext::default()
        };
        let r = classify_exit(&summary(Some(11), None), &ctx);
        assert_eq!(
            r,
            ExitReason::Segfault,
            "SIGSEGV (11) short-circuits to Segfault before the OOM \
             gate; got: {r:?}",
        );
    }

    /// signal=None WITHOUT a matching dmesg line falls through to
    /// `ExitReason::Unknown` — no false positives without dmesg
    /// evidence. The relaxed gate only opens the OOM CLASSIFIER
    /// path; the dmesg match is still required.
    #[test]
    fn signal_none_with_no_dmesg_evidence_remains_unknown() {
        let r = classify_exit(&summary(None, None), &ExitContext::default());
        assert_eq!(
            r,
            ExitReason::Unknown,
            "signal=None + no dmesg → Unknown; the relaxed gate \
             must NOT fabricate an OOM without evidence. Got: {r:?}",
        );
    }

    /// signal=None with a dmesg OOM line for a DIFFERENT PID does
    /// NOT misattribute. The `dmesg_killed_pid` AND-clause is the
    /// primary specificity guard and survives the gate relaxation.
    #[test]
    fn signal_none_with_unrelated_dmesg_pid_does_not_misattribute() {
        let ctx = ExitContext {
            dmesg_lines: vec![
                "[12345.678] Out of memory: Killed process 999 (cron) total-vm:1000".into(),
            ],
            ..ExitContext::default()
        };
        // Our default PID is 1234; dmesg names 999.
        let r = classify_exit(&summary(None, None), &ctx);
        assert_eq!(
            r,
            ExitReason::Unknown,
            "signal=None + dmesg-OOM for a different PID → Unknown; \
             got: {r:?}",
        );
    }
}
