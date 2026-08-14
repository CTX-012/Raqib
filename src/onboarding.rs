//! First-run onboarding: config discovery + `--init` starter config
//! + actionable errors for a new user's first launch.
//!
//! Split from `main.rs` so the discovery order and the generated
//! starter config are unit-testable (main.rs is thin; onboarding is
//! where the UX logic lives).
//!
//! ## Config discovery order (fallback chain when `--config` is not set)
//!
//!   1. `--config <path>` (explicit; caller checks this before calling)
//!   2. `./raqib.toml`             (CWD)
//!   3. `~/.config/raqib/raqib.toml` (XDG standard;
//!      where a Linux user EXPECTS an app's config)
//!   4. `/etc/raqib/raqib.toml`      (system-wide;
//!      lowest priority, rarely populated on personal machines)
//!   5. LEGACY (raqib rename): `./edge_monitor.toml`,
//!      `~/.config/edge_monitor/edge_monitor.toml`,
//!      `/etc/edge_monitor/edge_monitor.toml`. Loading any of
//!      these emits a `tracing::warn!` deprecation note pointing
//!      the operator at `raqib init` to migrate. Planned removal
//!      in the next version — one release of overlap only, so
//!      existing users don't have to re-init on upgrade.
//!
//! First existing file wins. If none exist, callers fall back to
//! `Config::default()` and — when web is enabled — get a friendly
//! "no config found; run `raqib init`" error via
//! [`no_config_error_message`].
//!
//! ## `--init` and the safe-off starter config
//!
//! [`DEFAULT_CONFIG_TOML`] is what `raqib init` writes. It:
//!
//!   * Ships the governor OFF: `[governor] auto_actuate = false` +
//!     `[policy] default_ai_action = "Allow"`. Killer is quiet by
//!     default (the observer-only v1 posture); a new user gets
//!     read-only vitals + workloads without any risk of accidental
//!     kills. Enabling auto-kill requires editing the config +
//!     restart per the safety-verdict investigation.
//!   * Sets `[web] allow_no_auth = true` with a LOUD comment
//!     explaining the security trade-off. Rationale: first-run
//!     friendliness — the user can immediately open the dashboard
//!     without hunting a generated token out of a file. The comment
//!     is unmissable and points to `auth_token` for LAN/remote use.
//!     Alternative considered: generate a random `auth_token`. That
//!     is MORE secure but adds friction (the user must find the
//!     token before the web UI works). Chosen (a) per dispatch
//!     recommendation; documented here so future readers can see the
//!     trade-off explicitly.
//!
//! ## Not clobber-safe by default
//!
//! `raqib init` refuses to overwrite an existing config unless
//! the caller passes `--force`. Preserves the operator's edits.

use std::path::{Path, PathBuf};

/// Where the config-discovery fallback chain looks after
/// `--config <path>` (which is checked by the caller before invoking
/// discovery). Ordered highest-priority first. Existence is checked
/// per-entry; the first present file wins.
///
/// See [`legacy_config_search_paths_with_home`] for the raqib
/// rename's one-release-overlap `edge_monitor.toml` fallback set.
pub fn config_search_paths() -> Vec<PathBuf> {
    config_search_paths_with_home(std::env::var("HOME").ok().as_deref())
}

/// Pure counterpart to [`config_search_paths`] — takes an optional
/// HOME value explicitly so tests can pin behaviour without racing
/// on the process-wide environment. `None` → skip the XDG entry (as
/// happens when `$HOME` is unset in production).
pub fn config_search_paths_with_home(home: Option<&str>) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("./raqib.toml")];
    if let Some(h) = home {
        paths.push(PathBuf::from(h).join(".config/raqib/raqib.toml"));
    }
    paths.push(PathBuf::from("/etc/raqib/raqib.toml"));
    paths
}

/// raqib rename — legacy edge_monitor.toml discovery paths. Read
/// as a FALLBACK after [`config_search_paths_with_home`] misses;
/// when a legacy file is loaded, the caller emits a
/// `tracing::warn!` deprecation note pointing the operator at
/// `raqib init` to migrate. One release only — planned removal in
/// the next version.
pub fn legacy_config_search_paths_with_home(home: Option<&str>) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("./edge_monitor.toml")];
    if let Some(h) = home {
        paths.push(PathBuf::from(h).join(".config/edge_monitor/edge_monitor.toml"));
    }
    paths.push(PathBuf::from("/etc/edge_monitor/edge_monitor.toml"));
    paths
}

/// Convenience env-driven variant of the legacy fallback list.
pub fn legacy_config_search_paths() -> Vec<PathBuf> {
    legacy_config_search_paths_with_home(std::env::var("HOME").ok().as_deref())
}

/// Default target path for `raqib init` (the XDG user-config
/// location). Same shape the discovery-chain looks at as entry #3, so
/// a freshly-initialised config is picked up automatically on the
/// next `raqib` invocation without any flag.
///
/// Returns `None` when `$HOME` isn't set (rare in interactive
/// contexts; may happen under `sudo -H` weirdness or minimal
/// containers) — the caller then requires an explicit `--path`.
pub fn default_init_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/raqib/raqib.toml"))
}

/// Result of `raqib init`, surfaced to the CLI so the
/// operator sees `where` the config was written.
#[derive(Debug, PartialEq)]
pub enum InitOutcome {
    /// Wrote the starter config to this path.
    Wrote(PathBuf),
    /// The file already existed and `force` was not set. Caller
    /// prints an operator-actionable error naming the path.
    RefusedExisting(PathBuf),
}

/// Write the starter config to `target`. Creates parent directories
/// if they don't exist. Refuses to overwrite an existing file unless
/// `force = true` — the operator's edits stay intact across repeated
/// `init` invocations.
///
/// The written bytes are exactly [`DEFAULT_CONFIG_TOML`] — a heavily
/// commented safe-off template.
pub fn write_starter_config(target: &Path, force: bool) -> std::io::Result<InitOutcome> {
    if target.exists() && !force {
        return Ok(InitOutcome::RefusedExisting(target.to_path_buf()));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, DEFAULT_CONFIG_TOML)?;
    Ok(InitOutcome::Wrote(target.to_path_buf()))
}

/// Format the "no config file found" error the CLI shows when the
/// discovery chain came up empty AND the operator needs a config
/// (i.e. the web server is going to start, since `--no-web` runs
/// happily on built-in defaults). Actionable: names the paths that
/// were searched + the exact commands to fix it.
pub fn no_config_error_message(searched: &[PathBuf]) -> String {
    let paths = searched
        .iter()
        .map(|p| format!("  * {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "no configuration file found.\n\
         \n\
         raqib searched (in order):\n\
         {paths}\n\
         \n\
         To create a starter config with safe defaults:\n\
         \n\
         \u{20}\u{20}raqib init\n\
         \n\
         To run without the web dashboard (no config required):\n\
         \n\
         \u{20}\u{20}raqib --no-web\n\
         \n\
         Or pass an explicit path:\n\
         \n\
         \u{20}\u{20}raqib --config <path>",
    )
}

/// The starter config `raqib init` writes. SAFE-OFF posture:
/// governor observer-only, web allows unauthenticated local access
/// with a LOUD comment about it. Heavily commented so a new user can
/// read + edit.
///
/// The template must PARSE + VALIDATE cleanly against the current
/// `Config` schema — pinned by
/// [`tests::default_starter_config_parses_and_validates`].
///
/// AUTH DECISION: `allow_no_auth = true` (option a per dispatch). See
/// module-level doc for the trade-off rationale.
pub const DEFAULT_CONFIG_TOML: &str = r#"# raqib starter configuration
#
# This file was generated by `raqib init`. It ships SAFE
# defaults: the auto-kill governor is OFF, the web dashboard is
# open on localhost with NO authentication. Everything below is
# commented — edit only the fields you want to override, then
# restart raqib.

# ─────────────────────────────────────────────────────────────────
# [web] Dashboard listener + authentication
# ─────────────────────────────────────────────────────────────────
[web]

# ⚠ FIRST-RUN DEFAULT: NO AUTHENTICATION on a LOCALHOST-only bind.
#
# The `--bind` CLI flag defaults to `127.0.0.1`, so the dashboard
# is only reachable from processes on THIS host. In that mode
# `allow_no_auth = true` is FINE — the port is not on the network.
#
# If you pass `--bind 0.0.0.0` (or a specific LAN IP) to expose
# the dashboard to other hosts, `allow_no_auth = true` becomes
# DANGEROUS: anyone who can reach the port gets full read access
# to your workloads, thermal state, and settings. A loud startup
# WARN fires in that case.
#
# For LAN / remote use, set an auth_token instead:
#     allow_no_auth = false
#     auth_token    = "long-random-string-here"
# Clients then send `Authorization: Bearer <token>` on every
# request. The token is never echoed in logs or error messages.
allow_no_auth = true

# Bearer token clients must present when `allow_no_auth = false`.
# Kept empty here so the fresh install runs; fill it in when you
# flip `allow_no_auth = false`.
# auth_token = ""

# ─────────────────────────────────────────────────────────────────
# [governor] Auto-kill actuation (SAFETY-CRITICAL)
# ─────────────────────────────────────────────────────────────────
[governor]

# The auto-kill governor is OFF by default. When false, the
# monitor observes + records everything but NEVER sends SIGTERM
# / SIGKILL on its own. This is the safe posture for a fresh
# install.
#
# Setting this to `true` is a DELIBERATE opt-in — the monitor
# will start sending real signals to processes that breach the
# [thresholds] below. Before flipping, read the safety notes in
# `docs/state/PENDING.md` ("Governor kill-target selection")
# and verify your [policy] allowlist covers every workload you
# care about (setting `default_ai_action = "Allow"` — the
# default — makes the killer a no-op even when armed).
auto_actuate = false

# Seconds a breach must persist before the actuation site fires
# a SIGTERM. Only relevant when `auto_actuate = true`.
kill_sustain_secs = 10

# ─────────────────────────────────────────────────────────────────
# [policy] Which processes the governor may target
# ─────────────────────────────────────────────────────────────────
[policy]

# Default action for AI-classified processes that match neither
# `allowlist` nor `blocklist`. "Allow" (default) means: even if
# `auto_actuate = true` above, unlisted AI processes are NEVER
# killed. Flip to "Kill" only when you understand which processes
# it will target — the governor iterates EVERY AI-classified PID
# and kills every one that breaches VRAM / RAM / host thermal
# (ordered lowest-PID-first if the rate-limit budget forces a
# subset). See docs/state/PENDING.md for the full selection logic.
default_ai_action = "Allow"

# Process names (from /proc/<pid>/comm, so max 15 chars) that
# the governor NEVER kills. The defaults cover shells and init;
# add your own workloads here (e.g. "ros2", "claude") if you
# ever flip `auto_actuate = true`.
allowlist = [
    "systemd", "init", "sshd", "bash", "zsh", "sh",
    "kworker", "kthreadd",
]

# Process names the governor ALWAYS treats as kill candidates
# (breach still required — a blocklisted process without any
# breach won't be killed). Useful for scripted test runs where
# you want a specific disposable target caught even if the
# classifier picks a different category.
blocklist = []

# Seconds between SIGTERM and SIGKILL escalation. Minimum 1.
sigterm_grace_secs = 5

# Rate limit: at most N automated kills per M-second window.
# CLAUDE.md safety rule 5.
rate_limit_max_kills   = 3
rate_limit_window_secs = 60

# ─────────────────────────────────────────────────────────────────
# Everything else uses built-in defaults. See
# `raqib.toml.example` in the repo for the full menu of tunables
# ([runtime], [storage], [regression], [telemetry], [thresholds],
# [[workloads]]).
# ─────────────────────────────────────────────────────────────────
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_search_paths_includes_cwd_and_xdg_when_home_set() {
        // Uses the pure `_with_home` variant so we don't race on the
        // process-wide `$HOME` env var (which other tests may
        // mutate in parallel test runs).
        let paths = config_search_paths_with_home(Some("/tmp/fake-home"));
        // CWD first (highest priority after --config).
        assert_eq!(paths[0], PathBuf::from("./raqib.toml"));
        // XDG standard location second.
        assert_eq!(
            paths[1],
            PathBuf::from("/tmp/fake-home/.config/raqib/raqib.toml"),
        );
        // System-wide fallback last.
        assert_eq!(
            paths.last(),
            Some(&PathBuf::from("/etc/raqib/raqib.toml")),
        );
    }

    #[test]
    fn config_search_paths_skips_xdg_when_no_home() {
        let paths = config_search_paths_with_home(None);
        // CWD + system-wide only; XDG entry omitted (a fresh user
        // with `$HOME` unset falls through to CWD then system-wide).
        assert!(paths.iter().any(|p| p == Path::new("./raqib.toml")));
        assert!(paths.iter().all(|p| !p.to_string_lossy().contains(".config/raqib")));
        assert!(paths.contains(&PathBuf::from("/etc/raqib/raqib.toml")));
    }

    /// raqib rename — legacy fallback paths still enumerate the old
    /// `edge_monitor.toml` locations so `Runtime::load_config` can
    /// try them AFTER the raqib set misses. Callers that hit a
    /// legacy path emit a `tracing::warn!` deprecation note. One
    /// release only.
    #[test]
    fn legacy_config_search_paths_still_list_edge_monitor_locations() {
        let paths = legacy_config_search_paths_with_home(Some("/tmp/fake-home"));
        assert_eq!(paths[0], PathBuf::from("./edge_monitor.toml"));
        assert_eq!(
            paths[1],
            PathBuf::from("/tmp/fake-home/.config/edge_monitor/edge_monitor.toml"),
        );
        assert_eq!(
            paths.last(),
            Some(&PathBuf::from("/etc/edge_monitor/edge_monitor.toml")),
        );
    }

    #[test]
    fn legacy_config_search_paths_skip_xdg_when_no_home() {
        let paths = legacy_config_search_paths_with_home(None);
        assert!(paths.iter().any(|p| p == Path::new("./edge_monitor.toml")));
        assert!(paths.iter().all(|p| !p.to_string_lossy().contains(".config/edge_monitor")));
    }

    /// raqib rename — the primary + legacy search sets must be
    /// DISJOINT. If the two ever start returning the same path,
    /// the caller's "found in raqib path → skip fallback" gate
    /// would fire in the wrong branch.
    #[test]
    fn raqib_and_legacy_search_paths_are_disjoint() {
        let primary = config_search_paths_with_home(Some("/tmp/fake-home"));
        let legacy = legacy_config_search_paths_with_home(Some("/tmp/fake-home"));
        for p in &primary {
            assert!(!legacy.contains(p), "raqib path {p:?} must not appear in the legacy fallback list");
        }
    }

    #[test]
    fn default_starter_config_parses_and_validates() {
        // THE load-bearing test — the template `raqib init`
        // writes must parse against the current schema AND pass
        // `Config::validate` + `validate_web_auth`. If a schema field
        // is added or renamed without updating the template, this
        // fires immediately.
        let cfg: crate::config::Config =
            toml::from_str(DEFAULT_CONFIG_TOML).expect("starter config parses as TOML");
        cfg.validate().expect("starter config validates");
        cfg.validate_web_auth()
            .expect("starter config passes web-auth posture (allow_no_auth = true)");
    }

    #[test]
    fn default_starter_config_ships_governor_off() {
        // Safe-off posture pin: auto_actuate=false AND
        // default_ai_action="Allow" — both required for the killer
        // to be inert on a fresh install. A future well-meaning
        // edit that flips either to on breaks this.
        let cfg: crate::config::Config =
            toml::from_str(DEFAULT_CONFIG_TOML).expect("parses");
        assert!(
            !cfg.governor.auto_actuate,
            "starter config MUST have governor.auto_actuate = false (safe-off)",
        );
        assert_eq!(
            cfg.policy.default_ai_action,
            crate::governor::policy::PolicyAction::Allow,
            "starter config MUST have policy.default_ai_action = Allow (safe-off)",
        );
    }

    #[test]
    fn default_starter_config_ships_web_open_on_localhost() {
        // The dispatch's option (a) decision pin: allow_no_auth =
        // true so the fresh install opens the dashboard without a
        // token. A well-meaning tightening to "false" would break
        // the first-run experience described in the module doc.
        let cfg: crate::config::Config =
            toml::from_str(DEFAULT_CONFIG_TOML).expect("parses");
        assert!(
            cfg.web.allow_no_auth,
            "starter config MUST have web.allow_no_auth = true (first-run friendliness); \
             see onboarding.rs module doc for the trade-off rationale",
        );
    }

    #[test]
    fn write_starter_config_creates_dir_and_writes_bytes() {
        let tmp = std::env::temp_dir()
            .join(format!("raqib_onboarding_test_write_{}", std::process::id()));
        let target = tmp.join("nested/raqib.toml");
        // Ensure clean state.
        let _ = std::fs::remove_dir_all(&tmp);

        let outcome = write_starter_config(&target, false).expect("write succeeds");
        assert_eq!(outcome, InitOutcome::Wrote(target.clone()));
        let written = std::fs::read_to_string(&target).expect("read back");
        assert_eq!(written, DEFAULT_CONFIG_TOML);
        // Nested parent dir was created.
        assert!(target.parent().unwrap().is_dir());

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_starter_config_refuses_existing_without_force() {
        let tmp = std::env::temp_dir()
            .join(format!("raqib_onboarding_test_refuse_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("raqib.toml");
        std::fs::write(&target, "PREEXISTING_CONTENT").unwrap();

        let outcome = write_starter_config(&target, false).expect("returns Ok(RefusedExisting)");
        assert_eq!(outcome, InitOutcome::RefusedExisting(target.clone()));
        // Content unchanged — the operator's file is intact.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "PREEXISTING_CONTENT");

        // With --force, the file IS overwritten.
        let outcome_forced =
            write_starter_config(&target, true).expect("force succeeds");
        assert_eq!(outcome_forced, InitOutcome::Wrote(target.clone()));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), DEFAULT_CONFIG_TOML);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_config_error_lists_searched_paths_and_actionable_commands() {
        let searched = vec![
            PathBuf::from("./raqib.toml"),
            PathBuf::from("/tmp/fake-home/.config/raqib/raqib.toml"),
        ];
        let msg = no_config_error_message(&searched);
        // Every searched path appears verbatim in the message.
        for p in &searched {
            assert!(msg.contains(&p.display().to_string()),
                "message must list searched path {}: {msg}", p.display());
        }
        // The two ways forward are named literally.
        assert!(msg.contains("raqib init"),
            "message must recommend `raqib init`: {msg}");
        assert!(msg.contains("raqib --no-web"),
            "message must offer `--no-web` alternative: {msg}");
        assert!(msg.contains("raqib --config"),
            "message must mention explicit --config path: {msg}");
        // No jargon: the pre-D85 language must NOT appear (that
        // belonged to the auth-existing-config error, not this one).
        assert!(!msg.contains("D85") && !msg.contains("pre-D85"),
            "no-config error must not reference D85 jargon: {msg}");
    }
}
