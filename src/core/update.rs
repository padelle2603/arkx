//! "Is there a newer Arkx?" — one anonymous HEAD request at startup.
//!
//! Privacy: this is the only network call Arkx makes, ever. It hits
//! `github.com/<repo>/releases/latest`, which answers `302` with the tag in
//! the `Location` header and an empty body. No GitHub API (no token, no JSON,
//! no rate-limit budget shared with other apps), no cookies, no redirect
//! following, nothing written to disk, nothing sent back. The User-Agent is the
//! bare project name: no version, no distro, no hostname.
//!
//! Every failure — offline, DNS, TLS, HTTP error — is silent by design. A
//! user with no network must not see an error about an optional feature.

use std::time::Duration;

/// Latest-release endpoint. Deliberately not `api.github.com`: the API is
/// rate-limited to 60/h per IP, this is not, and we need one header from it.
const LATEST_URL: &str = "https://github.com/padelle2603/arkx/releases/latest";

/// Where the "Download" button sends the user.
pub const RELEASES_URL: &str = "https://github.com/padelle2603/arkx/releases";

const TIMEOUT: Duration = Duration::from_secs(5);

/// Split `v1.2.3-rc1` into comparable tokens: `["1", "2", "3", "rc", "1"]`.
fn tokens(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '_', '-', '+'])
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

/// `true` when `remote` is strictly newer than `local`.
///
/// ponytail: length-first token compare, not a full semver implementation.
/// `1.6.1` loses to `1.6.2`; a dev build that is *ahead* of the newest release
/// (`1.7.0` vs `1.6.1`) reports no update. Replace with the `semver` crate if
/// pre-release ordering ever matters.
fn remote_is_newer(local: &str, remote: &str) -> bool {
    let (a, b) = (tokens(local), tokens(remote));
    for (x, y) in a.iter().zip(b.iter()) {
        match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) if nx != ny => return ny > nx,
            (Ok(_), Err(_)) => return false, // 1.2.3 < 1.2.rc
            (Err(_), Ok(_)) => return true,
            _ => {}
        }
        if x != y {
            return y > x;
        }
    }
    b.len() > a.len()
}

/// The tag of the newest published release, or `None` when there is none.
///
/// A repository with zero releases answers `302 → /releases` with no tag in
/// the path. That is not an error, it just means there is nothing to offer.
pub fn latest_tag() -> Option<String> {
    let agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .build()
        .new_agent();

    let res = agent
        .head(LATEST_URL)
        .header("user-agent", "arkx")
        .call()
        .ok()?;

    let location = res.headers().get("location")?.to_str().ok()?;
    let tag = location.rsplit('/').next()?.trim();
    if tag.is_empty() || tag == "releases" || tag == "latest" {
        return None;
    }
    Some(tag.trim_start_matches(['v', 'V']).to_string())
}

/// `true` when `remote` is newer than the running build.
pub fn is_available(remote: &str) -> bool {
    remote_is_newer(env!("CARGO_PKG_VERSION"), remote)
}

/// The version to advertise in the banner, or `None` when the app is current,
/// offline, or has no release to compare against.
pub fn available_version() -> Option<String> {
    let remote = latest_tag()?;
    is_available(&remote).then_some(remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_remote_wins() {
        assert!(remote_is_newer("1.6.1", "1.6.2"));
        assert!(remote_is_newer("1.6.9", "1.7.0"));
        assert!(remote_is_newer("1.6.1", "2.0.0"));
        assert!(remote_is_newer("v1.6.1", "v1.6.2"));
    }

    #[test]
    fn current_and_ahead_report_nothing() {
        assert!(!remote_is_newer("1.6.2", "1.6.2"));
        assert!(!remote_is_newer("1.7.0", "1.6.2"));
        assert!(!remote_is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn longer_version_is_newer() {
        assert!(remote_is_newer("1.6", "1.6.1"));
        assert!(!remote_is_newer("1.6.1", "1.6"));
    }

    #[test]
    fn tags_lose_their_v_prefix() {
        assert_eq!(tokens("v1.6.1"), ["1", "6", "1"]);
        assert_eq!(tokens("1.6.1"), ["1", "6", "1"]);
    }
}
