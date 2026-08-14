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
fn help_documents_bind_flag_with_localhost_default() {
    let help = run_help();
    assert!(
        help.contains("--bind"),
        "--bind flag missing from --help output:\n{help}"
    );
    // Secure-by-default: the user-facing default for `--bind`
    // MUST read as `127.0.0.1` so an operator who runs `--help`
    // sees that the dashboard is localhost-only until they
    // explicitly opt into LAN exposure (via `--bind 0.0.0.0`).
    // If this test ever regresses to asserting `0.0.0.0`, the
    // default was silently loosened to LAN-reachable — a
    // security posture change that must go through explicit
    // ratification, not a stealth CLI-default flip.
    assert!(
        help.contains("127.0.0.1"),
        "--bind default should read as 127.0.0.1 in --help:\n{help}"
    );
    // Complementary negative pin: `0.0.0.0` must NOT appear as
    // the default. It may still appear elsewhere in --help text
    // (e.g. the flag description mentions it as the opt-in
    // value), but the `[default: ...]` clause clap emits should
    // no longer print it. Guard by looking for the clap
    // convention `default value: 127.0.0.1`.
    assert!(
        help.contains("default value: 127.0.0.1")
            || help.contains("default: 127.0.0.1"),
        "clap default clause for --bind must name 127.0.0.1; got:\n{help}"
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
