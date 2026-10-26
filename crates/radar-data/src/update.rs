//! GitHub Releases update check + Android APK download.
//!
//! The app is sideloaded, so the update path is intentionally simple: ask the
//! public GitHub API for the newest release, compare its tag with the running
//! version, then download the `*-android.apk` asset. The Android shell hands
//! the file to the system package installer.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

const RELEASES_LATEST: &str = "https://api.github.com/repos/photon01666/stormscope/releases/latest";
const USER_AGENT: &str = concat!("stormscope/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub asset_url: String,
    pub page_url: String,
    pub notes: String,
}

/// Check GitHub for a newer release. `None` means the running version is
/// current. Errors are returned for the UI to show (offline, rate limit, …).
pub fn check_latest(current: &str) -> Result<Option<UpdateInfo>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(USER_AGENT)
        .build()
        .context("building the update client")?;
    let response = client
        .get(RELEASES_LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("contacting GitHub")?;
    if !response.status().is_success() {
        bail!("GitHub release check failed (HTTP {})", response.status());
    }
    let release: serde_json::Value = response.json().context("reading the release JSON")?;
    let tag = release
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let latest = tag.trim_start_matches('v');
    if latest.is_empty() || !is_newer(latest, current) {
        return Ok(None);
    }
    let assets = release
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let asset = assets
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.ends_with("-android.apk") || name.ends_with(".apk"))
        })
        .context("the latest release has no Android APK asset")?;
    let asset_url = asset
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)
        .context("the APK asset has no download URL")?
        .to_string();
    let page_url = release
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("https://github.com/photon01666/stormscope/releases")
        .to_string();
    let notes = release
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(Some(UpdateInfo {
        version: latest.to_string(),
        asset_url,
        page_url,
        notes,
    }))
}

/// Download an APK into the app cache and return its path. The ZIP magic check
/// stops an HTML error page or truncated file from being handed to the
/// installer.
pub fn download_apk(url: &str, version: &str) -> Result<PathBuf> {
    let dir = crate::cache::DiskCache::default_root().join("updates");
    std::fs::create_dir_all(&dir).context("creating the update directory")?;
    let path = dir.join(format!("stormscope-{version}-android.apk"));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(USER_AGENT)
        .build()
        .context("building the update download client")?;
    let response = client
        .get(url)
        .send()
        .context("downloading the Android APK")?;
    if !response.status().is_success() {
        bail!("APK download failed (HTTP {})", response.status());
    }
    let bytes = response.bytes().context("reading the APK")?;
    if bytes.len() < 1_000_000 || !bytes.starts_with(b"PK\x03\x04") {
        bail!("the downloaded file is not a valid APK");
    }
    std::fs::write(&path, &bytes).context("writing the APK to the cache")?;
    Ok(path)
}

/// Compare dotted numeric versions (`1.5.10` > `1.5.9`).
fn is_newer(candidate: &str, current: &str) -> bool {
    let a = version_parts(candidate);
    let b = version_parts(current);
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            part.split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering_is_numeric() {
        assert!(is_newer("1.5.10", "1.5.9"));
        assert!(is_newer("1.5.11", "1.5.10"));
        assert!(is_newer("1.6.0", "1.5.99"));
        assert!(!is_newer("1.5.11", "1.5.11"));
        assert!(!is_newer("1.5.9", "1.5.10"));
        assert!(!is_newer("v1.5.11-beta", "1.5.11"));
    }
}
