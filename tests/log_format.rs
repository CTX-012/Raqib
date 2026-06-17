//! S.2 — `--log-format json` end-to-end smoke at the binary level.
//!
//! Tracing's global subscriber is a process-global, so we cannot
//! exercise both formats from inside a single test binary. Instead
//! we spawn `edge_monitor` as a subprocess for each format and
//! validate stderr line shapes.
//!
//! Why an integration test rather than a unit test on `init_tracing`:
//! the value of S.2 is that downstream tooling (jq, fluentd, vector)
//! can read every line as JSON. That's an end-to-end claim and the
//! binary is the only thing that can prove it.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_edge_monitor")
}

#[test]
fn json_format_emits_one_json_object_per_stderr_line() {
    // 3 ticks at the default 1 s interval ≈ 3 s. A handful of lines is
    // enough to prove the schema; the manual smoke covers the 100+ case.
    let out = Command::new(binary())
        // --no-web skips the DISPATCH 85 web-auth posture gate
        // (these tests exercise log format, not the web companion).
        .args(["--no-ui", "--no-web", "--ticks", "3", "--log-format", "json"])
        .output()
        .expect("spawn edge_monitor");
    assert!(
        out.status.success(),
        "binary exited non-zero: {} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8(out.stderr).expect("stderr utf-8");
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        lines.len() >= 4,
        "expected ≥4 stderr lines from a 3-tick run, got {}: {:#?}",
        lines.len(),
        lines
    );

    let mut tick_messages = 0usize;
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("line did not parse as JSON: {line}\nerror: {e}");
        });
        let obj = v.as_object().expect("each line must be a JSON object");
        // tracing-subscriber's JSON formatter always emits these keys.
        for required in ["timestamp", "level", "message"] {
            assert!(
                obj.contains_key(required),
                "missing key {required} in: {line}"
            );
        }
        if obj.get("message").and_then(|m| m.as_str()) == Some("tick") {
            // structured fields must flatten onto the root, not nest.
            assert!(
                obj.contains_key("tick"),
                "tick events must expose `tick` at the root"
            );
            assert!(
                obj.contains_key("ai_processes"),
                "tick events must expose `ai_processes` at the root"
            );
            tick_messages += 1;
        }
    }
    assert!(
        tick_messages >= 3,
        "expected at least 3 tick messages in JSON output, got {tick_messages}"
    );
}

#[test]
fn human_format_is_not_json_shaped() {
    let out = Command::new(binary())
        .args(["--no-ui", "--no-web", "--ticks", "1", "--log-format", "human"])
        .output()
        .expect("spawn edge_monitor");
    assert!(out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("stderr utf-8");

    for line in stderr.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A well-formed JSON object must start with `{`. Human format
        // lines start with a timestamp like `2026-…`. If we ever start
        // emitting JSON in human mode, this assertion catches the
        // regression at PR time.
        if line.starts_with('{') {
            // Try to parse — if it's a real object, that's a bug.
            if let Ok(serde_json::Value::Object(_)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                panic!("human-format line parsed as a JSON object: {line}");
            }
        }
    }
}
