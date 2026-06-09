//! edge_monitor entrypoint — Module 8.
//!
//! Owns CLI parsing, config loading, signal handling, and the choice between
//! TUI and headless modes. The tick loop itself lives in `runtime`/`ui`; this
//! module is intentionally thin so the integration surface is small.
//!
//! All pipeline types are re-exported from the library crate; this file
//! only orchestrates.

// v1.1.7 ITEM 3 fallback (DISPATCH 22, operator-pre-approved) —
// mimalloc as the global allocator. ITEM 1's Arc-share fix dropped
// per-tick clone pressure ~85%, but the 5-min RSS endurance against
// 10× ROS2 publishers still showed ~220 MB/min growth from glibc
// allocator fragmentation under high short-lived-allocation churn
// (sysinfo /proc scans, JSON serialization, subprocess pipe buffers).
// mimalloc tracks free lists per-thread, coalesces aggressively, and
// returns pages to the OS on a faster cadence — closes the residual
// gap without a sampler-by-sampler allocation audit. Default features
// disabled to skip secure-mode overhead (not relevant here) and the
// experimental override layer.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::Level;
use tracing_subscriber::EnvFilter;

use edge_monitor::compare;
use edge_monitor::config::Config;
use edge_monitor::exec_wrapper;
use edge_monitor::history;
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

    /// Run without TUI; tick once per interval and log to stderr.
    #[arg(long)]
    no_ui: bool,

    /// Headless tick budget. Useful in CI / smoke tests. 0 = run until killed.
    #[arg(long, default_value_t = 0)]
    ticks: u64,

    /// Log level: trace, debug, info, warn, error.
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Log output format: "human" (default, K=V text) or "json"
    /// (one JSON object per line, parseable by `jq`). Use `json` for
    /// headless / pipeline-style runs where downstream tooling expects
    /// a stable schema (TEST.md S.2.3).
    #[arg(long, default_value = "human", value_parser = ["human", "json"])]
    log_format: String,

    /// Force tracing logs to stderr instead of the default log file
    /// when running the TUI. Useful for debugging the TUI itself or
    /// when `~/.cache/edge_monitor/` is unwritable. Has no effect in
    /// `--no-ui` mode or under subcommands (those already use stderr).
    #[arg(long)]
    log_stderr: bool,

    /// UI theme: `dark` (default), `light`, or `high-contrast`. Per
    /// UX_CONTRACT.md §13. CLI flag overrides `[ui].theme` in the
    /// config; unrecognised values fall back to `dark` at render time.
    #[arg(long, value_name = "NAME")]
    theme: Option<String>,

    /// Sprint-6 — disable the embedded web UI. Default is to bind
    /// the Svelte dashboard on port 7070 (see `--bind`) alongside
    /// the TUI; pass `--no-web` for headless / CI runs where the
    /// HTTP listener would be noise.
    #[arg(long)]
    no_web: bool,

    /// Sprint-7 — web UI listen address. Defaults to `0.0.0.0` so
    /// the dashboard is reachable from any host on the same LAN.
    /// **There is NO authentication in v1.0** — pass
    /// `--bind 127.0.0.1` to restrict the listener to localhost if
    /// the host is on an untrusted network. See README "Web UI
    /// security" for the trusted-LAN assumption.
    #[arg(long, default_value = "0.0.0.0")]
    bind: std::net::IpAddr,

    /// Sprint-6 — web UI listen port. Default 7070.
    #[arg(long, default_value_t = 7070)]
    port: u16,

    /// Subcommand. Defaults to running the monitor (TUI / headless) when
    /// omitted, preserving the Phase-1 invocation.
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show recent runs from the typed run store (latest.md Tier 1.1).
    History {
        /// Filter to runs of this model. Omit to list all known models.
        model: Option<String>,
        /// Maximum number of runs to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Side-by-side baseline comparison across one or more models
    /// (latest.md Tier 3.7).
    Compare {
        /// Models to compare. At least one. Use `edge_monitor history`
        /// (no args) to list known models.
        #[arg(required = true, num_args = 1..)]
        models: Vec<String>,
        /// Number of recent runs per model to fold into the baseline.
        #[arg(long, default_value_t = 10)]
        runs: usize,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Run a workload under instrumentation: forks COMMAND with
    /// piped stdio, tees the output to your terminal and to the
    /// stdout regex parser, then writes a `RunRecord` on exit
    /// (latest.md Tier 1.2d).
    Exec {
        /// Optional label for the model_name field of the run record.
        /// Defaults to argv[0] of the wrapped command.
        #[arg(long)]
        name: Option<String>,
        /// Command to run. Use `--` to separate edge_monitor flags
        /// from the wrapped command.
        #[arg(required = true, last = true)]
        command: Vec<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // TUI mode = no `--no-ui`, no subcommand, and no explicit
    // `--log-stderr` opt-out. In that mode tracing must NOT touch
    // stderr — the alternate-screen TUI shares the same buffer and
    // every log line corrupts the frame. Route to a file instead.
    let log_to_file = !cli.no_ui && cli.command.is_none() && !cli.log_stderr;
    init_tracing(&cli.log_level, &cli.log_format, log_to_file)?;

    let config = load_config(cli.config.as_deref())?;
    config.validate().context("config validation failed")?;

    // Subcommand path: query-only, no signal handler / runtime needed.
    if let Some(cmd) = cli.command {
        return match cmd {
            Commands::History { model, limit, json } => {
                history::run_history(model, limit, json, &config)
            }
            Commands::Compare { models, runs, json } => {
                compare::run_compare(models, runs, json, &config)
            }
            Commands::Exec { name, command } => {
                // exec needs an async runtime — spin one up locally.
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?;
                let code = rt.block_on(exec_wrapper::run_exec(name, command, &config))?;
                std::process::exit(code);
            }
        };
    }

    // v1.0.3 RIDE 1 — removed the "Governor will send real signals"
    // startup WARN that fired at every launch. v1.0.1 flipped
    // `policy.default_ai_action` to Allow and routed the kill-verb
    // branch in `record_governor_audit` to a no-op, so the warning
    // claimed an automated kill path that no longer exists. Manual
    // kills still require the kill_confirm card per UX_CONTRACT §6
    // (CAR-17); the help overlay already surfaces that.
    let shutdown = install_shutdown_handler()?;

    // v1.3.1 — `Runtime::new` now validates the operator's
    // `[thresholds]` config section and rejects invalid combinations
    // with an operator-actionable message (no silent clamp; v1.0.1
    // phantom-kill lesson). A bad TOML fails to start with the
    // resolver's error verbatim — the operator sees exactly which
    // field is wrong and what the constraint is.
    let runtime = Runtime::new(config)
        .with_context(|| "invalid configuration; aborting startup")?;

    // Sprint-6 — spawn the web companion on a background thread
    // BEFORE the TUI / headless loop takes the main thread. The TUI
    // tick loop publishes snapshots into a `watch::Sender` (created
    // here); the axum server holds the matching receiver. Disabled
    // with `--no-web` for headless / CI runs.
    let web_tx_for_loop = if cli.no_web {
        tracing::info!("--no-web set; web UI disabled");
        None
    } else {
        match spawn_web_server(cli.bind, cli.port, shutdown.clone()) {
            Ok(tx) => Some(tx),
            Err(e) => {
                // Don't fail the whole binary if the web companion
                // can't bind — the TUI is the primary surface, and
                // an EADDRINUSE shouldn't kill the operator's
                // monitoring session.
                tracing::warn!(error = %e, "web: server failed to start; continuing without it");
                None
            }
        }
    };

    if cli.no_ui {
        run_headless(runtime, shutdown, cli.ticks, web_tx_for_loop)?;
    } else {
        // §13 — resolve theme from CLI flag → [ui].theme → default
        // `dark`. The CLI string wins outright when provided so an
        // operator can flip themes for a single launch without
        // touching the config file.
        let theme_name = cli
            .theme
            .clone()
            .unwrap_or_else(|| runtime.config().ui.theme.clone());
        let theme = edge_monitor::ui::theme::current_theme(&theme_name);
        // Returns the runtime back to us so we can flush state on the way out.
        let runtime = ui::run(runtime, shutdown, theme, web_tx_for_loop)?;
        tracing::info!("exited cleanly after {} ticks", runtime.state().tick_count);
    }

    Ok(())
}

/// Sprint-6 — spawn the embedded web server on a background tokio
/// runtime. Returns the `watch::Sender` so the TUI / headless loop
/// can publish snapshots into it on every tick.
///
/// Sprint-7 Item 4 — `bind` is the listen address. Defaults to
/// `0.0.0.0` (any interface, accessible from the LAN); restrict
/// with `--bind 127.0.0.1` for localhost-only. **There is no auth
/// in v1.0** — the wider bind explicitly assumes a trusted LAN per
/// the README "Web UI security" section.
///
/// The `shutdown` flag plumbed in here is the same `Arc<AtomicBool>`
/// the rest of the binary watches; a background task polls it and
/// resolves the axum graceful-shutdown future when the operator
/// quits the TUI.
fn spawn_web_server(
    bind: std::net::IpAddr,
    port: u16,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<tokio::sync::watch::Sender<edge_monitor::web::WireSnapshot>> {
    use edge_monitor::web::{WebState, WireSnapshot, serve};

    let (tx, rx) = tokio::sync::watch::channel(WireSnapshot::empty());
    let state = WebState { rx };
    let addr: std::net::SocketAddr = (bind, port).into();

    // Sprint-7 Item 4 — surface the no-auth + LAN-exposure tradeoff
    // at startup so the operator can't claim they weren't warned.
    // The warning fires unconditionally (whether bind is 0.0.0.0 or
    // localhost) because even a localhost bind is auth-less; the
    // line just helps the operator notice the wider-than-expected
    // listener address when one is set.
    if !bind.is_loopback() {
        tracing::warn!(
            addr = %addr,
            "web UI on {addr} — NO AUTH, trusted LAN only. \
             Restrict with --bind 127.0.0.1 on untrusted networks."
        );
    }

    // Dedicated tokio runtime on a background OS thread so the TUI
    // tick loop (sync) and the web server (async) don't fight for
    // the main thread.
    std::thread::Builder::new()
        .name("web-runtime".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "web: tokio runtime build failed");
                    return;
                }
            };
            rt.block_on(async move {
                let shutdown_fut = async move {
                    // Poll the shutdown flag at the render cadence
                    // (~100 ms) so we resolve quickly when the TUI
                    // exits. axum's `with_graceful_shutdown` awaits
                    // this future and stops accepting new
                    // connections when it resolves.
                    loop {
                        if shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                };
                tracing::info!(
                    addr = %addr,
                    frontend = edge_monitor::web::handlers::frontend_build_status(),
                    "web: starting"
                );
                if let Err(e) = serve(addr, state, shutdown_fut).await {
                    tracing::warn!(error = %e, "web: server exited with error");
                }
            });
        })?;

    Ok(tx)
}

/// Headless per-tick log. Prints one aggregate line and — when AI workloads
/// are present — one detail line per AI process so operators can see the
/// exact model, PID, and resource footprint without running the TUI.
/// v1.1.11 / DISPATCH 36 — headless-mode alert emission to the
/// tracing log. Pre-v1.1.11 `--no-ui` never constructed `App` and
/// the alert state machine lived on `App`, so headless surfaces
/// (operators tailing logs, ops-driven dashboards via journald)
/// silently dropped every alert that fired.
///
/// One INFO line per VISIBLE alert per tick. Visible == Active
/// (per `AlertState::visible`) — Suppressed (ack'd) and Pending
/// (sustain-gated, not yet fired) slots are intentionally NOT
/// emitted. The `alert.fire=` prefix is grep-able; the line shape
/// is intentionally machine-readable so journald / vector / etc.
/// can pattern-match without needing a JSON formatter.
///
/// AUTHORITY LOCK: this is observation-only. The log line never
/// triggers any actuation; it's literally `tracing::info!`.
fn log_visible_alerts(state: &RuntimeState) {
    let visible = state.alerts.visible();
    if visible.is_empty() {
        return;
    }
    for entry in &visible {
        tracing::info!(
            alert.fire = ?entry.alert_id,
            scope = ?entry.scope,
            pid = entry.pid.map(|p| p as i64).unwrap_or(-1),
            workload = %entry.workload_name,
            "alert visible (headless)"
        );
    }
}

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

    // DESIGN_HANDOFF Principle 6 — one-shot teaching hint when the
    // first tick sees nothing AI-flavoured. A first-time user runs
    // `edge_monitor --no-ui --ticks 5`, sees five "ai_processes=0"
    // lines, and has no idea that's because they haven't started a
    // workload. The hint fires once per process lifetime and only
    // when the *first* tick observed zero AI processes — silent for
    // every subsequent tick (so log scrapers stay clean) and silent
    // entirely for users whose first tick already saw a workload.
    static EMPTY_HINT_EMITTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if state.tick_count == 1
        && ai_procs.is_empty()
        && !EMPTY_HINT_EMITTED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        tracing::info!(
            "No AI workloads detected this tick. Try one of these in \
             another terminal — edge_monitor will pick it up on the \
             next tick: `ollama run llama3 'hello'`, \
             `vllm serve <model>`, or wrap with \
             `edge_monitor exec -- <your command>`."
        );
    }
    for p in &ai_procs {
        // Two emission sites: with and without `model=`. Keeps the
        // structured-log shape clean — when no model name was extracted,
        // we omit the field entirely instead of emitting `model=-`,
        // which downstream parsers (jq, fluentd) would have to filter
        // out by value rather than by key presence.
        let model = p.model_name.as_deref().filter(|m| !m.is_empty());
        let vram_mb = p.vram_bytes.filter(|b| *b > 0).map(|b| b / (1024 * 1024));
        match (model, vram_mb) {
            (Some(model), Some(vram)) => tracing::info!(
                pid = p.pid,
                name = %p.name,
                category = ?p.category,
                model = %model,
                cpu_pct = p.cpu_pct,
                rss_mb = p.rss_mb,
                vram_mb = vram,
                "ai-process"
            ),
            (Some(model), None) => tracing::info!(
                pid = p.pid,
                name = %p.name,
                category = ?p.category,
                model = %model,
                cpu_pct = p.cpu_pct,
                rss_mb = p.rss_mb,
                "ai-process"
            ),
            (None, Some(vram)) => tracing::info!(
                pid = p.pid,
                name = %p.name,
                category = ?p.category,
                cpu_pct = p.cpu_pct,
                rss_mb = p.rss_mb,
                vram_mb = vram,
                "ai-process"
            ),
            (None, None) => tracing::info!(
                pid = p.pid,
                name = %p.name,
                category = ?p.category,
                cpu_pct = p.cpu_pct,
                rss_mb = p.rss_mb,
                "ai-process"
            ),
        }
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
        let model = summary
            .model_name
            .as_deref()
            .filter(|m| !m.is_empty());
        // Filter above guarantees Some(_); render the variant directly so the
        // exit line matches the live ai-process line (`category=Inference`)
        // instead of leaking `Some(Inference)` through the Debug formatter.
        let category = summary
            .category
            .map(|c| format!("{:?}", c))
            .unwrap_or_default();
        // Drop `peak_vram_mb` from the structured fields when zero — same
        // rationale as the live `vram_mb` branch above. Same for `model`.
        match (model, summary.peak_vram_mb) {
            (Some(model), vram) if vram > 0 => tracing::info!(
                pid = summary.pid,
                name = %summary.name,
                category = %category,
                model = %model,
                uptime_s = summary.uptime_secs,
                avg_cpu_pct = summary.avg_cpu_pct,
                peak_cpu_pct = summary.peak_cpu_pct,
                peak_rss_mb = summary.peak_rss_mb,
                peak_vram_mb = vram,
                samples = summary.samples,
                "exit"
            ),
            (Some(model), _) => tracing::info!(
                pid = summary.pid,
                name = %summary.name,
                category = %category,
                model = %model,
                uptime_s = summary.uptime_secs,
                avg_cpu_pct = summary.avg_cpu_pct,
                peak_cpu_pct = summary.peak_cpu_pct,
                peak_rss_mb = summary.peak_rss_mb,
                samples = summary.samples,
                "exit"
            ),
            (None, vram) if vram > 0 => tracing::info!(
                pid = summary.pid,
                name = %summary.name,
                category = %category,
                uptime_s = summary.uptime_secs,
                avg_cpu_pct = summary.avg_cpu_pct,
                peak_cpu_pct = summary.peak_cpu_pct,
                peak_rss_mb = summary.peak_rss_mb,
                peak_vram_mb = vram,
                samples = summary.samples,
                "exit"
            ),
            (None, _) => tracing::info!(
                pid = summary.pid,
                name = %summary.name,
                category = %category,
                uptime_s = summary.uptime_secs,
                avg_cpu_pct = summary.avg_cpu_pct,
                peak_cpu_pct = summary.peak_cpu_pct,
                peak_rss_mb = summary.peak_rss_mb,
                samples = summary.samples,
                "exit"
            ),
        }
    }
}

fn init_tracing(level: &str, format: &str, log_to_file: bool) -> anyhow::Result<()> {
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
    // stderr: stdout is reserved for subcommand output (e.g. JSON from
    // `history --json`). A consumer piping `edge_monitor history --json
    // | jq` would otherwise see the tracing log first and choke.
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(lvl.to_string()));

    if log_to_file {
        // TUI owns the alternate screen, which shares the terminal
        // buffer with stderr — any tracing line written there would
        // smear ANSI escapes across the rendered frame. Route to a
        // file under $HOME/.cache/edge_monitor instead. Falls back to
        // $TMPDIR/edge_monitor when HOME is unset (containers, init).
        let cache_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".cache").join("edge_monitor"))
            .unwrap_or_else(|| std::env::temp_dir().join("edge_monitor"));
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("creating log dir {}", cache_dir.display()))?;
        let log_path = cache_dir.join("edge_monitor.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening log file {}", log_path.display()))?;
        let builder = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file));
        match format {
            "json" => builder.json().flatten_event(true).init(),
            _ => builder.init(),
        }
        // Surface the path on stderr BEFORE the TUI takes the screen
        // so users know where to find logs. Single line, lands above
        // EnterAlternateScreen's clear and is preserved when the TUI
        // exits and the original buffer is restored.
        eprintln!("logs: {}", log_path.display());
    } else {
        // ANSI escape codes are great in an interactive terminal but break
        // grep / jq / log shippers reading piped stderr — they end up
        // splitting tokens like `tick=1` across `tick`, an SGR sequence,
        // and `=1`. Auto-detect via IsTerminal so live operators still get
        // colour while CI / headless runs stay machine-readable.
        let stderr_is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
        let builder = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_ansi(stderr_is_tty)
            .with_writer(std::io::stderr);
        match format {
            "json" => builder.json().flatten_event(true).init(),
            // "human" or anything clap accepted (clap restricts to {human, json}).
            _ => builder.init(),
        }
    }
    Ok(())
}

fn load_config(path: Option<&std::path::Path>) -> anyhow::Result<Config> {
    let cfg = match path {
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
                tracing::info!(
                    "Running with built-in defaults (no edge_monitor.toml \
                     found). Run history persists at \
                     ~/.local/share/edge_monitor. See \
                     edge_monitor.toml.example for tunables, or run with \
                     --config <path> to load one."
                );
                Config::default()
            }
        }
    };
    Ok(cfg)
}

/// Install Ctrl-C / SIGTERM / SIGHUP shutdown handler (S.0.8). Returns
/// an Arc<AtomicBool> that flips to true on the first signal. The TUI
/// loop and the headless loop both poll this flag between iterations.
///
/// `ctrlc`'s `termination` feature is what extends coverage from SIGINT
/// alone to SIGINT + SIGTERM + SIGHUP (Linux/macOS) and Ctrl-Break
/// (Windows). Without it, `kill -TERM <pid>` fell through to the
/// kernel's default action (exit 143, no shutdown log, no flush) — the
/// audit caught that and S.0.8 fixes it.
fn install_shutdown_handler() -> anyhow::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    let handler_flag = flag.clone();
    ctrlc::set_handler(move || {
        // Repeated signal while shutting down: exit hard. Better than hanging.
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
    web_tx: Option<tokio::sync::watch::Sender<edge_monitor::web::WireSnapshot>>,
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
            Ok(state) => {
                log_tick_summary(state);
                // v1.1.11 / DISPATCH 36 — emit any visible alerts to
                // the headless log on the same tick they're observed.
                // Pre-v1.1.11 `--no-ui` never constructed `App`, so
                // alerts were silently dropped (the eval lived on
                // App). Now that the state machine is on Runtime,
                // the headless surface gets the same `INFO` line per
                // visible alert as the TUI's status footer would
                // surface, prefixed with `alert.fire=` so it's
                // grep-able.
                log_visible_alerts(state);
            }
            Err(e) => {
                tracing::error!("tick failed: {}", e);
            }
        }
        runtime.record_governor_audit();

        // Sprint-6 — publish a fresh wire snapshot to the web
        // companion when running with `--no-ui` (headless +
        // web is a defensible mode: headless logging + remote
        // dashboard for ops who don't want a terminal up).
        if let Some(tx) = web_tx.as_ref() {
            // v1.0.1 B-NEW-8 — AI-only filter for the exit branch
            // lives inside `build_activity` now; non-AI shell exits
            // still don't reach the dashboard's activity feed.
            // v1.3.2 / DISPATCH 71 — the wire builder reads the
            // three event sources directly from state
            // (`completed` + `audit` + `regressions`); no more
            // recent-RunRecord pre-mapping here. Cap +
            // sort-time-desc + projection live in
            // `web::wire::build_activity`.
            let snap = edge_monitor::web::WireSnapshot::from_runtime_state(runtime.state());
            let _ = tx.send(snap);
        }

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
