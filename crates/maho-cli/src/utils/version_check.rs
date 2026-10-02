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
