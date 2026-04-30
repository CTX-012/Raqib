//! [UX-3] — `g` keybinding integration tests.
//!
//! Pins the substitution layer (pure function). The browser-open
//! side effect is intentionally NOT exercised — it would either
//! require a running browser (flaky in CI) or a mock that adds
//! little value over what the unit tests already pin.

use edge_monitor::ui::compute_dashboard_url;

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
