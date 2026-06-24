//! Tier 1.1 — integration test for the `edge_monitor history` CLI path.
//!
//! Runs the library entry point against a tempdir-backed RunStore so we
//! exercise:
//!  * config → RunStore::open path resolution
//!  * RunStore → recent() ordering (newest first)
//!  * history rendering (text + JSON shape) end-to-end
//!
//! Doesn't shell out to the binary — that's clap's job, covered by unit
//! tests in `main.rs`'s clap derive. This file owns the data path.

use chrono::Utc;
use edge_monitor::config::Config;
use edge_monitor::history::{ModelSummary, run_history_to};
use edge_monitor::lifecycle::LifecycleSummary;
use edge_monitor::model::AICategory;
use edge_monitor::storage::run_store::{RunRecord, RunStore};

fn fake_summary(pid: u32, model: &str, exit_code: Option<i32>) -> LifecycleSummary {
    LifecycleSummary {
        pid,
        name: "python".into(),
        category: Some(AICategory::Inference),
        model_name: Some(model.into()),
        spawn_time: Utc::now(),
        exit_time: Utc::now(),
        uptime_secs: 7,
        exit_code,
        signal: if exit_code.is_some() { None } else { Some(15) },
        avg_cpu_pct: 50.0,
        peak_cpu_pct: 75.0,
        peak_rss_mb: 128,
        peak_vram_mb: 0,
        samples: 7,
        trajectory: None,
    }
}

fn config_for(root: &std::path::Path) -> Config {
    let mut cfg = Config::default();
    cfg.storage.run_store_path = root.to_string_lossy().into_owned();
    cfg
}

#[test]
fn empty_store_prints_no_history_message() {
    let dir = tempfile::tempdir().unwrap();
    // Ensure the store dir exists even though no runs have been appended.
    let _ = RunStore::open(dir.path()).unwrap();
    let cfg = config_for(dir.path());

    let mut buf = Vec::new();
    run_history_to(None, 20, false, &cfg, &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    // DESIGN_HANDOFF Principle 6 — empty state teaches with at
    // least the banner + one launch example.
    assert!(
        out.contains("No run history yet"),
        "expected the banner line; got:\n{out}"
    );
    assert!(
        out.contains("ollama run") || out.contains("vllm serve"),
        "expected at least one launch example; got:\n{out}"
    );
}

#[test]
fn appended_record_shows_up_in_history_text() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RunStore::open(dir.path()).unwrap();
        store
            .append(RunRecord::from_summary(fake_summary(
                1,
                "phi3-mini",
                Some(0),
            )))
            .unwrap();
        store
            .append(RunRecord::from_summary(fake_summary(
                2,
                "phi3-mini",
                Some(0),
            )))
            .unwrap();
    }
    let cfg = config_for(dir.path());

    let mut buf = Vec::new();
    run_history_to(Some("phi3-mini".into()), 20, false, &cfg, &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("phi3-mini"), "missing model name in:\n{out}");
    assert!(out.contains("clean"), "missing exit status in:\n{out}");
    // Header row present.
    assert!(out.contains("Avg CPU"));
}

#[test]
fn appended_record_shows_up_in_model_summary_table() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RunStore::open(dir.path()).unwrap();
        for (model, code) in &[
            ("phi3-mini", Some(0)),
            ("phi3-mini", Some(0)),
            ("yolov8n", None),
        ] {
            store
                .append(RunRecord::from_summary(fake_summary(1, model, *code)))
                .unwrap();
        }
    }
    let cfg = config_for(dir.path());

    let mut buf = Vec::new();
    run_history_to(None, 20, false, &cfg, &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("phi3-mini"));
    assert!(out.contains("yolov8n"));
    // phi3-mini ran twice → "2" in the Runs column.
    assert!(
        out.lines()
            .any(|l| l.contains("phi3-mini") && l.contains(" 2  ")),
        "expected phi3-mini run count = 2 in:\n{out}"
    );
}

#[test]
fn json_output_parses_as_record_array_for_a_model() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RunStore::open(dir.path()).unwrap();
        store
            .append(RunRecord::from_summary(fake_summary(
                1,
                "phi3-mini",
                Some(0),
            )))
            .unwrap();
    }
    let cfg = config_for(dir.path());

    let mut buf = Vec::new();
    run_history_to(Some("phi3-mini".into()), 20, true, &cfg, &mut buf).unwrap();
    let parsed: Vec<RunRecord> = serde_json::from_slice(&buf).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].summary.model_name.as_deref(), Some("phi3-mini"));
}

#[test]
fn json_output_parses_as_summary_array_when_no_model() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RunStore::open(dir.path()).unwrap();
        store
            .append(RunRecord::from_summary(fake_summary(
                1,
                "phi3-mini",
                Some(0),
            )))
            .unwrap();
        store
            .append(RunRecord::from_summary(fake_summary(2, "yolov8n", Some(0))))
            .unwrap();
    }
    let cfg = config_for(dir.path());

    let mut buf = Vec::new();
    run_history_to(None, 20, true, &cfg, &mut buf).unwrap();
    let parsed: Vec<ModelSummary> = serde_json::from_slice(&buf).unwrap();
    assert_eq!(parsed.len(), 2);
    let names: Vec<_> = parsed.iter().map(|s| s.model.clone()).collect();
    assert!(names.contains(&"phi3-mini".to_string()));
    assert!(names.contains(&"yolov8n".to_string()));
}
