//! S.0.8 — SIGTERM clean shutdown integration test.
//!
//! Today's audit flagged this as "needs re-verification" because no
//! commit message mentioned the ctrlc termination feature. Fix landed
//! by enabling `ctrlc/termination`. This test pins the behaviour so a
//! future bump to ctrlc (or accidental feature removal) re-surfaces.
//!
//! The test spawns the release binary headlessly, sends SIGTERM, and
//! verifies:
//!   * exit code is 0 (not 143 — that's the kernel default action)
//!   * stderr contains the drain log lines
//!
//! We don't attempt the orphan-children check here — that is more
//! reliably done in `scripts/manual/sigterm_smoke.sh`, where shell
//! `ps --ppid` is the natural tool. The test focuses on the exit code
//! + log shape, which are the load-bearing claims.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_edge_monitor")
}

#[test]
fn sigterm_drains_and_exits_zero() {
    // Run a fast tick interval so the loop demonstrably processes a
    // tick before and after we send the signal. Default 1000 ms would
    // be slower; a tempfile config keeps the test self-contained.
    let cfg_dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = cfg_dir.path().join("em.toml");
    std::fs::write(&cfg_path, "[runtime]\ntick_interval_ms = 100\n").unwrap();

    let mut child = Command::new(binary())
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--no-ui",
            "--ticks",
            "0",
            "--log-format",
            "json",
        ])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn edge_monitor");

    // Let at least one tick complete so the shutdown handler has
    // something to do. 400 ms covers tick + interval slack.
    thread::sleep(Duration::from_millis(400));

    let pid = child.id();
    // SAFETY: we just spawned the child; pid is valid until we wait().
    // libc::kill with SIGTERM returns 0 on success, -1 on error.
    unsafe {
        let rc = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        assert_eq!(rc, 0, "kill -TERM failed");
    }

    // Wait up to 5 s for the process to exit cleanly.
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break s,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("edge_monitor did not exit within 5s of SIGTERM");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    };

    assert!(
        status.success(),
        "expected exit 0 (clean shutdown via signal handler); got {:?}. \
         Exit 143 = 128+15 means SIGTERM bypassed the handler — the \
         ctrlc `termination` feature is not enabled.",
        status.code()
    );

    let mut stderr = String::new();
    child
        .stderr
        .as_mut()
        .expect("captured stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert!(
        stderr.contains("\"message\":\"shutdown requested"),
        "missing 'shutdown requested' log line; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("\"message\":\"shutdown signal received"),
        "missing 'shutdown signal received; exiting' log line; stderr=\n{stderr}"
    );
}
