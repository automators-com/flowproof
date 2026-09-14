//! A best-effort "a newer version exists" notice, printed once per
//! invocation. Deliberately powerless to affect the command it rides
//! along with: any failure here (no network, no config dir, a bad
//! response) is swallowed silently, nothing here can slow a command down
//! by more than the network timeout below, and the notice always goes to
//! stderr so `--json` output and the `mcp-stdio` protocol stay clean.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASES_API: &str = "https://api.github.com/repos/automators-com/flowproof/releases/latest";
/// How long a cached "latest version" answer stays trusted before another
/// network call is worth making. The notice itself still prints on every
/// invocation while the cache is fresh — this bounds network calls, not
/// how often the user sees the message.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Short enough that a slow/unreachable network never makes an ordinary
/// command feel hung.
const HTTP_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Cache {
    checked_at_unix: u64,
    latest_version: String,
}

fn cache_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("flowproof")
            .join("update_check.json"),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache(path: &Path) -> Option<Cache> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(path: &Path, cache: &Cache) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, json);
    }
}

fn fetch_latest_version() -> Option<String> {
    let config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(HTTP_TIMEOUT))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(RELEASES_API)
        .header("User-Agent", "flowproof-cli")
        .call()
        .ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

/// Parse a strict `X.Y.Z` version into a comparable triple. Deliberately
/// tiny (no semver dep) — same reasoning as
/// `flowproof_agent::spec::parse_version_triple`, kept as a separate copy
/// here rather than a cross-crate dependency for one three-line parser.
fn parse_version_triple(v: &str) -> Option<(u64, u64, u64)> {
    match v.split('.').collect::<Vec<_>>().as_slice() {
        [a, b, c] => Some((a.parse().ok()?, b.parse().ok()?, c.parse().ok()?)),
        _ => None,
    }
}

/// Check (using a cached answer when it's still fresh) whether a newer
/// flowproof release exists, and print a one-line notice to stderr if so.
/// Opt out with `FLOWPROOF_NO_UPDATE_CHECK` (any value) — useful for CI and
/// air-gapped environments where the network call would only ever fail.
pub fn notify_if_outdated(current_version: &str) {
    if std::env::var_os("FLOWPROOF_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let Some(path) = cache_path() else {
        return;
    };
    let cached = read_cache(&path);
    let now = now_unix();
    let fresh = cached
        .as_ref()
        .is_some_and(|c| now.saturating_sub(c.checked_at_unix) < CHECK_INTERVAL.as_secs());

    let latest = if fresh {
        cached.map(|c| c.latest_version)
    } else {
        match fetch_latest_version() {
            Some(v) => {
                write_cache(
                    &path,
                    &Cache {
                        checked_at_unix: now,
                        latest_version: v.clone(),
                    },
                );
                Some(v)
            }
            // Network failed — fall back to a stale cache rather than
            // going silent, if one exists.
            None => cached.map(|c| c.latest_version),
        }
    };

    let Some(latest) = latest else {
        return;
    };
    let (Some(current_t), Some(latest_t)) = (
        parse_version_triple(current_version),
        parse_version_triple(&latest),
    ) else {
        return;
    };
    if latest_t > current_t {
        print_notice(current_version, &latest);
    }
}

/// True when stderr is an actual terminal and the user hasn't opted out of
/// color via the `NO_COLOR` convention (https://no-color.org) — a piped or
/// redirected stderr (a log file, a CI artifact) gets the plain, colorless
/// form so nothing downstream has to strip ANSI codes.
fn use_color() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn print_notice(current_version: &str, latest: &str) {
    let (y, b, r) = if use_color() {
        (YELLOW, BOLD, RESET)
    } else {
        ("", "", "")
    };
    let _ = writeln!(
        std::io::stderr(),
        "\n{y}Update available!{r} flowproof {current_version} -> {y}{latest}{r}\n\
         \x20 pip:  {b}pip install --upgrade flowproof{r}\n\
         \x20 npm:  {b}npm install --save-dev flowproof@latest{r}\n\
         \x20 see:  https://github.com/automators-com/flowproof/releases/tag/v{latest}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_triple_compares_numerically_not_lexically() {
        // A lexical compare would get "0.9.0" < "0.10.0" wrong; every
        // version here is a real release-shaped triple.
        assert!(parse_version_triple("0.10.0") > parse_version_triple("0.9.0"));
        assert!(parse_version_triple("0.21.0") > parse_version_triple("0.20.0"));
        assert_eq!(
            parse_version_triple("0.21.0"),
            parse_version_triple("0.21.0")
        );
    }

    #[test]
    fn malformed_versions_are_rejected_not_guessed_at() {
        assert_eq!(parse_version_triple("v0.21.0"), None);
        assert_eq!(parse_version_triple("0.21"), None);
        assert_eq!(parse_version_triple("latest"), None);
        assert_eq!(parse_version_triple(""), None);
    }

    #[test]
    fn opt_out_env_var_skips_everything_without_touching_the_network() {
        // SAFETY: test-only env mutation, restored immediately after; no
        // other test in this crate reads FLOWPROOF_NO_UPDATE_CHECK.
        std::env::set_var("FLOWPROOF_NO_UPDATE_CHECK", "1");
        notify_if_outdated("0.0.1"); // would otherwise always be "outdated"
        std::env::remove_var("FLOWPROOF_NO_UPDATE_CHECK");
    }

    #[test]
    fn cache_round_trips_through_json() {
        let dir = std::env::temp_dir().join("flowproof-update-check-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("update_check.json");
        let cache = Cache {
            checked_at_unix: 12345,
            latest_version: "9.9.9".to_string(),
        };
        write_cache(&path, &cache);
        let read_back = read_cache(&path).expect("cache reads back");
        assert_eq!(read_back.checked_at_unix, 12345);
        assert_eq!(read_back.latest_version, "9.9.9");
        std::fs::remove_dir_all(&dir).ok();
    }
}
