//! edge_monitor entrypoint — Module 8.
//!
//! Owns CLI parsing, config loading, signal handling, and the choice between
//! TUI and headless modes. The tick loop itself lives in `runtime`/`ui`; this
//! module is intentionally thin so the integration surface is small.
//!
//! All pipeline types are re-exported from the library crate; this file
//! only orchestrates.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use tracing::Level;
use tracing_subscriber::EnvFilter;

use edge_monitor::config::Config;
use edge_monitor::runtime::{Runtime, RuntimeState};
use edge_monitor::ui;

#[derive(Parser, Debug)]
#[command(
    name = "edge_monitor",
    about = "Model-aware resource monitor and governor for edge AI workloads",
    version
)]
struct Cli {
    /// Path to TOML config (defaults to ./edge_monitor.toml if present).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Force dry-run regardless of config (overrides policy.enforce).
    #[arg(long)]
    dry_run: bool,

    /// Run without TUI; tick once per interval and log to stderr.
    #[arg(long)]
    no_ui: bool,

    /// Headless tick budget. Useful in CI / smoke tests. 0 = run until killed.
    #[arg(long, default_value_t = 0)]
    ticks: u64,

    /// Log level: trace, debug, info, warn, error.
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log_level)?;

    let config = load_config(cli.config.as_deref(), cli.dry_run)?;
    config.validate().context("config validation failed")?;

    if config.policy.enforce {
        tracing::warn!(
            "ENFORCE mode active — automated policy will send real signals. \
             Allowlist + rate limit + grace period still apply."
        );
    } else {
        tracing::info!("DRY-RUN mode — no signals will be sent.");
    }

    let shutdown = install_shutdown_handler()?;

    let runtime = Runtime::new(config);

    if cli.no_ui {
        run_headless(runtime, shutdown, cli.ticks)?;
    } else {
        // Returns the runtime back to us so we can flush state on the way out.
        let runtime = ui::run(runtime, shutdown)?;
        tracing::info!("exited cleanly after {} ticks", runtime.state().tick_count);
    }

    Ok(())
}

/// Headless per-tick log. Prints one aggregate line and — when AI workloads
/// are present — one detail line per AI process so operators can see the
/// exact model, PID, and resource footprint without running the TUI.
fn log_tick_summary(state: &RuntimeState) {
    let ai_procs: Vec<_> = state.ai_processes().collect();
    let exits = state
        .last_lifecycle
        .as_ref()
        .map(|l| l.recent_exits.len())
        .unwrap_or(0);
    tracing::info!(
        tick = state.tick_count,
        ai_processes = ai_procs.len(),
        exits = exits,
        "tick"
    );
    for p in &ai_procs {
        let vram_mb = p
            .vram_bytes
            .map(|b| format!("{}M", b / (1024 * 1024)))
            .unwrap_or_else(|| "-".into());
        let model = p.model_name.as_deref().unwrap_or("-");
        tracing::info!(
            pid = p.pid,
            name = %p.name,
            category = ?p.category,
            model = %model,
            cpu_pct = p.cpu_pct,
            rss_mb = p.rss_mb,
            vram = %vram_mb,
            "ai-process"
        );
    }
    // Operator-facing exit channel: only summarise processes the classifier
    // ever recognised as AI. Short-lived shells, udev workers, etc. still land
    // in the persistent JSONL summary log for forensic replay — the noise just
    // doesn't reach stderr. Drops from `category=None` to `Some(_)` are sticky
    // in lifecycle, so any process that was AI at any tick survives the filter.
    for summary in state
        .last_lifecycle
        .as_ref()
        .map(|l| l.recent_exits.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|s| s.category.is_some())
    {
        let model = summary.model_name.as_deref().unwrap_or("-");
        // Filter above guarantees Some(_); render the variant directly so the
        // exit line matches the live ai-process line (`category=Inference`)
        // instead of leaking `Some(Inference)` through the Debug formatter.
        let category = summary
            .category
            .map(|c| format!("{:?}", c))
            .unwrap_or_default();
        tracing::info!(
            pid = summary.pid,
            name = %summary.name,
            category = %category,
            model = %model,
            uptime_s = summary.uptime_secs,
            avg_cpu_pct = summary.avg_cpu_pct,
            peak_cpu_pct = summary.peak_cpu_pct,
            peak_rss_mb = summary.peak_rss_mb,
            peak_vram_mb = summary.peak_vram_mb,
            samples = summary.samples,
            "exit"
        );
    }
}

fn init_tracing(level: &str) -> anyhow::Result<()> {
    let lvl = match level.to_ascii_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        other => {
            return Err(anyhow::anyhow!(
                "invalid --log-level {}: expected one of trace,debug,info,warn,error",
                other
            ));
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(lvl.to_string())),
        )
        .with_target(false)
        .init();
    Ok(())
}

fn load_config(path: Option<&std::path::Path>, force_dry_run: bool) -> anyhow::Result<Config> {
    let mut cfg = match path {
        Some(p) => {
            tracing::info!(path = %p.display(), "loading config");
            Config::from_file(p).with_context(|| format!("loading {}", p.display()))?
        }
        None => {
            // Fall back to ./edge_monitor.toml if present; otherwise built-in defaults.
            let default_path = PathBuf::from("./edge_monitor.toml");
            if default_path.exists() {
                tracing::info!(path = %default_path.display(), "loading config");
                Config::from_file(&default_path)?
            } else {
                tracing::info!("no config file; using built-in defaults");
                Config::default()
            }
        }
    };
    if force_dry_run {
        cfg.force_dry_run();
    }
    Ok(cfg)
}

/// Install Ctrl-C / SIGTERM handler. Returns an Arc<AtomicBool> that flips
/// to true on the first signal. The TUI loop and the headless loop both
/// poll this flag between iterations.
fn install_shutdown_handler() -> anyhow::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    let handler_flag = flag.clone();
    ctrlc::set_handler(move || {
        // Repeated Ctrl-C while shutting down: exit hard. Better than hanging.
        if handler_flag.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        tracing::info!("shutdown requested; finishing current tick");
    })
    .context("failed to install signal handler")?;
    Ok(flag)
}

/// Headless tick loop. Uses an mpsc receiver as the timing primitive so we
/// never call `std::thread::sleep` directly — `recv_timeout` parks the
/// thread on a Condvar and wakes either on the timeout or on shutdown.
fn run_headless(
    mut runtime: Runtime,
    shutdown: Arc<AtomicBool>,
    tick_budget: u64,
) -> anyhow::Result<()> {
    let interval = Duration::from_millis(runtime.config().runtime.tick_interval_ms);
    let (tx, rx) = mpsc::channel::<()>();
    let _tx_keep_alive = tx; // keep sender alive so recv_timeout stays in business

    let mut ticks_done: u64 = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("shutdown signal received; exiting");
            break;
        }

        let started = Instant::now();
        match runtime.tick() {
            Ok(state) => log_tick_summary(state),
            Err(e) => {
                tracing::error!("tick failed: {}", e);
            }
        }
        runtime.record_governor_audit();

        ticks_done += 1;
        if tick_budget > 0 && ticks_done >= tick_budget {
            tracing::info!(ticks = ticks_done, "tick budget reached; exiting");
            break;
        }

        // Wait the remaining slice. recv_timeout returns immediately if the
        // shutdown handler ever sends on `tx` (it doesn't — the handler flips
        // the atomic — but the channel exists so we have a clean wait point
        // that does NOT use std::thread::sleep).
        let elapsed = started.elapsed();
        if elapsed < interval {
            let _ = rx.recv_timeout(interval - elapsed);
        }
    }

    Ok(())
}
