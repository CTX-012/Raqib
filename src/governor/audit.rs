//! Persistent audit-log writer.
//!
//! The in-memory `AuditLog` in `manual.rs` keeps entries for the TUI's audit
//! panel. Operators running in enforce mode also need a durable trail: a
//! newline-delimited JSON (JSONL) file that survives crashes, can be tail-f'd
//! during incidents, and replayed into a later session to reconstruct every
//! decision the governor made.
//!
//! Append-only on purpose — we never rewrite or rotate in-process. External
//! tooling (logrotate, journald) handles retention.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;

use crate::governor::manual::AuditLogEntry;

#[derive(Debug, Error)]
pub enum AuditWriterError {
    #[error("opening audit file {path:?}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing to audit file: {0}")]
    Write(#[from] std::io::Error),
    #[error("serialising audit entry: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Thread-safe append-only JSONL sink. Instantiated at startup and shared
/// between the manual-kill path and the automated-governor audit path.
pub struct AuditWriter {
    path: PathBuf,
    inner: Mutex<BufWriter<File>>,
}

impl AuditWriter {
    /// Opens the file in append mode, creating it if absent. Returns an
    /// error rather than falling back to /dev/null — a silently-missing
    /// audit trail is worse than a startup crash.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AuditWriterError> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| AuditWriterError::Open {
                path: path.clone(),
                source: e,
            })?;
        Ok(Self {
            path,
            inner: Mutex::new(BufWriter::new(file)),
        })
    }

    /// Appends one entry. Flushes immediately so an operator tailing the
    /// file sees events in real time rather than lagging a buffer.
    pub fn append(&self, entry: &AuditLogEntry) -> Result<(), AuditWriterError> {
        self.append_serializable(entry)
    }

    /// Generic over any `Serialize` type so the same writer can back both
    /// audit entries and run summaries. JSONL invariant: one line per record,
    /// no embedded newlines (serde_json never emits them in non-pretty mode).
    pub fn append_serializable<T: serde::Serialize>(
        &self,
        entry: &T,
    ) -> Result<(), AuditWriterError> {
        let line = serde_json::to_string(entry)?;
        // ok: expect — mutex poisoning means another thread panicked while
        // holding the lock. Audit writes are critical (CLAUDE.md safety
        // rule 6); refusing to continue with a corrupted writer is the
        // safer behaviour than silently dropping subsequent kill records.
        let mut guard = self.inner.lock().expect("audit writer mutex poisoned");
        guard.write_all(line.as_bytes())?;
        guard.write_all(b"\n")?;
        guard.flush()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Replays a JSONL audit file into a Vec, skipping malformed lines with a
/// warn-level log so a torn tail from a crash doesn't block replay.
pub fn replay(path: &Path) -> Result<Vec<AuditLogEntry>, AuditWriterError> {
    use std::io::BufRead;
    let file = File::open(path).map_err(|e| AuditWriterError::Open {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut out = Vec::new();
    for (i, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditLogEntry>(&line) {
            Ok(entry) => out.push(entry),
            Err(e) => tracing::warn!(
                line = i + 1,
                error = %e,
                "skipping malformed audit line"
            ),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governor::manual::{KillSource, ManualKillAction};
    use crate::model::AICategory;
    use chrono::Utc;

    fn entry(pid: u32) -> AuditLogEntry {
        AuditLogEntry {
            timestamp: Utc::now(),
            action: ManualKillAction::SendSigterm,
            source: KillSource::Automated,
            pid,
            process_name: format!("proc{pid}"),
            category: Some(AICategory::Inference),
            reason: "test".to_string(),
            success: true,
            error_msg: None,
        }
    }

    #[test]
    fn round_trip_single_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let writer = AuditWriter::open(&path).unwrap();
        writer.append(&entry(42)).unwrap();
        drop(writer);

        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].pid, 42);
        assert_eq!(replayed[0].process_name, "proc42");
    }

    #[test]
    fn append_preserves_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        {
            let w = AuditWriter::open(&path).unwrap();
            w.append(&entry(1)).unwrap();
        }
        {
            let w = AuditWriter::open(&path).unwrap();
            w.append(&entry(2)).unwrap();
        }
        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].pid, 1);
        assert_eq!(replayed[1].pid, 2);
    }

    #[test]
    fn replay_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let writer = AuditWriter::open(&path).unwrap();
        writer.append(&entry(1)).unwrap();
        // Inject a torn line by opening the file directly.
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "{{not valid json").unwrap();
        }
        writer.append(&entry(2)).unwrap();

        let replayed = replay(&path).unwrap();
        assert_eq!(replayed.len(), 2, "malformed line is skipped, not fatal");
    }
}
