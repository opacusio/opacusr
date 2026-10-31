//! Update check: compare the running version against the latest published
//! GitHub release. Strictly informational: OpacusR never downloads or
//! installs anything on its own.
//!
//! Set the environment variable `OPACUSR_NO_UPDATE_CHECK=1` to disable
//! the check entirely (used by the helpers below).

use serde_json::Value;
use std::time::Duration;

const LATEST_URL: &str = "https://api.github.com/repos/opacusio/opacusr/releases/latest";

/// The latest published GitHub release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// Release tag, for example `v0.3.2`.
    pub tag: String,
    /// Tag without the leading `v`, for example `0.3.2`.
    pub version: String,
    /// Human-facing release page URL.
    pub url: String,
}

/// Fetch the latest published release from GitHub. Blocking call that times
/// out after 10 seconds; `/releases/latest` never returns drafts or
/// prereleases. All failures are reported as `Err(message)`, callers decide
/// how loudly to complain.
pub fn fetch_latest_release() -> Result<LatestRelease, String> {
    let resp = match ureq::get(LATEST_URL)
        .set("User-Agent", concat!("opacusr/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
        Err(e) => return Err(format!("request failed: {e}")),
    };
    let body = resp
        .into_string()
        .map_err(|e| format!("failed to read response body: {e}"))?;
    let json: Value = serde_json::from_str(&body).map_err(|e| format!("unexpected JSON: {e}"))?;
    let tag = json
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "response has no tag_name".to_string())?
        .trim()
        .to_string();
    let url = json
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("https://github.com/opacusio/opacusr/releases")
        .to_string();
    Ok(LatestRelease {
        version: tag.trim_start_matches('v').to_string(),
        tag,
        url,
    })
}

/// Compare dotted numeric versions. Returns true if `latest` is strictly
/// newer than `current`. Missing components count as zero, so `0.3` equals
/// `0.3.0`. Non-numeric suffixes like `-beta` are ignored.
pub fn is_newer(current: &str, latest: &str) -> bool {
    let cur = version_parts(current);
    let lat = version_parts(latest);
    let len = cur.len().max(lat.len());
    for i in 0..len {
        let c = cur.get(i).copied().unwrap_or(0);
        let l = lat.get(i).copied().unwrap_or(0);
        if l != c {
            return l > c;
        }
    }
    false
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

/// Check GitHub for a release newer than this build. Returns `None` when the
/// check fails (offline, rate limited) or when the running version is the
/// latest one. Blocking for up to 10 seconds: call from a background thread.
pub fn check_for_update() -> Option<LatestRelease> {
    let latest = fetch_latest_release().ok()?;
    if is_newer(env!("CARGO_PKG_VERSION"), &latest.version) {
        Some(latest)
    } else {
        None
    }
}

/// Fire-and-forget update notice for CLI binaries: runs
/// [`check_for_update`] on a background thread and prints one line to
/// stderr when a newer release exists. No download, no auto-install.
pub fn spawn_update_notice() {
    if std::env::var_os("OPACUSR_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("opacusr-update-check".into())
        .spawn(|| {
            if let Some(latest) = check_for_update() {
                eprintln!(
                    "opacusr: update available: {} (you are running {}) - {}",
                    latest.tag,
                    env!("CARGO_PKG_VERSION"),
                    latest.url
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions() {
        assert!(is_newer("0.3.1", "0.3.2"));
        assert!(is_newer("0.3.1", "0.4.0"));
        assert!(is_newer("0.9", "1.0.0"));
        assert!(!is_newer("0.3.2", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3"));
        assert!(!is_newer("0.3.1", "v0.3.1"));
    }

    #[test]
    fn parses_odd_versions() {
        assert_eq!(version_parts("v1.2.3-beta"), vec![1, 2, 3]);
        assert_eq!(version_parts("2"), vec![2]);
        assert_eq!(version_parts("x"), vec![0]);
    }
}
