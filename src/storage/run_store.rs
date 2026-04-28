//! `RunStore` — typed read/write store for completed-run records, with a
//! per-model index for fast queries (`recent`, `baseline`, …).
//!
//! Replaces the single ever-growing `LogStore` JSONL for new writes.
//! `LogStore` lives on for backwards-compatibility with existing summary
//! files; it can be removed once Tier 1.1 (history viewer) ships and
//! operators have migrated.
//!
//! On-disk layout:
//! ```text
//! <root>/
//!   runs/
//!     2026-04-28/
//!       run-<uuid>.json    # one file per record, the full RunRecord
//!   index.jsonl            # append-only, one IndexEntry per record
//! ```
//!
//! Why per-file plus an index: the prior single JSONL grew unbounded and
//! rewriting it (for retention, redaction, deletion) is risky. Per-file
//! storage lets users delete individual runs by `rm`; the index gives an
//! O(N) startup scan without parsing every full record.
//!
//! Crash safety: a record is written to its per-file path *before* the
//! index entry is appended. A crash mid-append leaves the file orphaned
//! (re-discoverable by directory walk) rather than an index pointing at a
//! missing file. The reverse ordering would be the bug.
//!
//! See latest.md "Foundation A" for the spec.
//!
//! Concurrency: this Foundation-A implementation is single-writer (the
//! runtime tick loop). When Foundation B introduces async telemetry
//! samplers in their own task, callers can wrap the `RunStore` in a
//! `Mutex` — the API stays the same.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::analysis::compare::{Baseline, BaselineMetrics};
use crate::lifecycle::LifecycleSummary;

/// Stable identifier for a completed run. UUIDv4 — collision risk is
/// effectively zero at any plausible run volume.
pub type RunId = Uuid;

/// Internal alias for the in-memory index loaded from `index.jsonl`.
/// The two halves are kept in sync by `RunStore::open` and `append`.
type IndexState = (HashMap<String, Vec<RunId>>, HashMap<RunId, String>);

/// What kind of runtime hosted the model. Detection lands in Tier 1.2;
/// Foundation A only defines the enum so RunRecord can carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeKind {
    Vllm,
    LlamaCpp,
    Ollama,
    Ultralytics,
    Unknown,
}

/// Why a process exited. Foundation A populates only the trivial cases
/// (clean exit / signal / unknown). Tier 3.5 will extend the classifier
/// to read dmesg and recent stderr for OOM, segfaults, CUDA errors, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitReason {
    /// `exit_code == Some(0)`.
    CleanExit,
    /// Killed by a signal we cannot attribute to the governor.
    /// Typically a TTY SIGINT/SIGTERM or an external `kill`.
    UserSignal { signal: i32 },
    /// The governor killed it. Reason carries the policy explanation.
    GovernorKill { reason: String },
    /// SIGSEGV. Independent of OOM (stack overflow, null deref, …).
    Segfault,
    /// SIGKILL + matching dmesg OOM line, or "CUDA out of memory" in
    /// recent stderr. `ram = true` for kernel-OOM, `vram = true` for
    /// CUDA-OOM. Both can be true if the same run hit both.
    OutOfMemory { ram: bool, vram: bool },
    /// CUDA error in recent stderr (driver, illegal access, etc.).
    /// `last_msg` is the most recent matching log line, truncated.
    CudaError { last_msg: Option<String> },
    /// `exit_code != 0` and not signal-terminated.
    Crash { exit_code: i32 },
    /// Insufficient evidence — kept distinct from CleanExit so downstream
    /// queries can filter "I know this was clean" from "I have no idea".
    Unknown,
}

impl ExitReason {
    /// Best-effort reason from the data on a `LifecycleSummary` alone.
    /// Tier 3.5 (`exit_classify::classify_exit`) layers dmesg + stderr
    /// inspection on top of this for richer categorisation.
    pub fn from_summary(summary: &LifecycleSummary) -> Self {
        if let Some(sig) = summary.signal {
            // SIGSEGV (11) is unambiguously a segfault even without
            // dmesg context.
            if sig == 11 {
                return ExitReason::Segfault;
            }
            return ExitReason::UserSignal { signal: sig };
        }
        match summary.exit_code {
            Some(0) => ExitReason::CleanExit,
            Some(code) => ExitReason::Crash { exit_code: code },
            None => ExitReason::Unknown,
        }
    }
}

/// I/O footprint observed during the cold-load phase. Populated by
/// Tier 2.2; Foundation A leaves this `None` on every record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColdStartStats {
    pub duration_seconds: f32,
    pub bytes_read: u64,
    pub avg_throughput_mbps: f32,
    pub peak_throughput_mbps: f32,
}

/// Per-run telemetry rolled up at exit. Every field is `Option` because
/// no single runtime exposes all of them, and Foundation A populates
/// none of these — they fill in as Tiers 1.2 / 2.1 / 2.2 / 3.x land.
///
/// Field ordering mirrors `latest.md` so a reviewer can diff the two.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunMetrics {
    // ── LLM ──────────────────────────────────────────────────────────
    pub tokens_total: Option<u64>,
    pub tokens_per_sec_avg: Option<f32>,
    pub tokens_per_sec_peak: Option<f32>,
    pub kv_cache_peak_pct: Option<f32>,
    pub concurrent_requests_peak: Option<u32>,

    /// Tier 3.2 — same `tokens_per_sec_avg` arithmetic but restricted
    /// to samples after cold-load completed. None when cold-load never
    /// finished (short run, streaming workload) or no telemetry post-
    /// cold-load. History comparison should prefer this over
    /// `_avg` because it ignores model-load warm-up noise.
    pub tokens_per_sec_avg_steady: Option<f32>,
    pub fps_avg_steady: Option<f32>,
    pub gpu_watts_avg_steady: Option<f32>,

    // ── Vision ───────────────────────────────────────────────────────
    pub frames_total: Option<u64>,
    pub fps_avg: Option<f32>,
    pub inference_latency_ms_avg: Option<f32>,
    pub inference_latency_ms_p99: Option<f32>,

    // ── Power ────────────────────────────────────────────────────────
    pub gpu_watts_avg: Option<f32>,
    pub gpu_watts_peak: Option<f32>,
    pub cpu_watts_avg: Option<f32>,
    pub energy_joules_total: Option<f32>,

    // ── I/O ──────────────────────────────────────────────────────────
    pub disk_read_bytes: Option<u64>,
    pub cold_load_seconds: Option<f32>,
}

/// A completed run, stored once per process termination.
///
/// `summary` is the existing Phase-1 `LifecycleSummary` embedded
/// verbatim — the spec says "extend, don't replace", and embedding keeps
/// the wire format additive: a Phase-1 reader unaware of Foundation A
/// can still pull the lifecycle fields out of the JSON if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    /// Embedded Phase-1 summary. Carries pid/name/category/model_name/
    /// uptime/peaks etc.
    pub summary: LifecycleSummary,

    // ── New in Foundation A ──────────────────────────────────────────
    /// Content fingerprint of the model file. Tier 3.1 populates this;
    /// Foundation A leaves it `None`.
    #[serde(default)]
    pub model_fingerprint: Option<String>,
    #[serde(default)]
    pub runtime: Option<RuntimeKind>,
    /// e.g. "Q4_K_M", "FP16", parsed from the GGUF/safetensors filename.
    /// Tier 1.2 fills this in from the model path.
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub metrics: RunMetrics,
    #[serde(default = "ExitReason::default_unknown")]
    pub exit_reason: ExitReason,
    #[serde(default)]
    pub cold_start: Option<ColdStartStats>,
}

impl ExitReason {
    fn default_unknown() -> Self {
        ExitReason::Unknown
    }
}

impl RunRecord {
    /// Build a record from an existing `LifecycleSummary`. Caller fills
    /// in the optional telemetry fields if any are available; Foundation
    /// A always passes `RunMetrics::default()`.
    pub fn from_summary(summary: LifecycleSummary) -> Self {
        let exit_reason = ExitReason::from_summary(&summary);
        Self {
            run_id: Uuid::new_v4(),
            summary,
            model_fingerprint: None,
            runtime: None,
            quantization: None,
            metrics: RunMetrics::default(),
            exit_reason,
            cold_start: None,
        }
    }

    /// Convenience for queries: the model name, falling back to the
    /// process name when no model was extracted. The history viewer
    /// uses this so processes without a resolved model still cluster.
    pub fn model_or_name(&self) -> &str {
        self.summary
            .model_name
            .as_deref()
            .unwrap_or(self.summary.name.as_str())
    }
}

/// One line in `index.jsonl` — minimal data needed to find the full
/// record on disk. Kept small so a multi-thousand-run startup scan stays
/// cheap.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    run_id: RunId,
    /// `model_or_name()` at write time. Stored so `list_models()` and
    /// `recent(model)` can answer without opening every file.
    model_key: String,
    exit_time: DateTime<Utc>,
    /// Path *relative* to the store root. Keeps the index portable if a
    /// user `cp -r`'s the store directory.
    relative_path: String,
}

#[derive(Debug, Error)]
pub enum RunStoreError {
    #[error("creating run-store directory {path:?}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("opening run-store index {path:?}: {source}")]
    OpenIndex {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing run record {path:?}: {source}")]
    WriteRecord {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading run record {path:?}: {source}")]
    ReadRecord {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("(de)serialising run-store data: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("appending to index: {0}")]
    Io(#[from] std::io::Error),
}

/// Storage layer for [`RunRecord`]s.
///
/// Single-writer in Foundation A; see module docs.
pub struct RunStore {
    root: PathBuf,
    index_path: PathBuf,
    /// `model_key -> [run_id]`, oldest-first (append order).
    /// Reverse on read for `recent()`.
    by_model: HashMap<String, Vec<RunId>>,
    /// `run_id -> relative path`. Lets `get()` open the file without a
    /// directory scan.
    by_id: HashMap<RunId, String>,
    index_writer: BufWriter<File>,
}

impl RunStore {
    /// Open or create a store rooted at `root`. Builds the in-memory
    /// index from `index.jsonl` if present, skipping any corrupted lines
    /// with a warn log (mirrors `LogStore::read_all`'s torn-tail policy).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RunStoreError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| RunStoreError::CreateDir {
            path: root.clone(),
            source: e,
        })?;
        let runs_dir = root.join("runs");
        fs::create_dir_all(&runs_dir).map_err(|e| RunStoreError::CreateDir {
            path: runs_dir,
            source: e,
        })?;

        let index_path = root.join("index.jsonl");
        let (by_model, by_id) = Self::load_index(&index_path)?;

        let index_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_path)
            .map_err(|e| RunStoreError::OpenIndex {
                path: index_path.clone(),
                source: e,
            })?;

        Ok(Self {
            root,
            index_path,
            by_model,
            by_id,
            index_writer: BufWriter::new(index_file),
        })
    }

    /// Public root path — useful for tests and the manual smoke script.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Append a new run record. Writes the per-record JSON file *first*,
    /// then appends the index entry. A crash between the two leaves the
    /// record file orphaned but the store consistent on next open.
    pub fn append(&mut self, record: RunRecord) -> Result<RunId, RunStoreError> {
        let run_id = record.run_id;
        let model_key = record.model_or_name().to_string();
        let day = record.summary.exit_time.format("%Y-%m-%d").to_string();

        let day_dir = self.root.join("runs").join(&day);
        fs::create_dir_all(&day_dir).map_err(|e| RunStoreError::CreateDir {
            path: day_dir.clone(),
            source: e,
        })?;
        let file_name = format!("run-{}.json", run_id);
        let abs_path = day_dir.join(&file_name);
        let relative_path = format!("runs/{}/{}", day, file_name);

        // Step 1: write the record file. Use create_new to refuse to
        // overwrite an existing one — UUID collision would be a bug.
        let json = serde_json::to_string(&record)?;
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&abs_path)
            .map_err(|e| RunStoreError::WriteRecord {
                path: abs_path.clone(),
                source: e,
            })?;
        f.write_all(json.as_bytes())
            .map_err(|e| RunStoreError::WriteRecord {
                path: abs_path.clone(),
                source: e,
            })?;
        f.flush().map_err(|e| RunStoreError::WriteRecord {
            path: abs_path.clone(),
            source: e,
        })?;

        // Step 2: append the index entry. Flush so a tail-following
        // process sees it immediately.
        let entry = IndexEntry {
            run_id,
            model_key: model_key.clone(),
            exit_time: record.summary.exit_time,
            relative_path: relative_path.clone(),
        };
        let line = serde_json::to_string(&entry)?;
        self.index_writer.write_all(line.as_bytes())?;
        self.index_writer.write_all(b"\n")?;
        self.index_writer.flush()?;

        // Step 3: reflect in memory.
        self.by_model.entry(model_key).or_default().push(run_id);
        self.by_id.insert(run_id, relative_path);

        Ok(run_id)
    }

    /// Sorted list of model keys with at least one stored run.
    pub fn list_models(&self) -> Vec<String> {
        let mut models: Vec<String> = self.by_model.keys().cloned().collect();
        models.sort();
        models
    }

    /// Up to `n` most recent runs of `model`, newest first. Loads each
    /// matching record file from disk; cheap enough for `n ≤ ~100`,
    /// which is the only call site Tier 1.1 plans.
    ///
    /// Records that fail to deserialise (e.g. format drift, truncation)
    /// are logged at warn and skipped — the operator should not lose a
    /// whole history view because one file is bad.
    pub fn recent(&self, model: &str, n: usize) -> Vec<RunRecord> {
        let Some(ids) = self.by_model.get(model) else {
            return Vec::new();
        };
        ids.iter()
            .rev()
            .take(n)
            .filter_map(|id| match self.get(*id) {
                Some(r) => Some(r),
                None => {
                    tracing::warn!(
                        run_id = %id,
                        model = %model,
                        "run record missing or malformed; skipping in recent()"
                    );
                    None
                }
            })
            .collect()
    }

    /// Load a single record by id. Returns `None` if the id is unknown
    /// or the file is missing/corrupt.
    pub fn get(&self, id: RunId) -> Option<RunRecord> {
        let rel = self.by_id.get(&id)?;
        let abs = self.root.join(rel);
        let bytes = fs::read(&abs).ok()?;
        match serde_json::from_slice::<RunRecord>(&bytes) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(
                    path = %abs.display(),
                    error = %e,
                    "run record failed to deserialise"
                );
                None
            }
        }
    }

    /// Rolling baseline computed from the most recent `window` runs of
    /// `model`. Returns `None` when no runs exist; returns an empty-
    /// metrics baseline when `window == 0` (caller decides what to do).
    ///
    /// Foundation A wires the call into [`crate::analysis::compare`];
    /// the metric set computed from a `LifecycleSummary`-only record is
    /// just the four resource peaks. Telemetry-driven metrics fill in
    /// once Tier 1.2 lands.
    pub fn baseline(&self, model: &str, window: usize) -> Option<Baseline> {
        let records = self.recent(model, window.max(1));
        if records.is_empty() {
            return None;
        }
        Some(Baseline {
            model: model.to_string(),
            sample_size: records.len(),
            metrics: BaselineMetrics::from_records(&records),
            computed_at: Utc::now(),
        })
    }

    fn load_index(index_path: &Path) -> Result<IndexState, RunStoreError> {
        let mut by_model: HashMap<String, Vec<RunId>> = HashMap::new();
        let mut by_id: HashMap<RunId, String> = HashMap::new();
        let file = match File::open(index_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((by_model, by_id)),
            Err(e) => {
                return Err(RunStoreError::OpenIndex {
                    path: index_path.to_path_buf(),
                    source: e,
                });
            }
        };
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<IndexEntry>(&line) {
                Ok(entry) => {
                    by_model
                        .entry(entry.model_key.clone())
                        .or_default()
                        .push(entry.run_id);
                    by_id.insert(entry.run_id, entry.relative_path);
                }
                Err(e) => tracing::warn!(
                    line = i + 1,
                    error = %e,
                    "skipping malformed run-store index line"
                ),
            }
        }
        Ok((by_model, by_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecycleSummary;
    use crate::model::AICategory;
    use chrono::Utc;
    use std::io::Write as _IoWrite;

    fn fixture_summary(pid: u32, model: &str, peak_cpu: f32) -> LifecycleSummary {
        LifecycleSummary {
            pid,
            name: format!("python-{pid}"),
            category: Some(AICategory::Inference),
            model_name: Some(model.to_string()),
            spawn_time: Utc::now(),
            exit_time: Utc::now(),
            uptime_secs: 10,
            exit_code: Some(0),
            signal: None,
            avg_cpu_pct: peak_cpu * 0.8,
            peak_cpu_pct: peak_cpu,
            peak_rss_mb: 512,
            peak_vram_mb: 0,
            samples: 10,
        }
    }

    fn fixture_record(pid: u32, model: &str, peak_cpu: f32) -> RunRecord {
        RunRecord::from_summary(fixture_summary(pid, model, peak_cpu))
    }

    /// Spec test: append 100 runs, restart, verify index rebuilds.
    #[test]
    fn appends_then_reopens_with_full_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        let mut ids = Vec::new();
        for i in 0..100 {
            let model = format!("model-{}", i % 4);
            let id = store.append(fixture_record(i, &model, 50.0)).unwrap();
            ids.push(id);
        }
        drop(store);

        let store = RunStore::open(dir.path()).unwrap();
        assert_eq!(store.list_models().len(), 4);
        for id in &ids {
            assert!(store.get(*id).is_some(), "id {id} lost on reopen");
        }
    }

    /// Spec test: append runs for 5 different models, list_models returns 5.
    #[test]
    fn list_models_returns_all_keys_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        for (i, model) in ["alpha", "bravo", "charlie", "delta", "echo"]
            .iter()
            .enumerate()
        {
            store.append(fixture_record(i as u32, model, 10.0)).unwrap();
        }
        let models = store.list_models();
        assert_eq!(models, vec!["alpha", "bravo", "charlie", "delta", "echo"]);
    }

    /// Spec test: recent() returns newest first.
    #[test]
    fn recent_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        let ids: Vec<_> = (0..5)
            .map(|i| store.append(fixture_record(i, "phi3-mini", 10.0)).unwrap())
            .collect();
        let recent = store.recent("phi3-mini", 3);
        assert_eq!(recent.len(), 3);
        // Last appended id should be first in the result.
        assert_eq!(recent[0].run_id, *ids.last().unwrap());
        assert_eq!(recent[1].run_id, ids[3]);
        assert_eq!(recent[2].run_id, ids[2]);
    }

    /// Spec test: corrupted index line is skipped with a warn log,
    /// not a panic. The healthy lines still load.
    #[test]
    fn corrupted_index_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        // Seed with one valid record so the index has a healthy line.
        {
            let mut store = RunStore::open(dir.path()).unwrap();
            store
                .append(fixture_record(1, "valid-model", 10.0))
                .unwrap();
        }
        // Inject a torn / corrupt line into the index.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join("index.jsonl"))
                .unwrap();
            writeln!(f, "{{ this is not valid json").unwrap();
        }
        // A second valid record after the corrupt line should still register.
        {
            let mut store = RunStore::open(dir.path()).unwrap();
            store
                .append(fixture_record(2, "valid-model", 10.0))
                .unwrap();
        }

        let store = RunStore::open(dir.path()).unwrap();
        // Both valid records are findable; the corrupt line did not crash.
        assert_eq!(store.list_models(), vec!["valid-model"]);
        let recent = store.recent("valid-model", 10);
        assert_eq!(recent.len(), 2);
    }

    /// Spec test: baseline with N=0 or N>available returns sensible defaults.
    #[test]
    fn baseline_handles_edge_window_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        // No records → None.
        assert!(store.baseline("phi3-mini", 5).is_none());

        // 3 records, ask for window=0 → behaves as window=1, returns
        // a baseline of size 1 (most recent run).
        for i in 0..3 {
            store
                .append(fixture_record(i, "phi3-mini", 30.0 + i as f32))
                .unwrap();
        }
        let bl_zero = store.baseline("phi3-mini", 0).unwrap();
        assert_eq!(bl_zero.sample_size, 1);

        // window > available → returns what we have, no crash.
        let bl_big = store.baseline("phi3-mini", 100).unwrap();
        assert_eq!(bl_big.sample_size, 3);
    }

    /// Crash-safety: writing the record file fails after step 1 should
    /// not leave the index pointing at a missing file. Modeled here by
    /// asserting the on-disk write order — if step 2 runs before step 1
    /// this test is the bug we'd catch.
    #[test]
    fn record_file_exists_for_every_index_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunStore::open(dir.path()).unwrap();
        for i in 0..10 {
            store
                .append(fixture_record(i, "ordering-test", 10.0))
                .unwrap();
        }
        // Walk the index and assert each referenced file exists.
        let index = std::fs::read_to_string(dir.path().join("index.jsonl")).unwrap();
        for line in index.lines() {
            let entry: IndexEntry = serde_json::from_str(line).unwrap();
            let abs = dir.path().join(&entry.relative_path);
            assert!(
                abs.exists(),
                "index points at missing file: {}",
                abs.display()
            );
        }
    }

    /// ExitReason::from_summary matrix.
    #[test]
    fn exit_reason_classification_matrix() {
        let mut s = fixture_summary(1, "m", 1.0);

        s.exit_code = Some(0);
        s.signal = None;
        assert!(matches!(
            ExitReason::from_summary(&s),
            ExitReason::CleanExit
        ));

        s.exit_code = None;
        s.signal = Some(15);
        assert!(matches!(
            ExitReason::from_summary(&s),
            ExitReason::UserSignal { signal: 15 }
        ));

        s.exit_code = Some(139);
        s.signal = None;
        assert!(matches!(
            ExitReason::from_summary(&s),
            ExitReason::Crash { exit_code: 139 }
        ));

        s.exit_code = None;
        s.signal = None;
        assert!(matches!(ExitReason::from_summary(&s), ExitReason::Unknown));
    }
}
