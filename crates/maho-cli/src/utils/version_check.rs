use std::sync::LazyLock;
pub struct VersionCheckCause<'a> {
    pub code: Option<&'a str>,
    pub message: Option<&'a str>,
}
pub fn format_version_check_error(root_message: &str, causes: &[VersionCheckCause<'_>]) -> String {
    let mut codes = Vec::new();
    for cause in causes {
        if let Some(code) = cause.code
            && !codes.contains(&code)
        {
            codes.push(code);
        }
    }
    if !codes.is_empty() {
        return format!("{root_message} ({})", codes.join(", "));
    }
    if let Some(message) = causes
        .iter()
        .find_map(|cause| cause.message.filter(|message| !message.is_empty()))
    {
        return format!("{root_message} (cause: {message})");
    }
    root_message.to_owned()
}
pub fn read_available_version(
    payload: &serde_json::Value,
    dist_tag: Option<&str>,
) -> Option<String> {
    payload
        .as_object()?
        .get(dist_tag.unwrap_or("version"))?
        .as_str()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
}
pub fn compare_package_versions(left: &str, right: &str) -> Option<i8> {
    static CALVER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:-([2-9]\d*))?$").expect("calver regex")
    });
    let left = left.trim();
    let right = right.trim();
    let ordering =
        if let (Some(left), Some(right)) = (CALVER.captures(left), CALVER.captures(right)) {
            let parts = |captures: regex::Captures<'_>| -> Option<Vec<u64>> {
                (1..=4)
                    .map(|index| {
                        captures
                            .get(index)
                            .map_or(Ok(1), |value| value.as_str().parse())
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            };
            parts(left)?.cmp(&parts(right)?)
        } else {
            semver::Version::parse(left.strip_prefix('v').unwrap_or(left))
                .ok()?
                .cmp(&semver::Version::parse(right.strip_prefix('v').unwrap_or(right)).ok()?)
        };
    Some(match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}
pub fn is_newer_package_version(candidate: &str, current: &str) -> bool {
    compare_package_versions(candidate, current).is_some_and(|ordering| ordering > 0)
}
pub fn get_release_changelog_url(version: &str, channel_url: Option<&str>) -> String {
    let version = version.trim();
    if let Some(url) = channel_url.filter(|url| !url.is_empty()) {
        return url.replacen("{version}", version, 1);
    }
    let tag = if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    };
    format!("https://github.com/code-yeongyu/senpi/blob/{tag}/packages/coding-agent/CHANGELOG.md")
}
#[derive(Debug, PartialEq, Eq)]
pub struct LatestRelease {
    pub version: String,
    pub package_name: Option<String>,
    pub note: Option<String>,
}
#[derive(Default)]
pub struct VersionCheckOptions {
    pub timeout_ms: Option<u64>,
    pub retry: bool,
}
/// One registry request of the version check (senpi `fetch(url, { headers })`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionFetchRequest {
    pub url: String,
    pub user_agent: String,
    pub accept: String,
}
pub struct VersionFetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
/// The HTTP seam under `getLatestPiRelease` (upstream tests stub the global `fetch`): one attempt per
/// call, the bounded retry runs above it.
pub trait VersionFetch: Send + Sync {
    fn fetch(
        &self,
        request: VersionFetchRequest,
    ) -> futures::future::BoxFuture<'_, Result<VersionFetchResponse, String>>;
}
/// Production fetch: one reqwest attempt.
pub struct ReqwestVersionFetch;
impl VersionFetch for ReqwestVersionFetch {
    fn fetch(
        &self,
        request: VersionFetchRequest,
    ) -> futures::future::BoxFuture<'_, Result<VersionFetchResponse, String>> {
        Box::pin(async move {
            let response = reqwest::Client::new()
                .get(&request.url)
                .header("User-Agent", request.user_agent)
                .header("accept", request.accept)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|error| error.to_string())?
                .to_vec();
            Ok(VersionFetchResponse { status, body })
        })
    }
}
/// What the version check reads from its surroundings: the HTTP seam, the brand's update channel and
/// the environment (`envValue`).
pub struct VersionCheckIo<'a> {
    pub fetch: &'a dyn VersionFetch,
    pub channel: Option<maho_core::brand::BrandUpdateChannel>,
    pub env: std::collections::HashMap<String, String>,
}
fn process_io() -> VersionCheckIo<'static> {
    VersionCheckIo {
        fetch: &ReqwestVersionFetch,
        channel: maho_core::brand::brand_profile().update,
        env: maho_core::config::current_env(),
    }
}
pub async fn get_latest_release(
    current_version: &str,
    options: VersionCheckOptions,
) -> Result<Option<LatestRelease>, String> {
    get_latest_release_with(&process_io(), current_version, options).await
}
pub async fn get_latest_release_with(
    io: &VersionCheckIo<'_>,
    current_version: &str,
    options: VersionCheckOptions,
) -> Result<Option<LatestRelease>, String> {
    if maho_core::brand::env_value("OFFLINE", &io.env).is_some_and(|value| !value.is_empty()) {
        return Ok(None);
    }
    let Some(channel) = io.channel.clone() else {
        return Ok(None);
    };
    let encoded: String = channel
        .package_name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    let request = VersionFetchRequest {
        url: format!("https://registry.npmjs.org/-/package/{encoded}/dist-tags"),
        user_agent: super::pi_user_agent::get_pi_user_agent(current_version),
        accept: "application/json".to_owned(),
    };
    let response = super::management_http::retry_attempts(
        None,
        super::management_http::FetchRetryOptions {
            max_retries: if options.retry { 2 } else { 0 },
            timeout_ms: Some(options.timeout_ms.unwrap_or(10000)),
            ..Default::default()
        },
        |response: &VersionFetchResponse| response.status,
        || io.fetch.fetch(request.clone()),
    )
    .await?;
    if !(200..300).contains(&response.status) {
        return Ok(None);
    }
    let payload: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
    Ok(
        read_available_version(&payload, Some(&channel.dist_tag)).map(|version| LatestRelease {
            version,
            package_name: Some(channel.package_name),
            note: None,
        }),
    )
}
pub async fn get_latest_version(
    current_version: &str,
    options: VersionCheckOptions,
) -> Result<Option<String>, String> {
    get_latest_version_with(&process_io(), current_version, options).await
}
pub async fn get_latest_version_with(
    io: &VersionCheckIo<'_>,
    current_version: &str,
    options: VersionCheckOptions,
) -> Result<Option<String>, String> {
    Ok(get_latest_release_with(io, current_version, options)
        .await?
        .map(|release| release.version))
}
pub async fn check_for_new_version(current_version: &str) -> Option<LatestRelease> {
    check_for_new_version_with(&process_io(), current_version).await
}
pub async fn check_for_new_version_with(
    io: &VersionCheckIo<'_>,
    current_version: &str,
) -> Option<LatestRelease> {
    if maho_core::brand::env_value("SKIP_VERSION_CHECK", &io.env)
        .is_some_and(|value| !value.is_empty())
    {
        return None;
    }
    get_latest_release_with(io, current_version, Default::default())
        .await
        .ok()
        .flatten()
        .filter(|release| is_newer_package_version(&release.version, current_version))
}
