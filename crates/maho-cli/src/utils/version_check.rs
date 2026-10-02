use std::sync::LazyLock;
pub fn read_available_version(payload: &serde_json::Value, dist_tag: Option<&str>) -> Option<String> {
    payload.as_object()?.get(dist_tag.unwrap_or("version"))?.as_str().map(str::trim).filter(|version| !version.is_empty()).map(str::to_owned)
}
pub fn compare_package_versions(left: &str, right: &str) -> Option<i8> {
    static CALVER: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:-([2-9]\d*))?$").expect("calver regex"));
    let left = left.trim();
    let right = right.trim();
    let ordering = if let (Some(left), Some(right)) = (CALVER.captures(left), CALVER.captures(right)) {
        let parts = |captures: regex::Captures<'_>| -> Option<Vec<u64>> { (1..=4).map(|index| captures.get(index).map_or(Ok(1), |value| value.as_str().parse())).collect::<Result<Vec<_>, _>>().ok() };
        parts(left)?.cmp(&parts(right)?)
    } else {
        semver::Version::parse(left.strip_prefix('v').unwrap_or(left)).ok()?.cmp(&semver::Version::parse(right.strip_prefix('v').unwrap_or(right)).ok()?)
    };
    Some(match ordering { std::cmp::Ordering::Less => -1, std::cmp::Ordering::Equal => 0, std::cmp::Ordering::Greater => 1 })
}
pub fn is_newer_package_version(candidate: &str, current: &str) -> bool { compare_package_versions(candidate, current).is_some_and(|ordering| ordering > 0) }
pub fn get_release_changelog_url(version: &str, channel_url: Option<&str>) -> String {
    let version = version.trim();
    if let Some(url) = channel_url.filter(|url| !url.is_empty()) { return url.replacen("{version}", version, 1); }
    let tag = if version.starts_with('v') { version.to_owned() } else { format!("v{version}") };
    format!("https://github.com/code-yeongyu/senpi/blob/{tag}/packages/coding-agent/CHANGELOG.md")
}
#[derive(Debug, PartialEq, Eq)]
pub struct LatestRelease { pub version: String, pub package_name: Option<String>, pub note: Option<String> }
#[derive(Default)]
pub struct VersionCheckOptions { pub timeout_ms: Option<u64>, pub retry: bool }
pub async fn get_latest_release(current_version: &str, options: VersionCheckOptions) -> Result<Option<LatestRelease>, String> {
    if maho_core::brand::env_value("OFFLINE", &maho_core::config::current_env()).is_some_and(|value| !value.is_empty()) { return Ok(None); }
    let brand = maho_core::brand::brand_profile();
    let Some(channel) = brand.update else { return Ok(None); };
    let encoded: String = channel.package_name.bytes().map(|byte| {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) { (byte as char).to_string() } else { format!("%{byte:02X}") }
    }).collect();
    let client = reqwest::Client::new();
    let request = client.get(format!("https://registry.npmjs.org/-/package/{encoded}/dist-tags"))
        .header("User-Agent", super::pi_user_agent::get_pi_user_agent(current_version)).header("accept", "application/json").build().map_err(|error| error.to_string())?;
    let response = super::management_http::fetch_with_retry(&client, request, None, super::management_http::FetchRetryOptions {
        max_retries: if options.retry { 2 } else { 0 }, timeout_ms: Some(options.timeout_ms.unwrap_or(10000)), ..Default::default()
    }).await?;
    if !response.status().is_success() { return Ok(None); }
    let payload = response.json().await.map_err(|error| error.to_string())?;
    Ok(read_available_version(&payload, Some(&channel.dist_tag)).map(|version| LatestRelease { version, package_name: Some(channel.package_name), note: None }))
}
pub async fn get_latest_version(current_version: &str, options: VersionCheckOptions) -> Result<Option<String>, String> {
    Ok(get_latest_release(current_version, options).await?.map(|release| release.version))
}
pub async fn check_for_new_version(current_version: &str) -> Option<LatestRelease> {
    if maho_core::brand::env_value("SKIP_VERSION_CHECK", &maho_core::config::current_env()).is_some_and(|value| !value.is_empty()) { return None; }
    get_latest_release(current_version, Default::default()).await.ok().flatten().filter(|release| is_newer_package_version(&release.version, current_version))
}
