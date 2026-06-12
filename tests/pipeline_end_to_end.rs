//! End-to-end integration: classify → lifecycle → governor with real
//! (in-process, no hardware) data. Validates that the four library
//! modules actually compose the way the architecture doc claims.

use std::collections::HashMap;

use edge_monitor::classifier::classify_process;
use edge_monitor::governor::policy::PolicyAction;
use edge_monitor::governor::{GovernorExecutor, GovernorPolicy, KillAction};
use edge_monitor::lifecycle::tracker::LifecycleTracker;
use edge_monitor::model::{AICategory, ProcessSample};

fn sample(pid: u32, name: &str, argv: &[&str]) -> ProcessSample {
    ProcessSample {
        pid,
        ppid: Some(1),
        name: name.into(),
        cmdline: argv.iter().map(|s| s.to_string()).collect(),
        environ: HashMap::new(),
        cwd: None,
        ..Default::default()
    }
}

#[test]
fn ai_process_with_model_path_is_tracked_and_killed_in_enforce_mode() {
    let ai = sample(
        1000,
        "llama-server",
        &["llama-server", "--model", "/models/llama3-8b.gguf"],
    );
    let shell = sample(1001, "bash", &["bash"]);

    let result = classify_process(&ai);
    assert_eq!(result.category, AICategory::Inference);
    assert_eq!(result.model_name.as_deref(), Some("llama3-8b"));

    let shell_result = classify_process(&shell);
    assert_eq!(shell_result.category, AICategory::NotAi);

    let mut tracker = LifecycleTracker::new();
    let snapshot = tracker.update(&[ai, shell]).unwrap();
    assert_eq!(snapshot.active_count(), 2);

    // v1.0.1 B-NEW-1 — `safe_default()` ships with
    // `default_ai_action = Allow`, so an "enforce mode" pipeline
    // test must opt back in to kills explicitly. The point of this
    // test is the AI→Kill pathway, so the opt-in is exactly what
    // the scenario covers.
    let mut policy = GovernorPolicy::safe_default();
    policy.default_ai_action = PolicyAction::Kill;
    let mut governor = GovernorExecutor::new(policy);
    // v1.3.2 / DISPATCH 78 — the AI process is the one we want to
    // kill; pass a breach for its PID so the new
    // policy-Kill-needs-breach gate is satisfied. The shell isn't
    // an AI process and policy will allow it regardless.
    let breaches = vec![edge_monitor::governor::threshold_breach::ThresholdBreach {
        pid: 1000,
        vram_pct: Some(99.0),
        vram_breached: true,
    }];
    let decisions = governor.evaluate(&snapshot, &breaches);

    let ai_action = decisions
        .iter()
        .find(|(pid, _, _)| *pid == 1000)
        .map(|(_, a, _)| *a)
        .expect("AI process must appear in decisions");
    let shell_action = decisions
        .iter()
        .find(|(pid, _, _)| *pid == 1001)
        .map(|(_, a, _)| *a)
        .expect("shell must appear in decisions");

    assert_eq!(
        ai_action,
        KillAction::SignalTermSent,
        "enforced governor kills AI process"
    );
    assert_eq!(
        shell_action,
        KillAction::Whitelisted,
        "allowlisted shell survives even in enforce mode"
    );
}

#[test]
fn exited_ai_process_generates_summary_with_resource_stats() {
    let mut tracker = LifecycleTracker::new();
    tracker
        .update(&[sample(
            2000,
            "python3",
            &["python3", "-m", "vllm.entrypoints.openai.api_server"],
        )])
        .unwrap();

    // Fold in two ticks of synthetic resource readings.
    tracker.record_sample(2000, 15.0, 300 * 1024 * 1024, Some(1024 * 1024 * 1024));
    tracker.record_sample(2000, 60.0, 500 * 1024 * 1024, Some(2048 * 1024 * 1024));
    tracker.record_model_name(2000, Some("qwen2.5-0.5b-instruct-q8_0".into()));

    let snapshot = tracker.update(&[]).unwrap();
    assert_eq!(snapshot.recent_exits.len(), 1);
    let summary = &snapshot.recent_exits[0];

    assert_eq!(summary.pid, 2000);
    assert_eq!(summary.category, Some(AICategory::Inference));
    assert_eq!(
        summary.model_name.as_deref(),
        Some("qwen2.5-0.5b-instruct-q8_0")
    );
    assert_eq!(summary.samples, 2);
    assert!((summary.peak_cpu_pct - 60.0).abs() < 1e-6);
    assert_eq!(summary.peak_rss_mb, 500);
    assert_eq!(summary.peak_vram_mb, 2048);
}

#[test]
fn persistent_summary_round_trips_through_log_store() {
    use edge_monitor::storage::LogStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("summaries.jsonl");

    let mut tracker = LifecycleTracker::new();
    tracker
        .update(&[sample(3000, "python3", &["python3", "train.py"])])
        .unwrap();
    tracker.record_sample(3000, 99.0, 1024 * 1024 * 1024, Some(4096 * 1024 * 1024));
    tracker.record_model_name(3000, Some("yolov8n".into()));

    let snapshot = tracker.update(&[]).unwrap();
    let summary = snapshot.recent_exits.first().unwrap().clone();

    {
        let store = LogStore::open(&path).unwrap();
        store.append(&summary).unwrap();
    }

    let replayed = LogStore::read_all(&path).unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].model_name.as_deref(), Some("yolov8n"));
    assert_eq!(replayed[0].peak_vram_mb, 4096);
}
