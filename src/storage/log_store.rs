//! `LogStore` — append-only JSONL store for `LifecycleSummary` records.
//!
//! Deliberately separate from `governor::audit`:
//!   • audit log is the governor's decision trail (who got killed, by whom,
//!     why) — short lines, aggressive flushing, security-sensitive.
//!   • log store is the run-summary archive — one record per process that
//!     finished, written for post-hoc analysis. Same wire format, different
//!     retention policy, different on-disk file.
//!
//! Kept tiny on purpose. Rotation, retention, and compression are external
//! concerns (logrotate, systemd-journald, cron).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;

use crate::lifecycle::LifecycleSummary;

#[derive(Debug, Error)]
pub enum LogStoreError {
    #[error("opening log store {path:?}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing to log store: {0}")]
    Write(#[from] std::io::Error),
    #[error("serialising run summary: {0}")]
    Serde(#[from] serde_json::Error),
}

pub struct LogStore {
    path: PathBuf,
    inner: Mutex<BufWriter<File>>,
}

impl LogStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, LogStoreError> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| LogStoreError::Open {
                path: path.clone(),
                source: e,
            })?;
        Ok(Self {
            path,
            inner: Mutex::new(BufWriter::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one summary. Flushes so log tailers / crash recovery see
    /// records the moment the process that generated them exits.
    pub fn append(&self, summary: &LifecycleSummary) -> Result<(), LogStoreError> {
        let line = serde_json::to_string(summary)?;
        let mut guard = self.inner.lock().expect("log store mutex poisoned");
        guard.write_all(line.as_bytes())?;
        guard.write_all(b"\n")?;
        guard.flush()?;
        Ok(())
    }

    /// Round-trip helper for tests and UI history loading. Malformed lines
    /// are logged and skipped rather than aborting the entire replay — a
    /// torn tail from a crash is the most common cause of corruption and
    /// shouldn't prevent reading the healthy prefix.
    pub fn read_all(path: &Path) -> Result<Vec<LifecycleSummary>, LogStoreError> {
        let file = File::open(path).map_err(|e| LogStoreError::Open {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mut out = Vec::new();
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LifecycleSummary>(&line) {
                Ok(summary) => out.push(summary),
                Err(e) => tracing::warn!(
                    line = i + 1,
                    error = %e,
                    "skipping malformed run-summary line"
                ),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{ProcessLifecycle, ResourceStats};
    use crate::model::AICategory;
    use chrono::Utc;

    fn summary_fixture(pid: u32) -> LifecycleSummary {
        LifecycleSummary {
            pid,
            name: format!("proc{pid}"),
            category: Some(AICategory::Inference),
            model_name: Some("qwen2.5-0.5b-instruct-q8_0".into()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 42,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: 12.5,
            peak_cpu_pct: 80.1,
            peak_rss_mb: 512,
            peak_vram_mb: 2048,
            samples: 42,
        }
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summaries.jsonl");
        {
            let store = LogStore::open(&path).unwrap();
            store.append(&summary_fixture(1)).unwrap();
            store.append(&summary_fixture(2)).unwrap();
        }
        let replayed = LogStore::read_all(&path).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].pid, 1);
        assert_eq!(replayed[1].pid, 2);
        assert_eq!(
            replayed[0].model_name.as_deref(),
            Some("qwen2.5-0.5b-instruct-q8_0")
        );
        assert!((replayed[0].avg_cpu_pct - 12.5).abs() < 1e-6);
        assert_eq!(replayed[0].peak_rss_mb, 512);
    }

    #[test]
    fn round_trip_from_lifecycle_integration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summaries.jsonl");

        let mut lc = ProcessLifecycle {
            pid: 77,
            name: "python3".into(),
            spawn_time: Utc::now(),
            exit_time: None,
            exit_code: None,
            signal: None,
            category: Some(AICategory::Inference),
            model_name: Some("yolov8n".into()),
            resources: ResourceStats {
                cpu_sum_pct: 50.0,
                cpu_peak_pct: 30.0,
                rss_peak_bytes: 256 * 1024 * 1024,
                vram_peak_bytes: 512 * 1024 * 1024,
                sample_count: 5,
            },
        };
        lc.mark_exit(Some(0), None);
        let summary = LifecycleSummary::from_lifecycle(&lc).unwrap();

        let store = LogStore::open(&path).unwrap();
        store.append(&summary).unwrap();
        drop(store);

        let replayed = LogStore::read_all(&path).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].peak_rss_mb, 256);
        assert_eq!(replayed[0].peak_vram_mb, 512);
        assert_eq!(replayed[0].samples, 5);
    }

    #[test]
    fn append_does_not_truncate_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summaries.jsonl");
        {
            let s = LogStore::open(&path).unwrap();
            s.append(&summary_fixture(1)).unwrap();
        }
        {
            let s = LogStore::open(&path).unwrap();
            s.append(&summary_fixture(2)).unwrap();
        }
        let replayed = LogStore::read_all(&path).unwrap();
        assert_eq!(replayed.len(), 2);
    }
}
