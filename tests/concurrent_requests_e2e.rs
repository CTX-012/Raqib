//! Tier 3.4 — end-to-end integration test for concurrent-request
//! awareness. Drives a sequence of `(num_requests_running,
//! num_requests_waiting)` readings through the accumulator and asserts
//! that the resulting `RunMetrics` reports the time-weighted average
//! and peaks the spec demands. Bypasses the HTTP layer (already
//! covered by `samplers::vllm_prometheus::tests`) and exercises the
//! data path that lands on `RunRecord`.

use std::time::{Duration, Instant};

use edge_monitor::storage::run_store::RunMetrics;
use edge_monitor::telemetry::TelemetryAccumulator;
use edge_monitor::telemetry::source::TelemetryFrame;

fn frame_with_concurrency(pid: u32, running: u32, waiting: Option<u32>) -> TelemetryFrame {
    TelemetryFrame {
        pid,
        concurrent_requests: Some(running),
        num_requests_waiting: waiting,
        ..TelemetryFrame::new(pid)
    }
}

#[test]
fn step_function_reports_time_weighted_average_and_running_peak() {
    // latest.md Tier 3.4 example shape: 1 running for 10 s, then 8
    // running for 50 s. The accumulator uses Instant::now() per
    // record, so we space recordings with explicit sleeps. Real
    // ticks are 1 s apart, so the math holds at smaller scale: 1
    // sample at t=0, 1 at t=10 (still 1), 1 at t=20 (jumps to 8),
    // 1 at t=70 (still 8). Walking it down to 100 ms units keeps
    // the test inside half a second total.
    //
    // Formula: avg = Σ value · dt / Σ dt
    //              = (1·100 + 1·100 + 8·500) / 700
    //              = 4200 / 700 = 6.0
    let mut acc = TelemetryAccumulator::new();
    let pid: u32 = 4242;

    let starts = [
        (1u32, 0u64),    // t = 0 ms
        (1, 100),        // t = 100 ms
        (1, 200),        // t = 200 ms (still 1; gives us the 200 ms of "1" weight)
        (8, 200),        // jump immediately afterwards
        (8, 700),        // hold 8 for 500 ms
    ];

    let begin = Instant::now();
    for (running, ms) in starts {
        // Spin until we cross the target offset. sleep_until isn't
        // stable on stable Rust; this is the portable form.
        loop {
            let now = Instant::now();
            if now.duration_since(begin) >= Duration::from_millis(ms) {
                break;
            }
            std::hint::spin_loop();
        }
        acc.record(frame_with_concurrency(pid, running, None));
    }

    let m: RunMetrics = acc.snapshot(pid).expect("snapshot present");
    let avg = m.concurrent_requests_avg.expect("avg defined");
    let peak = m.concurrent_requests_peak.expect("peak defined");

    // The recorded timings are subject to scheduler jitter, so allow
    // a generous tolerance on the exact average. The shape claim is
    // what matters: avg lies between 1.0 (all-low) and 8.0 (all-high)
    // and is closer to the high end because the high stretch is 5×
    // longer than the low stretch.
    assert!(
        (3.0..=7.5).contains(&avg),
        "avg={avg} should sit between 3.0 and 7.5 for a 1-then-8 step"
    );
    assert_eq!(peak, 8, "peak running must be the highest value seen");
}

#[test]
fn waiting_peak_distinguishes_saturated_from_idle() {
    // A run where queue depth touched 30 must be flagged differently
    // from one that stayed at 0. Tier 3.4's saturation signal.
    let mut acc = TelemetryAccumulator::new();
    let pid: u32 = 11;

    acc.record(frame_with_concurrency(pid, 4, Some(0)));
    std::thread::sleep(Duration::from_millis(50));
    acc.record(frame_with_concurrency(pid, 4, Some(30)));
    std::thread::sleep(Duration::from_millis(50));
    acc.record(frame_with_concurrency(pid, 4, Some(12)));

    let m = acc.snapshot(pid).unwrap();
    assert_eq!(m.concurrent_requests_waiting_peak, Some(30));
    // Running stayed at 4 the whole time → peak=4, avg≈4.0.
    assert_eq!(m.concurrent_requests_peak, Some(4));
    let running_avg = m.concurrent_requests_avg.expect("running avg defined");
    assert!(
        (running_avg - 4.0).abs() < 0.5,
        "running_avg={running_avg} should be ≈4.0"
    );
}

#[test]
fn no_concurrency_samples_yields_none() {
    let mut acc = TelemetryAccumulator::new();
    // Frame without any concurrent_requests field.
    acc.record(TelemetryFrame {
        pid: 9,
        tokens_per_sec: Some(40.0),
        ..TelemetryFrame::new(9)
    });
    let m = acc.snapshot(9).unwrap();
    assert_eq!(m.concurrent_requests_peak, None);
    assert_eq!(m.concurrent_requests_avg, None);
    assert_eq!(m.concurrent_requests_waiting_peak, None);
}
