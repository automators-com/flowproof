//! Fiori reachability: an unauthenticated HTTP probe against a launchpad
//! URL. Stage 1 of `flowproof doctor --fiori`
//! (plans/002-sap-fiori-doctor.md) — cheap enough to run before anything
//! that needs a real browser, and it needs no credentials at all.
//!
//! Same `ureq` idiom already used at the agent boundary
//! (`agent_proxy.rs`'s `upstream_agent`, `mcp_http.rs`'s forwarders):
//! `http_status_as_error(false)` so a 404/500 becomes data to report
//! rather than a transport `Err`, because the doctor's job is to observe,
//! not to judge.

use std::time::{Duration, Instant};

use ureq::ResponseExt;

/// A local network round-trip or a DNS/ROT-style lookup should never
/// legitimately take longer than this — matches the reasoning in
/// plans/002-sap-fiori-doctor.md's "Timeout defaults": this stage self-limits
/// well under `doctor`'s shared `--timeout`, which exists for the one check
/// (Stage 2, a real browser login) that actually needs it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// What Stage 1 observed. `status`/`final_url` are `None` only when the
/// request itself never completed (DNS, TLS, connect, timeout) — `error`
/// names why. A non-2xx status is still `Some` and not an error: a 404 or a
/// redirect to an SSO login is exactly the kind of fact this check exists to
/// surface, not a failure of the probe itself.
#[derive(Debug, Clone)]
pub struct FioriReachability {
    pub status: Option<u16>,
    /// The URL actually reached after following redirects — same host as
    /// the request unless something redirected it, e.g. off to an external
    /// SSO provider (plans/002-sap-fiori-doctor.md, "The Fiori check").
    pub final_url: Option<String>,
    pub elapsed: Duration,
    pub error: Option<String>,
}

/// GET `url` once, reporting what came back rather than judging it. No
/// retries: a transient blip is itself worth reporting, not hidden by a
/// silent second attempt.
pub fn fiori_reachability(url: &str) -> FioriReachability {
    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TOTAL_TIMEOUT))
        .build();
    let agent = config.new_agent();

    let started = Instant::now();
    match agent.get(url).call() {
        Ok(response) => FioriReachability {
            status: Some(response.status().as_u16()),
            final_url: Some(response.get_uri().to_string()),
            elapsed: started.elapsed(),
            error: None,
        },
        Err(e) => FioriReachability {
            status: None,
            final_url: None,
            elapsed: started.elapsed(),
            error: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A one-shot HTTP server: replies with `response` to exactly one
    /// connection, then stops. Enough to test success/error-status/redirect
    /// without any real network access, matching the plan's own call for
    /// `tiny_http`-fixture-style tests (`flowproof-cli/tests` already uses
    /// `tiny_http` this way) — this crate has no `tiny_http` dev-dependency,
    /// so a bare `TcpListener` gets the same no-network-needed property with
    /// no new dependency.
    fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                // Drain the request line and headers; content doesn't matter.
                while reader.read_line(&mut line).unwrap_or(0) > 2 {
                    line.clear();
                }
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn a_200_reports_status_and_no_error() {
        let base = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        let result = fiori_reachability(&base);
        assert_eq!(result.status, Some(200));
        // ureq's `Uri` normalizes a bare authority to carry an explicit `/`
        // path - same origin, not a redirect, so trim it before comparing.
        assert_eq!(
            result.final_url.as_deref().map(|u| u.trim_end_matches('/')),
            Some(base.trim_end_matches('/'))
        );
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    #[test]
    fn a_404_is_reported_not_treated_as_a_transport_error() {
        let base = serve_once("HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
        let result = fiori_reachability(&base);
        assert_eq!(result.status, Some(404));
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    #[test]
    fn an_unreachable_host_reports_no_status_and_names_the_error() {
        // Port 0 never accepts a real connection; nothing is listening on
        // this address once the OS reclaims it.
        let result = fiori_reachability("http://127.0.0.1:1");
        assert_eq!(result.status, None);
        assert!(result.final_url.is_none());
        assert!(result.error.is_some());
    }
}
