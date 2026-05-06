//! [UX-3] — `g` keybinding integration tests.
//!
//! Pins the substitution layer (pure function) AND the UI Contract
//! v2 URL-source priority order (config → env → hardcoded fallback).
//! The browser-open side effect is intentionally NOT exercised — it
//! would either require a running browser (flaky in CI) or a mock
//! that adds little value over what the unit tests already pin.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edge_monitor::config::Config;
use edge_monitor::ui::app::{App, Dispatch};
use edge_monitor::ui::input::translate;
use edge_monitor::ui::{compute_dashboard_url, resolve_dashboard_template};
use ux_contract::Action;

#[test]
fn substitutes_model_and_pid_into_template() {
    let url = compute_dashboard_url(
        "http://example.test/{model}?pid={pid}",
        Some("phi3-mini"),
        4242,
    );
    assert_eq!(url, "http://example.test/phi3-mini?pid=4242");
}

#[test]
fn empty_model_substitutes_as_empty_string_not_dash() {
    // UI Contract: when `model_name` is None we substitute empty,
    // not "-" or "unknown". Templates that want a fallback can
    // write `var-model={model}` and rely on Grafana to handle the
    // empty value.
    let url = compute_dashboard_url(
        "http://example.test/d?var-model={model}&var-pid={pid}",
        None,
        99,
    );
    assert_eq!(url, "http://example.test/d?var-model=&var-pid=99");
}

#[test]
fn template_without_substitution_tokens_is_passed_through() {
    let url = compute_dashboard_url("http://example.test/overview", Some("phi3"), 1);
    assert_eq!(url, "http://example.test/overview");
}

#[test]
fn pid_substitutes_as_decimal_integer_no_leading_zeros() {
    let url = compute_dashboard_url("p={pid}", None, 7);
    assert_eq!(url, "p=7");
}

#[test]
fn template_with_unrelated_curly_brace_tokens_is_left_alone() {
    let url = compute_dashboard_url("http://example.test/d/{board}?pid={pid}", None, 1);
    assert_eq!(url, "http://example.test/d/{board}?pid=1");
}

/// UI Contract v2 — config takes precedence over the env var even
/// when the env var is also set.
#[test]
fn url_priority_config_beats_env_var() {
    let mut config = Config::default();
    config.dashboard.url_template = "http://from-config/{model}".into();
    // Set an env var that should be ignored because config wins.
    // Use a unique name so we can clean up without racing.
    let var = "EDGE_MONITOR_GRAFANA_URL";
    let prev = std::env::var(var).ok();
    // SAFETY: process-wide; isolated by the unique var name + restore
    // at end of test. CI runs cargo test single-threaded for env-var
    // sensitive tests via the harness, but we still restore to be
    // robust to parallel runs.
    unsafe {
        std::env::set_var(var, "http://from-env/never-used");
    }
    let template = resolve_dashboard_template(&config);
    if let Some(p) = prev {
        unsafe { std::env::set_var(var, p) };
    } else {
        unsafe { std::env::remove_var(var) };
    }
    assert_eq!(template, "http://from-config/{model}");
}

/// UI Contract v2 — env var fills in when config is empty.
#[test]
fn url_priority_env_var_used_when_config_empty() {
    let config = Config::default(); // url_template = ""
    let var = "EDGE_MONITOR_GRAFANA_URL";
    let prev = std::env::var(var).ok();
    unsafe {
        std::env::set_var(var, "http://from-env/d/edge");
    }
    let template = resolve_dashboard_template(&config);
    if let Some(p) = prev {
        unsafe { std::env::set_var(var, p) };
    } else {
        unsafe { std::env::remove_var(var) };
    }
    assert_eq!(template, "http://from-env/d/edge");
}

/// UI Contract v2 — when both config and env are empty, the
/// hardcoded fallback fires so `g` still does *something* useful.
#[test]
fn url_priority_hardcoded_fallback_when_both_empty() {
    let config = Config::default();
    let var = "EDGE_MONITOR_GRAFANA_URL";
    let prev = std::env::var(var).ok();
    unsafe {
        std::env::remove_var(var);
    }
    let template = resolve_dashboard_template(&config);
    if let Some(p) = prev {
        unsafe { std::env::set_var(var, p) };
    }
    assert_eq!(template, "http://localhost:3000/d/edge_monitor");
}

/// L2a / UX_CONTRACT.md §6 — pressing `g` on the focused workload
/// must surface as `ux_contract::Action::OpenGrafana`. Pinned at the
/// integration-test level so the input-layer rename
/// (`OpenDashboard` → `OpenGrafana`) and the contract Action surface
/// stay locked together; future drift on either side fails this test.
#[test]
fn g_keybinding_emits_open_grafana_from_contract_enum() {
    let app = App::new();
    let key = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(
        translate(key, &app),
        Some(Dispatch::Contract(Action::OpenGrafana)),
    );
}
