//! Sprint-7 Item 4 — CLI-flag regression tests for the web bind /
//! port surface.
//!
//! These tests parse the binary's argv via clap (the `Cli` struct
//! lives in `src/main.rs` and isn't reachable from the lib crate),
//! so they assert the user-facing default by spawning the built
//! binary with `--help` and inspecting the printed defaults.
//! Lightweight — no full TUI run.

use std::process::Command;

fn run_help() -> String {
    let bin = env!("CARGO_BIN_EXE_raqib");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .expect("spawn edge_monitor --help");
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn help_documents_bind_flag_with_zero_default() {
    let help = run_help();
    assert!(
        help.contains("--bind"),
        "--bind flag missing from --help output:\n{help}"
    );
    // The user-facing default for `--bind` must read as `0.0.0.0`
    // so an operator who runs `--help` sees the LAN-exposure
    // posture before launching for the first time.
    assert!(
        help.contains("0.0.0.0"),
        "--bind default should read as 0.0.0.0 in --help:\n{help}"
    );
}

#[test]
fn help_documents_port_flag_with_7070_default() {
    let help = run_help();
    assert!(
        help.contains("--port"),
        "--port flag missing from --help output:\n{help}"
    );
    assert!(
        help.contains("7070"),
        "--port default 7070 missing from --help:\n{help}"
    );
}

#[test]
fn help_documents_no_web_flag() {
    let help = run_help();
    assert!(
        help.contains("--no-web"),
        "--no-web flag missing from --help output:\n{help}"
    );
}
