//! `raqib config check` — the pre-arm verification gate.
//!
//! Spawns the built binary and asserts the three exit-code paths
//! nginx -t / sshd -t / visudo -c set the expectations for:
//!
//!   * exit 0 — valid config, pretty-print includes `[policy]`
//!     allowlist + blocklist NAMES (the thing no wire endpoint
//!     surfaces) and ends with `VALIDATION: OK`.
//!   * exit 1 — no config file found in any search location.
//!   * exit 2 — file found but parse / validate / threshold-resolve
//!     failed. Error goes to stderr; the operator's shell can
//!     `raqib config check --path X || exit 1` scriptably.
//!
//! Load-bearing pin: the printed allowlist + blocklist ENUMERATE
//! their contents, not just a count. This is the ONLY channel that
//! surfaces the parsed policy before the operator arms the killer
//! (2026-07-30 investigator finding — the pre-existing web
//! `/api/settings` carries only `default_ai_action_readonly` +
//! `auto_actuate_readonly`, no allowlist/blocklist).

use std::io::Write;
use std::process::Command;

/// Path to the built binary — populated by cargo at build time.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_raqib")
}

/// Write a TOML fixture to a fresh temp file and return the path.
/// Each caller gets a distinct file so parallel test execution can't
/// stomp another test's fixture.
fn tempfile(name: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "raqib_config_check_test_{}_{}",
        name,
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let path = dir.join(format!("{name}.toml"));
    let mut f = std::fs::File::create(&path).expect("create tempfile");
    f.write_all(contents.as_bytes()).expect("write tempfile");
    path
}

// ─────────────────────────────────────────────────────────────────
// Exit 0 — valid config prints allowlist+blocklist NAMES + OK
// ─────────────────────────────────────────────────────────────────

#[test]
fn valid_config_prints_policy_lists_and_exits_zero() {
    // Minimal-but-valid config exercising every printed section:
    // policy names, thresholds pair, governor disarm state.
    let path = tempfile(
        "valid",
        r#"
[web]
allow_no_auth = true

[thresholds]
ram_attention_pct = 14.0
ram_critical_pct  = 15.0
thermal_amber_c   = 100.0
thermal_red_c     = 120.0

[policy]
default_ai_action = "Allow"
allowlist = ["sshd", "bash", "systemd", "claude", "ros2"]
blocklist = ["ram_canary", "vram_canary"]
rate_limit_max_kills = 1

[governor]
auto_actuate      = false
kill_sustain_secs = 30
"#,
    );

    let out = Command::new(bin())
        .args(["config", "check", "--path"])
        .arg(&path)
        .output()
        .expect("spawn raqib config check");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0 on valid config; got {:?}. stdout=\n{stdout}\nstderr=\n{stderr}",
        out.status.code(),
    );

    // The path must be printed up front — resolves the operator's
    // "which file actually loaded?" pre-arm question.
    assert!(
        stdout.contains(path.to_str().unwrap()),
        "stdout must print the loaded config path; got:\n{stdout}"
    );
    // Load-bearing: full ALLOWLIST names, not a count.
    for name in ["sshd", "bash", "systemd", "claude", "ros2"] {
        assert!(
            stdout.contains(name),
            "stdout must enumerate allowlist name {name:?}; got:\n{stdout}"
        );
    }
    // Load-bearing: full BLOCKLIST names, not a count.
    for name in ["ram_canary", "vram_canary"] {
        assert!(
            stdout.contains(name),
            "stdout must enumerate blocklist name {name:?}; got:\n{stdout}"
        );
    }
    // Resolved thresholds visible — the operator's ram_critical_pct
    // set to 15.0 must appear (not the 95.0 default).
    assert!(
        stdout.contains("ram_critical_pct     = 15.0"),
        "stdout must show the RESOLVED (post-defaults) threshold value; got:\n{stdout}"
    );
    // Governor state must be legible with the DISARMED marker (the
    // pre-arm gate signal the operator scans for).
    assert!(
        stdout.contains("auto_actuate      = false")
            && stdout.contains("KILLER IS DISARMED"),
        "stdout must show auto_actuate + disarmed marker; got:\n{stdout}"
    );
    // Validation footer — the nginx -t idiom.
    assert!(
        stdout.trim_end().ends_with("VALIDATION: OK"),
        "stdout must end with VALIDATION: OK footer; got:\n{stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────
// Exit 2 — invalid config (threshold pair reversed)
// ─────────────────────────────────────────────────────────────────

#[test]
fn threshold_order_violation_exits_two_and_names_the_field_on_stderr() {
    // `ram_critical_pct < ram_attention_pct` fails `check_pair` in
    // `src/thresholds.rs`. The subcommand must catch it and exit
    // non-zero WITHOUT printing the success footer — a script
    // gating on `|| exit 1` must fail here.
    let path = tempfile(
        "bad_threshold",
        r#"
[web]
allow_no_auth = true

[thresholds]
ram_attention_pct = 90.0
ram_critical_pct  = 50.0
"#,
    );

    let out = Command::new(bin())
        .args(["config", "check", "--path"])
        .arg(&path)
        .output()
        .expect("spawn raqib config check");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "expected non-zero exit on invalid threshold pair; got 0. stdout=\n{stdout}\nstderr=\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "exit code MUST be 2 for a parse/validate/resolve failure; got {:?}",
        out.status.code(),
    );
    // Error must go to STDERR (so `> /tmp/loaded.txt` captures only
    // the pretty-print when it succeeds).
    assert!(
        stderr.contains("ram_critical_pct") && stderr.contains("ram_attention_pct"),
        "stderr must name the offending threshold field; got:\n{stderr}"
    );
    // Success footer MUST NOT appear on failure.
    assert!(
        !stdout.contains("VALIDATION: OK"),
        "VALIDATION: OK must not print on failure; got stdout:\n{stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────
// Exit 1 — no config file found (--path to nonexistent file)
// ─────────────────────────────────────────────────────────────────

#[test]
fn explicit_path_to_missing_file_exits_two_with_read_error_on_stderr() {
    // A dead --path is a file-read failure (exit 2), NOT a
    // "no config found in discovery" case (exit 1). The operator
    // named a specific file; not finding it is a hard error.
    let path = std::env::temp_dir().join(format!(
        "raqib_config_check_missing_{}.toml",
        std::process::id()
    ));
    // Ensure it doesn't exist.
    let _ = std::fs::remove_file(&path);

    let out = Command::new(bin())
        .args(["config", "check", "--path"])
        .arg(&path)
        .output()
        .expect("spawn raqib config check");

    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(2),
        "exit code MUST be 2 for --path to a nonexistent file; got {:?}. stderr=\n{stderr}",
        out.status.code(),
    );
    assert!(
        stderr.contains(path.to_str().unwrap()),
        "stderr must name the missing path; got:\n{stderr}"
    );
}

// ─────────────────────────────────────────────────────────────────
// Discovery + `--help` pin (documents the subcommand exists)
// ─────────────────────────────────────────────────────────────────

#[test]
fn help_documents_config_check_subcommand() {
    // `raqib --help` must list the `config` subcommand so operators
    // discover it. `raqib config --help` must list `check`. Guards
    // against a future clap refactor that would hide the pre-arm
    // gate from the CLI surface.
    let root_help = Command::new(bin())
        .arg("--help")
        .output()
        .expect("spawn raqib --help");
    let root_text = String::from_utf8_lossy(&root_help.stdout);
    assert!(
        root_text.contains("config"),
        "raqib --help must list the `config` subcommand; got:\n{root_text}"
    );

    let cfg_help = Command::new(bin())
        .args(["config", "--help"])
        .output()
        .expect("spawn raqib config --help");
    let cfg_text = String::from_utf8_lossy(&cfg_help.stdout);
    assert!(
        cfg_text.contains("check"),
        "raqib config --help must list the `check` subcommand; got:\n{cfg_text}"
    );
}
