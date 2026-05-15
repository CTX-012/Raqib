//! Grafana preflight TCP probe (WP5 — Linux parity with Windows).
//!
//! Operator presses `g` → before we spawn `xdg-open <url>` we open a
//! short TCP connection to the (host, port) of that URL. If the probe
//! fails we surface `ux_contract::status::GRAFANA_UNREACHABLE` and skip
//! the spawn. Without the probe a stopped Grafana plus `xdg-open`
//! produces a generic "browser failed to open this page" error
//! downstream — indistinguishable to the operator from "the keybinding
//! is broken". The probe converts that into an actionable message.
//!
//! Stdlib-only on purpose: a one-shot 500ms blocking probe doesn't
//! justify pulling tokio onto the UI thread, and `std::net::TcpStream::
//! connect_timeout` is exactly the primitive we need. The probe runs
//! on the keypress thread, not the 10 Hz render tick, so a sub-second
//! block is fine — 500ms is below the ~600ms perceived-instant
//! threshold the UI contract uses elsewhere.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Default probe timeout — WP5 implementation rule (mirrors Windows side).
/// Kept under the perceived-instant threshold so the operator does not
/// feel the keypress hang on the way to the browser.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("address resolution failed for {host}: {source}")]
    Resolve {
        host: String,
        #[source]
        source: std::io::Error,
    },
    #[error("unreachable: {0}")]
    Unreachable(String),
}

/// Probe `url` with [`DEFAULT_PROBE_TIMEOUT`]. Returns `Ok(())` if a TCP
/// connection to the URL's (host, port) succeeds within the timeout.
pub fn probe(url: &str) -> Result<(), PreflightError> {
    probe_with_timeout(url, DEFAULT_PROBE_TIMEOUT)
}

/// Probe `url` with an explicit `timeout`. Exposed so tests can use a
/// shorter timeout without waiting the full 500ms on negative cases.
pub fn probe_with_timeout(url: &str, timeout: Duration) -> Result<(), PreflightError> {
    let (host, port) = parse_host_port(url)?;
    let addr_str = format!("{host}:{port}");
    let addrs = addr_str
        .to_socket_addrs()
        .map_err(|source| PreflightError::Resolve {
            host: host.clone(),
            source,
        })?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(PreflightError::Unreachable(format!(
            "no addresses resolved for {host}"
        )));
    }
    // Walk every resolved address (IPv4 + IPv6) and accept the first
    // that connects. Only surface "unreachable" once every candidate
    // fails — matches what a browser would do.
    let mut last_err: Option<std::io::Error> = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(PreflightError::Unreachable(format!(
        "{host}:{port} ({})",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "no candidate addresses".to_string())
    )))
}

/// Pure URL → (host, port) extractor. Accepts `http://` and `https://`
/// only — the dashboard URLs we open are always one of those, and
/// rejecting other schemes early gives a clearer error than letting
/// resolution fail downstream.
pub fn parse_host_port(url: &str) -> Result<(String, u16), PreflightError> {
    if url.is_empty() {
        return Err(PreflightError::InvalidUrl("empty URL".to_string()));
    }
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest)
    } else {
        return Err(PreflightError::InvalidUrl(format!(
            "missing http:// or https:// scheme: {url}"
        )));
    };
    // The authority is everything up to the first `/`, `?`, or `#`.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(PreflightError::InvalidUrl(format!("no host in URL: {url}")));
    }
    // Strip userinfo (`user:pass@host`) — we never use it for probing.
    let authority = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    // IPv6 literal: `[::1]` or `[::1]:3000`. Handled separately so the
    // colons inside the literal don't get mistaken for a port separator.
    if let Some(rest_after_bracket) = authority.strip_prefix('[') {
        let close = rest_after_bracket.find(']').ok_or_else(|| {
            PreflightError::InvalidUrl(format!("unterminated IPv6 literal: {url}"))
        })?;
        let host = &rest_after_bracket[..close];
        let after = &rest_after_bracket[close + 1..];
        let port = if let Some(p) = after.strip_prefix(':') {
            p.parse::<u16>().map_err(|_| {
                PreflightError::InvalidUrl(format!("invalid port in URL: {url}"))
            })?
        } else if after.is_empty() {
            default_port(scheme)
        } else {
            return Err(PreflightError::InvalidUrl(format!(
                "trailing junk after IPv6 literal: {url}"
            )));
        };
        return Ok((host.to_string(), port));
    }
    // hostname[:port]
    match authority.rsplit_once(':') {
        Some((host, port_str)) => {
            if host.is_empty() {
                return Err(PreflightError::InvalidUrl(format!("no host in URL: {url}")));
            }
            let port = port_str.parse::<u16>().map_err(|_| {
                PreflightError::InvalidUrl(format!("invalid port in URL: {url}"))
            })?;
            Ok((host.to_string(), port))
        }
        None => Ok((authority.to_string(), default_port(scheme))),
    }
}

fn default_port(scheme: &str) -> u16 {
    // `parse_host_port` only emits "http" or "https" here, but be
    // defensive — an unknown scheme should fall back to 80 rather than
    // panic.
    match scheme {
        "https" => 443,
        _ => 80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    #[test]
    fn default_probe_timeout_is_500ms() {
        assert_eq!(DEFAULT_PROBE_TIMEOUT, Duration::from_millis(500));
    }

    #[test]
    fn parse_host_port_http_with_explicit_port() {
        let (host, port) = parse_host_port("http://localhost:3000/d/edge_monitor").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 3000);
    }

    #[test]
    fn parse_host_port_http_defaults_port_80() {
        let (host, port) = parse_host_port("http://example.test/path").unwrap();
        assert_eq!(host, "example.test");
        assert_eq!(port, 80);
    }

    #[test]
    fn parse_host_port_https_defaults_port_443() {
        let (host, port) = parse_host_port("https://example.test/path").unwrap();
        assert_eq!(host, "example.test");
        assert_eq!(port, 443);
    }

    #[test]
    fn parse_host_port_strips_query_and_fragment() {
        let (host, port) =
            parse_host_port("http://localhost:3000/d/edge?var-pid=42#anchor").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 3000);
    }

    #[test]
    fn parse_host_port_rejects_empty_url() {
        let err = parse_host_port("").unwrap_err();
        assert!(matches!(err, PreflightError::InvalidUrl(_)));
    }

    #[test]
    fn parse_host_port_rejects_missing_scheme() {
        let err = parse_host_port("localhost:3000/d").unwrap_err();
        assert!(matches!(err, PreflightError::InvalidUrl(_)));
    }

    #[test]
    fn parse_host_port_rejects_invalid_port() {
        let err = parse_host_port("http://localhost:not-a-port/").unwrap_err();
        assert!(matches!(err, PreflightError::InvalidUrl(_)));
    }

    #[test]
    fn parse_host_port_ipv6_literal_with_port() {
        let (host, port) = parse_host_port("http://[::1]:3000/d/edge_monitor").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 3000);
    }

    /// Bind an ephemeral port, then probe it — must succeed within the
    /// timeout. Catches "we forgot to plumb host/port" regressions.
    #[test]
    fn probe_succeeds_against_listening_port() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        let url = format!("http://{}:{}/d/edge_monitor", addr.ip(), addr.port());
        probe_with_timeout(&url, Duration::from_millis(500)).expect("probe should reach listener");
    }

    /// Bind, capture addr, drop the listener so the port is closed —
    /// then probe must error and must NOT exceed the timeout budget by
    /// more than a generous slack (CI machines are noisy). Catches
    /// regressions where the timeout doesn't actually apply.
    #[test]
    fn probe_errors_within_timeout_on_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let url = format!("http://{}:{}/", addr.ip(), addr.port());
        let timeout = Duration::from_millis(500);
        let start = Instant::now();
        let result = probe_with_timeout(&url, timeout);
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected probe to fail on closed port");
        // 5x slack — connect_timeout should refuse fast (RST), but the
        // kernel and CI both add jitter. The cap exists to catch a
        // regression where the timeout isn't applied at all, not to
        // pin tight wall-clock behaviour.
        assert!(
            elapsed < timeout * 5,
            "probe took {elapsed:?}, way over budget"
        );
    }
}
