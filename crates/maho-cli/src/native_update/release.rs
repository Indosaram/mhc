use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub asset_url: String,
    pub checksum_url: String,
    pub asset_name: String,
}

pub fn current_version() -> &'static str {
    option_env!("MHC_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

pub fn target() -> Result<&'static str, String> {
    target_from_platform()
}

pub fn target_from_str(s: &str) -> Result<&'static str, String> {
    match s.trim() {
        "linux-x86_64" => Ok("linux-x86_64"),
        "macos-aarch64" => Ok("macos-aarch64"),
        "windows-x86_64" => Ok("windows-x86_64"),
        other => Err(format!("unsupported target: {other}")),
    }
}

fn target_from_platform() -> Result<&'static str, String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("linux-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("macos-aarch64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("windows-x86_64")
    } else {
        Err(format!(
            "unsupported platform/architecture: {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    }
}

pub fn asset_name_for_target(target: &str) -> Result<String, String> {
    match target {
        "linux-x86_64" => Ok("mhc-linux-x86_64.tar.gz".to_owned()),
        "macos-aarch64" => Ok("mhc-macos-aarch64.tar.gz".to_owned()),
        "windows-x86_64" => Ok("mhc-windows-x86_64.zip".to_owned()),
        other => Err(format!("unsupported target for asset lookup: {other}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub revision: u64,
    pub prerelease: Option<String>,
}

pub fn parse_version(input: &str) -> Result<ParsedVersion, String> {
    let clean = input.trim();
    if clean.is_empty() {
        return Err("version string is empty".to_string());
    }
    let s = clean.strip_prefix(['v', 'V']).unwrap_or(clean);
    if s.is_empty() {
        return Err(format!("invalid version string '{input}': only prefix 'v' provided"));
    }

    static CALVER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:[.-](\d+))?(?:-([a-zA-Z0-9_.-]+))?$")
            .expect("calver regex must compile")
    });

    if let Some(caps) = CALVER_RE.captures(s) {
        let year: u64 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .ok_or_else(|| format!("invalid year in '{input}'"))?;
        let month: u64 = caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .ok_or_else(|| format!("invalid month in '{input}'"))?;
        let day: u64 = caps
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .ok_or_else(|| format!("invalid day in '{input}'"))?;
        let revision: u64 = caps
            .get(4)
            .map(|m| m.as_str().parse::<u64>())
            .transpose()
            .map_err(|error| format!("invalid revision in '{input}': {error}"))?
            .unwrap_or(0);
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return Err(format!("invalid month in '{input}'")),
        };
        if day == 0 || day > days {
            return Err(format!("invalid day in '{input}'"));
        }
        let prerelease = caps.get(5).map(|m| m.as_str().to_string());
        return Ok(ParsedVersion {
            major: year,
            minor: month,
            patch: day,
            revision,
            prerelease,
        });
    }

    if let Ok(sem) = semver::Version::parse(s) {
        return Ok(ParsedVersion {
            major: sem.major,
            minor: sem.minor,
            patch: sem.patch,
            revision: 0,
            prerelease: if sem.pre.is_empty() {
                None
            } else {
                Some(sem.pre.to_string())
            },
        });
    }

    Err(format!("invalid version string: '{input}'"))
}

fn compare_prereleases(a: &str, b: &str) -> Ordering {
    if let (Ok(pa), Ok(pb)) = (semver::Prerelease::new(a), semver::Prerelease::new(b)) {
        return pa.cmp(&pb);
    }
    let parts_a: Vec<&str> = a.split('.').collect();
    let parts_b: Vec<&str> = b.split('.').collect();
    for (seg_a, seg_b) in parts_a.iter().zip(parts_b.iter()) {
        let ord = match (seg_a.parse::<u64>(), seg_b.parse::<u64>()) {
            (Ok(num_a), Ok(num_b)) => num_a.cmp(&num_b),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => seg_a.cmp(seg_b),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    parts_a.len().cmp(&parts_b.len())
}

pub fn compare_versions(a: &str, b: &str) -> Result<Ordering, String> {
    let parsed_a = parse_version(a)?;
    let parsed_b = parse_version(b)?;

    let major_cmp = parsed_a.major.cmp(&parsed_b.major);
    if major_cmp != Ordering::Equal {
        return Ok(major_cmp);
    }
    let minor_cmp = parsed_a.minor.cmp(&parsed_b.minor);
    if minor_cmp != Ordering::Equal {
        return Ok(minor_cmp);
    }
    let patch_cmp = parsed_a.patch.cmp(&parsed_b.patch);
    if patch_cmp != Ordering::Equal {
        return Ok(patch_cmp);
    }
    let rev_cmp = parsed_a.revision.cmp(&parsed_b.revision);
    if rev_cmp != Ordering::Equal {
        return Ok(rev_cmp);
    }

    match (&parsed_a.prerelease, &parsed_b.prerelease) {
        (None, None) => Ok(Ordering::Equal),
        (Some(_), None) => Ok(Ordering::Less),
        (None, Some(_)) => Ok(Ordering::Greater),
        (Some(pre_a), Some(pre_b)) => Ok(compare_prereleases(pre_a, pre_b)),
    }
}

pub fn is_newer_version(candidate: &str, current: &str) -> Result<bool, String> {
    compare_versions(candidate, current).map(|ord| ord == Ordering::Greater)
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct GitHubReleaseResponse {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost") || d.ends_with(".localhost"),
        None => false,
    }
}

fn validate_release_asset_url(
    raw_url: &str,
    _is_custom_endpoint: bool,
    is_loopback: bool,
) -> Result<(), String> {
    let parsed = url::Url::parse(raw_url).map_err(|e| format!("invalid URL '{raw_url}': {e}"))?;

    if is_loopback {
        if !is_loopback_host(&parsed) {
            return Err(format!("loopback QA endpoint provided non-loopback asset URL: {raw_url}"));
        }
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(format!("unsupported URL scheme in loopback asset: {}", parsed.scheme()));
        }
        return Ok(());
    }

    if parsed.scheme() != "https" {
        return Err(format!("release asset URL must use https scheme: {raw_url}"));
    }
    let host = parsed.host_str().unwrap_or("");
    let is_github_host = host == "github.com"
        || host == "api.github.com"
        || host == "objects.githubusercontent.com"
        || host == "raw.githubusercontent.com"
        || host.ends_with(".githubusercontent.com");
    if !is_github_host {
        return Err(format!("release asset URL host '{host}' is not a trusted GitHub release origin"));
    }

    let path = parsed.path();
    if host != "github.com" || !path.starts_with("/Indosaram/mhc/releases/download/") {
        return Err(format!("release asset URL path '{path}' does not originate from default GitHub repository Indosaram/mhc"));
    }

    Ok(())
}

pub async fn latest() -> Result<Release, String> {
    let endpoint_override = std::env::var("MHC_UPDATE_RELEASE_URL").ok();
    latest_from_endpoint(endpoint_override.as_deref(), None).await
}

pub async fn latest_from_endpoint(
    endpoint_override: Option<&str>,
    target_override: Option<&str>,
) -> Result<Release, String> {
    let (endpoint, is_custom, is_loopback) = match endpoint_override.filter(|s| !s.trim().is_empty()) {
        Some(raw) => {
            let parsed = url::Url::parse(raw).map_err(|e| format!("invalid MHC_UPDATE_RELEASE_URL '{raw}': {e}"))?;
            let loopback = is_loopback_host(&parsed);
            match parsed.scheme() {
                "http" => {
                    if !loopback {
                        return Err(format!(
                            "HTTP is only permitted for loopback in MHC_UPDATE_RELEASE_URL (got: '{raw}')"
                        ));
                    }
                    (raw.to_string(), true, true)
                }
                "https" => (raw.to_string(), true, loopback),
                other => {
                    return Err(format!(
                        "MHC_UPDATE_RELEASE_URL scheme must be http (loopback only) or https (got: '{other}')"
                    ));
                }
            }
        }
        None => (
            "https://api.github.com/repos/Indosaram/mhc/releases/latest".to_string(),
            false,
            false,
        ),
    };

    let current_target = match target_override {
        Some(t) => target_from_str(t)?,
        None => target()?,
    };

    let expected_asset_name = asset_name_for_target(current_target)?;
    let expected_checksum_name = format!("{expected_asset_name}.sha256");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let mut response = client
        .get(&endpoint)
        .header("User-Agent", format!("mhc/{}", current_version()))
        .header("Accept", "application/vnd.github+json, application/json")
        .send()
        .await
        .map_err(|e| format!("release lookup request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("release lookup returned HTTP status {}", response.status()));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("failed to read release response body: {e}"))? {
        if body.len() + chunk.len() > 1024 * 1024 {
            return Err("release metadata exceeds size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    let gh_release: GitHubReleaseResponse = serde_json::from_slice(&body)
        .map_err(|e| format!("failed to parse release metadata: {e}"))?;

    if gh_release.draft {
        return Err(format!("release '{}' is marked as draft", gh_release.tag_name));
    }
    if gh_release.prerelease {
        return Err(format!("release '{}' is marked as prerelease", gh_release.tag_name));
    }

    let tag = gh_release.tag_name.trim();
    if tag.is_empty() {
        return Err("release metadata contains empty tag_name".to_string());
    }
    parse_version(tag).map_err(|e| format!("release has invalid tag '{tag}': {e}"))?;
    let clean_version = tag.strip_prefix(['v', 'V']).unwrap_or(tag).to_string();

    let mut found_asset: Option<&GitHubAsset> = None;
    let mut found_checksum: Option<&GitHubAsset> = None;
    for asset in &gh_release.assets {
        if asset.name == expected_asset_name {
            found_asset = Some(asset);
        }
        if asset.name == expected_checksum_name {
            found_checksum = Some(asset);
        }
    }

    let asset = found_asset.ok_or_else(|| {
        format!("release '{tag}' is missing asset for target '{current_target}': {expected_asset_name}")
    })?;
    let checksum = found_checksum.ok_or_else(|| {
        format!("release '{tag}' is missing checksum asset for target '{current_target}': {expected_checksum_name}")
    })?;

    let asset_url = asset
        .browser_download_url
        .as_deref()
        .or(asset.url.as_deref())
        .ok_or_else(|| format!("asset '{expected_asset_name}' is missing download URL"))?;

    let checksum_url = checksum
        .browser_download_url
        .as_deref()
        .or(checksum.url.as_deref())
        .ok_or_else(|| format!("checksum asset '{expected_checksum_name}' is missing download URL"))?;

    validate_release_asset_url(asset_url, is_custom, is_loopback)?;
    validate_release_asset_url(checksum_url, is_custom, is_loopback)?;

    Ok(Release {
        version: clean_version,
        tag: tag.to_string(),
        asset_url: asset_url.to_string(),
        checksum_url: checksum_url.to_string(),
        asset_name: expected_asset_name,
    })
}

pub fn version_cache_path() -> PathBuf {
    Path::new(&maho_core::config::get_agent_dir())
        .join("cache")
        .join("mhc-latest-release.json")
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CacheRecord {
    schema_version: u32,
    endpoint: String,
    target: String,
    checked_at: u64,
    success: bool,
    release: Option<Release>,
    failure_reason: Option<String>,
}

const CACHE_SCHEMA_VERSION: u32 = 1;
const SUCCESS_CACHE_TTL_SECS: u64 = 86_400;
const FAILURE_CACHE_TTL_SECS: u64 = 300;

fn write_cache_atomic(path: &Path, record: &CacheRecord) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "missing cache parent dir".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("failed to create cache directory: {e}"))?;

    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| format!("failed to serialize cache record: {e}"))?;

    use std::io::Write;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("failed to create temporary cache: {e}"))?;
    temporary.write_all(&bytes).map_err(|e| format!("failed to write temporary cache: {e}"))?;
    temporary.as_file().sync_all().map_err(|e| format!("failed to sync cache: {e}"))?;
    temporary.persist(path).map_err(|e| format!("failed to atomically replace cache: {e}"))?;
    Ok(())
}

pub async fn available_cached() -> Option<Release> {
    available_cached_with(&version_cache_path(), None, None, None).await
}

pub async fn available_cached_with(
    cache_path: &Path,
    endpoint_override: Option<&str>,
    target_override: Option<&str>,
    offline_override: Option<bool>,
) -> Option<Release> {
    let env = maho_core::config::current_env();
    if maho_core::brand::env_value("SKIP_VERSION_CHECK", &env).is_some_and(|v| !v.is_empty()) {
        return None;
    }

    let is_offline = match offline_override {
        Some(off) => off,
        None => maho_core::brand::env_value("OFFLINE", &env).is_some_and(|v| !v.is_empty()),
    };

    let endpoint = endpoint_override
        .map(str::to_string)
        .or_else(|| std::env::var("MHC_UPDATE_RELEASE_URL").ok())
        .unwrap_or_else(|| "https://api.github.com/repos/Indosaram/mhc/releases/latest".to_string());

    let current_target = match target_override {
        Some(t) => t.to_string(),
        None => target().unwrap_or("unknown").to_string(),
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if let Ok(bytes) = std::fs::read(cache_path) {
        if let Ok(record) = serde_json::from_slice::<CacheRecord>(&bytes) {
            let key_match = record.schema_version == CACHE_SCHEMA_VERSION
                && record.endpoint == endpoint
                && record.target == current_target;

            if key_match && record.checked_at <= now {
                let age = now.saturating_sub(record.checked_at);
                if record.success {
                    if age < SUCCESS_CACHE_TTL_SECS {
                        if let Some(rel) = record.release {
                            if compare_versions(&rel.version, current_version()) == Ok(Ordering::Greater) {
                                return Some(rel);
                            } else {
                                return None;
                            }
                        }
                    }
                } else if age < FAILURE_CACHE_TTL_SECS {
                    return None;
                }
            }
        }
    }

    if is_offline {
        return None;
    }

    let fetch_future = latest_from_endpoint(Some(&endpoint), Some(&current_target));
    let timeout_duration = std::time::Duration::from_secs(5);
    let result = tokio::time::timeout(timeout_duration, fetch_future).await;

    match result {
        Ok(Ok(rel)) => {
            let record = CacheRecord {
                schema_version: CACHE_SCHEMA_VERSION,
                endpoint,
                target: current_target,
                checked_at: now,
                success: true,
                release: Some(rel.clone()),
                failure_reason: None,
            };
            let _ = write_cache_atomic(cache_path, &record);
            if compare_versions(&rel.version, current_version()) == Ok(Ordering::Greater) {
                Some(rel)
            } else {
                None
            }
        }
        Ok(Err(err)) => {
            let record = CacheRecord {
                schema_version: CACHE_SCHEMA_VERSION,
                endpoint,
                target: current_target,
                checked_at: now,
                success: false,
                release: None,
                failure_reason: Some(err),
            };
            let _ = write_cache_atomic(cache_path, &record);
            None
        }
        Err(_) => {
            let record = CacheRecord {
                schema_version: CACHE_SCHEMA_VERSION,
                endpoint,
                target: current_target,
                checked_at: now,
                success: false,
                release: None,
                failure_reason: Some("release check timed out after 5s".to_string()),
            };
            let _ = write_cache_atomic(cache_path, &record);
            None
        }
    }
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod release_tests;
